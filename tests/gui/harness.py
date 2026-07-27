#!/usr/bin/env python3
"""
Broadway-backed GUI harness for finupdate.

Replaces the dogtail/behave suite in `tests/smoke/`, which could not run here:
it needs a live GNOME session plus `gnome-ponytail-daemon`, neither of which
exists on a build host (see docs/GUI_TESTING.md).

Design, and why it is shaped this way
-------------------------------------

Ported from tuna-os/gtk-office-suite's `broadway-inspect` skill, but with one
finding that changes the approach. The office-suite scripts scrape the Broadway
DOM for text and click with `get_by_text()`. **That cannot work for GTK4.**
Measured against this app: Broadway emits 133 `<div>`s and 19 `<img>`s and
*zero* text nodes — GTK4 rasterises text into textures rather than DOM text. So
there is nothing to match on by label.

What *does* work, verified on himachal:

* **Rendering** is pixel-accurate. Screenshots are the strong half.
* **Input** is forwarded. A `page.mouse.click()` at the "Check" button's
  coordinates opened the update-check dialog, and `AdwDialog` renders correctly
  under Broadway. Keyboard events arrive too, so Tab/Enter traversal works.
* **Backend intent** is observable through the JSONL action journal written by
  `src/action_journal.rs`.

So each feature check is a triple:

    1. drive    — click a coordinate, or Tab/Enter to the control
    2. look     — capture a PNG
    3. assert   — the journal recorded the right command, with the right argv

Point 3 is the one a screenshot can never give you. It is the difference
between "the rebase dialog looks right" and "clicking Switch would really have
run `bootc switch ghcr.io/ublue-os/bluefin:stable`".

Coordinates
-----------

Because labels aren't selectable, targets are coordinates in a **fixed-size
window**. That is only stable if rendering is deterministic, so the launcher
pins: window geometry (`FINUPDATE_WINDOW_SIZE`), `GTK_ENABLE_ANIMATIONS=0`,
an isolated `XDG_CONFIG_HOME`, and a fixed mock image. Coordinates live in
`WIDGETS` below, each with the capture they were read from, so they can be
re-derived when the layout changes.

Prefer `activate_by_keyboard()` where the tab order is stable — it survives
layout shifts that would break a coordinate.

Everything runs on himachal (the local VPS cannot build GTK4). This module is
executed there and drives `broadway-launch.sh`, its sibling in this directory.
"""

from __future__ import annotations

import json
import re
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

BROADWAY_URL = "http://localhost:8085"
# The tracked script, not a copy of it. This used to point at
# /var/tmp/broadway-launch.sh, which had to be refreshed by hand — so editing
# the version under git changed nothing, and the suite kept exercising whatever
# had last been copied out. Silent staleness in a test harness is worse than a
# crash: the run goes green against code that isn't the code under review.
LAUNCHER = str(Path(__file__).resolve().parent / "broadway-launch.sh")
# Must match broadway-launch.sh. $HOME is shared host<->toolbox; /var/tmp is not.
JOURNAL_PATH = str(Path.home() / ".finupdate-test-journal.jsonl")
APP_LOG = "/var/tmp/finupdate-broadway.log"
REMOTE_SHOTS = "/var/tmp/gui-shots"

SCREENSHOT_DIR = Path(__file__).resolve().parent / "screenshots"

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")

# Default capture geometry. The window is 750x700 by default; the viewport is
# larger so the whole window including its CSD shadow is in frame.
VIEWPORT = (1000, 800)

_PW = None
_BROWSER = None


# ── Widget coordinate map ────────────────────────────────────────────────────
#
# (x, y) in viewport pixels at VIEWPORT size with the default window geometry.
# `source` names the capture each was read from so they can be re-derived.
#
# MOUSE CLICKS ONLY WORK ON THE MAIN WINDOW. Pointer events do not reach
# AdwDialog content under Broadway — the click lands on nothing and the app
# carries on as if it never happened. Verified by clicking the "Include App
# Updates" switch inside the Advanced dialog (it does not toggle) and the
# Cancel button in the update-check dialog (the run continues); both are
# ordinary widgets, so this is Broadway pointer routing, not an app bug.
# Assumed not to affect a real Wayland session, which this suite cannot check.
#
# Drive dialogs with the keyboard instead — see MNEMONICS below. Keyboard
# events reach dialog content fine.
#
# This mattered because it was invisible: a click that misses raises nothing,
# so any check asserting only `assert_no_panics()` passed identically whether
# it drove the UI or not. Use `interact()` for anything that should change the
# screen.
WIDGETS: dict[str, tuple[int, int]] = {
    # from screenshots/light/idle.png
    "check_button": (628, 242),
    "hero_change": (585, 164),
    "automatic_updates_switch": (643, 297),
    "advanced_row": (399, 375),
    "main_menu": (638, 47),
    # from screenshots/light/check-dialog.png
    "check_dialog_cancel": (546, 563),
    "check_dialog_close": (587, 172),
}

