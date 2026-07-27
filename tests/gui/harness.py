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
executed there and drives `/var/tmp/broadway-launch.sh` locally.
"""

from __future__ import annotations

import json
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

BROADWAY_URL = "http://localhost:8085"
LAUNCHER = "/var/tmp/broadway-launch.sh"
# Must match broadway-launch.sh. $HOME is shared host<->toolbox; /var/tmp is not.
JOURNAL_PATH = str(Path.home() / ".finupdate-test-journal.jsonl")
APP_LOG = "/var/tmp/finupdate-broadway.log"
REMOTE_SHOTS = "/var/tmp/gui-shots"

SCREENSHOT_DIR = Path(__file__).resolve().parent / "screenshots"

# Default capture geometry. The window is 750x700 by default; the viewport is
# larger so the whole window including its CSD shadow is in frame.
VIEWPORT = (1000, 800)

_PW = None
_BROWSER = None


# ── Widget coordinate map ────────────────────────────────────────────────────
#
# (x, y) in viewport pixels at VIEWPORT size with the default window geometry.
# `source` names the capture each was read from so they can be re-derived.
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
        sh(f"{' '.join(env)} {LAUNCHER} >/dev/null 2>&1")
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

    def app_log(self) -> str:
        return sh(f"cat {APP_LOG} 2>/dev/null", check=False).stdout

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