# Mnemonics for widgets inside dialogs, where the mouse cannot reach.
#
# More robust than the tab counts the coordinate map's siblings rely on: a
# mnemonic is bound to the widget's own label, so it survives rows being
# reordered or inserted. These come from the access keys in preferences.rs
# ("Image _Source", "What's _New", ...), which exist for HIG conformance
# anyway — the suite just gets to reuse them.
MNEMONICS: dict[str, str] = {
    # Advanced dialog → Image group. Each closes the dialog and navigates the
    # main window's AdwNavigationView to the named page.
    "image_source_row": "Alt+s",
    "image_history_row": "Alt+h",
    "whats_new_row": "Alt+n",
    # Rebase dialog. The primary button's label is dynamic ("Switch to
    # :latest" / "Pin to 20260506"), so its access key moves with the label —
    # with_access_key() in rebase_dialog.rs marks the first character.
    "rebase_primary_switch": "Alt+s",
    "rebase_primary_pin": "Alt+p",
    # The AlertDialog that confirms a switch: responses "_Cancel"/"_Switch".
    "confirm_switch": "Alt+s",
    "confirm_cancel": "Alt+c",
}


# Kills every finupdate/broadwayd instance. The bracket trick — [t]arget —
# stops the pattern from matching the very shell that is running it: `pkill -f`
# compares against full command lines, and without this the shell SIGKILLs
# itself mid-script, which stalled the whole suite after the first check.
KILL_ALL = (
    'pkill -9 -f "[t]arget/debug/finupdate" 2>/dev/null; '
    'pkill -9 -f "[g]tk4-broadwayd" 2>/dev/null; true'
)


def _browser():
    """One chromium instance shared by every check in the process.

    Playwright's browser launch is expensive (~2s) and its teardown can hang, so
    a browser per check made the suite both slow and prone to stalling. Each
    check gets a fresh *page* instead, which is enough isolation: the app itself
    is restarted between checks, so no UI state carries over.
    """
    global _PW, _BROWSER
    if _BROWSER is None:
        from playwright.sync_api import sync_playwright

        _PW = sync_playwright().start()
        _BROWSER = _PW.chromium.launch(
            headless=True, args=["--no-sandbox", "--use-gl=swiftshader"]
        )
    return _BROWSER


def shutdown_browser():
    """Tear down the shared browser. Call once, at the end of the run."""
    global _PW, _BROWSER
    if _BROWSER is not None:
        try:
            _BROWSER.close()
        except Exception:
            pass
        _BROWSER = None
    if _PW is not None:
        try:
            _PW.stop()
        except Exception:
            pass
        _PW = None


def sh(cmd: str, check: bool = True, timeout: int = 120) -> subprocess.CompletedProcess:
    """Run a shell command on the build host.

    This module executes *on* himachal — Broadway listens on its loopback, so
    the browser has to be there too. Commands are therefore local, not ssh'd.

    Always bounded by a timeout: an unbounded `subprocess.run` here turned a
    single wedged command into a suite that hung indefinitely with no output.
    """
    try:
        # `bash -c`, not `-lc`. A login shell sources /etc/profile.d/* and
        # ~/.profile, and on a host where those emit errors (a missing
        # ~/.cargo/env, a broken motd hook) podman stops detecting rootless mode
        # and fails with "creating runtime static files directory
        # /var/lib/containers/storage/libpod: permission denied". The suite then
        # reports ERR_CONNECTION_REFUSED, which points at Broadway rather than
        # at the shell. Nothing here needs a login environment.
        return subprocess.run(
            ["bash", "-c", cmd], check=check,
            capture_output=True, text=True, timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(cmd, returncode=124, stdout="", stderr="timeout")


@dataclass
class JournalEntry:
    """One recorded intent from src/action_journal.rs."""

    seq: int
    action: str
    args: dict
    would_run: list[str]
    suppressed_by: str
    ts: str = ""

    @classmethod
    def parse(cls, line: str) -> "JournalEntry":
        d = json.loads(line)
        return cls(
            seq=d["seq"], action=d["action"], args=d.get("args") or {},
            would_run=d.get("would_run") or [],
            suppressed_by=d.get("suppressed_by", "none"), ts=d.get("ts", ""),
        )


class CheckFailed(AssertionError):
    """Carries journal + log context so a failure is debuggable at a glance."""


@dataclass
class FinupdateApp:
    """A running finupdate under Broadway on the build host."""

    dev_mode: bool = False
    dry_run: bool = True
    sim: str | None = None
    window_size: str | None = None
    image: str = "ghcr.io/ublue-os/bluefin:stable"
    page: object | None = field(default=None, repr=False)

    # ── lifecycle ────────────────────────────────────────────────────────

    def __enter__(self) -> "FinupdateApp":
        self.start()
        return self

    def __exit__(self, *exc):
        self.stop()
        return False

    def _args(self) -> str:
        argv = ["--dry-run"] if self.dry_run else []
        argv.append("--dev-mode" if self.dev_mode else "--no-dev-mode")
        if self.sim:
            argv.append(f"--sim={self.sim}")
        return " ".join(argv)

    def start(self):
        env = [f'APP_ARGS="{self._args()}"', f'FIN_IMAGE={self.image}']
        if self.window_size:
            env.append(f"WSIZE={self.window_size}")
        # Keep the launcher's output rather than routing it to /dev/null: it
        # carries the compiler diagnostics when the build step fails, and
        # without them a broken build surfaces as a bare "exit status 1".
        proc = sh(f"{' '.join(env)} {LAUNCHER} 2>&1", check=False, timeout=1800)
        if proc.returncode != 0:
            raise CheckFailed(
                f"launcher failed (exit {proc.returncode}):\n"
                f"{proc.stdout[-2000:]}{proc.stderr[-2000:]}"
            )
        # The launcher already sleeps for the listener; give the first frame
        # time to paint before any capture.
        time.sleep(4)

        w, h = VIEWPORT
        self.page = _browser().new_page(viewport={"width": w, "height": h})
        self.page.goto(BROADWAY_URL, timeout=30000)
        self.page.wait_for_timeout(5000)

    def stop(self):
        # Close only the page. Tearing down the whole browser between checks
        # was both slow and unreliable — `browser.close()` could block
        # indefinitely when the page still had a modal open, which stalled the
        # suite mid-run with no output.
        if self.page:
            try:
                self.page.close()
            except Exception:
                pass
            self.page = None
        sh(KILL_ALL, check=False)

    # ── driving ──────────────────────────────────────────────────────────

    def click(self, widget: str, settle_ms: int = 2500):
        """Click a named widget from WIDGETS."""
        if widget not in WIDGETS:
            raise KeyError(f"unknown widget {widget!r}; known: {sorted(WIDGETS)}")
        x, y = WIDGETS[widget]
        self.page.mouse.click(x, y)
        self.page.wait_for_timeout(settle_ms)

    def activate(self, widget: str, settle_ms: int = 3000):
        """Activate a dialog widget by its mnemonic. See MNEMONICS."""
        if widget not in MNEMONICS:
            raise KeyError(f"no mnemonic for {widget!r}; known: {sorted(MNEMONICS)}")
        self.page.keyboard.press(MNEMONICS[widget])
        self.page.wait_for_timeout(settle_ms)

    def click_xy(self, x: int, y: int, settle_ms: int = 2500):
        self.page.mouse.click(x, y)
        self.page.wait_for_timeout(settle_ms)

    def key(self, chord: str, settle_ms: int = 600):
        self.page.keyboard.press(chord)
        self.page.wait_for_timeout(settle_ms)

    def activate_by_keyboard(self, tabs: int, settle_ms: int = 2500):
        """Tab `tabs` times from the current focus, then activate with Enter.

        Preferred over a coordinate where the tab order is stable — it survives
        layout changes that would silently move a pixel target onto the wrong
        widget (which fails as a confusing screenshot diff rather than an
        error).
        """
        for _ in range(tabs):
            self.page.keyboard.press("Tab")
            self.page.wait_for_timeout(200)
        self.page.keyboard.press("Enter")
        self.page.wait_for_timeout(settle_ms)

    # ── observing ────────────────────────────────────────────────────────

    def screenshot(self, name: str, theme: str = "light") -> Path:
        out_dir = SCREENSHOT_DIR / theme
        out_dir.mkdir(parents=True, exist_ok=True)
        path = out_dir / f"{name}.png"
        self.page.screenshot(path=str(path))
        return path

    def pixels(self) -> bytes:
        """Raw bytes of the current frame, for change detection."""
        return self.page.screenshot()

    def app_log(self) -> str:
        raw = sh(f"cat {APP_LOG} 2>/dev/null", check=False).stdout
        # tracing's ANSI colouring interleaves escape sequences *inside* field
        # renderings — `page=changelog` is emitted as
        # `\x1b[3mpage\x1b[0m\x1b[2m=\x1b[0mchangelog`. A plain substring
        # search for "page=changelog" therefore never matches, which would
        # make every assert_log on a structured field a silent false negative.
        return ANSI_RE.sub("", raw)

    def journal(self) -> list[JournalEntry]:
        out = sh(f"cat {JOURNAL_PATH} 2>/dev/null", check=False).stdout
        entries = [JournalEntry.parse(l) for l in out.splitlines() if l.strip()]
        return sorted(entries, key=lambda e: e.seq)

    def panic_count(self) -> int:
        """Panics are a hard failure even when the screenshot looks fine."""
        # `grep -c` prints its count *and* exits 1 when the count is zero, so a
        # `|| echo 0` fallback yields "0\n0". Count lines instead.
        out = sh(f"grep -c panicked {APP_LOG} 2>/dev/null; true", check=False)
        first = (out.stdout or "0").strip().splitlines()
        return int(first[0]) if first and first[0].isdigit() else 0

    # ── asserting ────────────────────────────────────────────────────────

    def assert_action(
        self,
        action: str,
        *,
        would_run_contains: list[str] | None = None,
        args_include: dict | None = None,
        suppressed: bool = True,
    ) -> JournalEntry:
        """Assert finupdate recorded the intent to perform `action`.

        The half a screenshot cannot cover: that the control is wired to the
        right backend call, with the right target.
        """
        entries = self.journal()
        matches = [e for e in entries if e.action == action]
        if not matches:
            raise CheckFailed(
                f"no journal entry for {action!r}.\n"
                f"recorded: {[e.action for e in entries] or '(nothing)'}\n"
                f"log tail:\n{self.app_log()[-1500:]}"
            )
        entry = matches[-1]

        if suppressed and entry.suppressed_by == "none":
            raise CheckFailed(
                f"{action!r} was NOT suppressed — it executed for real. "
                "dry-run failed to block a destructive command."
            )

        for tok in would_run_contains or []:
            if tok not in entry.would_run:
                raise CheckFailed(
                    f"{action!r} would_run missing {tok!r}\nactual: {entry.would_run}"
                )

        for k, v in (args_include or {}).items():
            if entry.args.get(k) != v:
                raise CheckFailed(
                    f"{action!r} arg {k!r}={entry.args.get(k)!r}, expected {v!r}\n"
                    f"full args: {entry.args}"
                )
        return entry

    def assert_no_action(self, action: str):
        hits = [e for e in self.journal() if e.action == action]
        if hits:
            raise CheckFailed(
                f"expected no {action!r} but found {len(hits)}: {hits[-1].would_run}"
            )

    def assert_no_panics(self):
        n = self.panic_count()
        if n:
            raise CheckFailed(
                f"{n} panic(s) in the app log:\n"
                + "\n".join(
                    l for l in self.app_log().splitlines() if "panicked" in l
                )[:1500]
            )

    def wait_for_log(self, needle: str, timeout_s: int = 60) -> str:
        """Block until `needle` appears in the app log, or fail.

        Needed for anything that races a network fetch. Asserting the *absence*
        of a failure marker is worthless if the work simply hasn't finished:
        the package-diff check asserted "not zero packages" and passed while
        the SBOM was still downloading, so it proved nothing and the screenshot
        caught the page mid-spinner.
        """
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            if needle in self.app_log():
                return self.app_log()
            self.page.wait_for_timeout(1000)
        raise CheckFailed(
            f"{needle!r} did not appear within {timeout_s}s\n"
            f"log tail:\n{self.app_log()[-2000:]}"
        )

    def assert_log(self, needle: str, *, absent: bool = False):
        """Assert a line is (or isn't) in the app log.

        GTK4 rasterises text into GPU textures, so under Broadway the DOM
        carries no text nodes and Playwright's text selectors are useless. A
        log line is the cheapest observable the app can offer the harness about
        its own internal state.
        """
        log = self.app_log()
        if absent and needle in log:
            raise CheckFailed(f"did not expect {needle!r} in the app log")
        if not absent and needle not in log:
            raise CheckFailed(
                f"expected {needle!r} in the app log\nlog tail:\n{log[-2000:]}"
            )

    def interact(self, fn, *, settle_ms: int = 3000, what: str = "interaction"):
        """Run `fn`, then fail if the frame is byte-identical to before it.

        The reason this exists. Every dialog-driven check in this suite once
        asserted nothing but `assert_no_panics()`, so a click that landed on
        nothing passed exactly like a click that worked — and several did land
        on nothing. `check-dialog-cancel` reported "Cancel closes the dialog"
        while the captured screenshot showed the dialog still open, mid-run.
        A no-op cannot be distinguished from success by a green exit code, only
        by a human looking at the PNG, which is not a test.

        This does not prove the *right* thing happened — pair it with
        `assert_log` or `assert_action` for that. It proves something did.
        """
        before = self.pixels()
        fn()
        self.page.wait_for_timeout(settle_ms)
        if self.pixels() == before:
            raise CheckFailed(
                f"{what} changed nothing on screen — the interaction was a "
                "no-op. A pixel-identical frame after a click means the click "
                "missed, not that the app agreed with you."
            )
