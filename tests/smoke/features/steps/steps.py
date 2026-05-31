"""
Custom step definitions for the finupdate smoke suite.

`qecore.common_steps` covers the low-level lifecycle and most UI actions; the
custom steps here cover finupdate-specific affordances:

- Activating Developer Mode + selecting a simulation scenario before triggering
  an update (so the same scenarios can drive Success / AlreadyUpToDate /
  Failure without root or a live system).
- "Activate" — a convenience that waits-for + clicks, since several buttons
  appear after async work.
- Time-bounded waits with a friendlier error message than `assert child is None`.
"""
import json
import os
import time
from pathlib import Path

from behave import step
from qecore.common_steps import *  # noqa: F401,F403


# ── Helpers ────────────────────────────────────────────────────────────────

def _settings_path() -> Path:
    """Settings live inside the Flatpak sandbox's per-app config dir."""
    # gtk::glib::user_config_dir() inside the sandbox resolves to
    # ~/.var/app/<app-id>/config — at least on Linux/Flatpak.
    base = Path.home() / ".var/app/org.projectbluefin.Finupdate.Devel/config"
    return base / "finupdate/settings.json"


def _write_settings(**overrides) -> None:
    """Pre-write settings.json before launching the app so dev_mode + scenario
    are in effect from process start. The app reads this file on construction.
    """
    path = _settings_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    current = {}
    if path.exists():
        try:
            current = json.loads(path.read_text())
        except json.JSONDecodeError:
            current = {}
    current.update(overrides)
    path.write_text(json.dumps(current, indent=2))


def _force_close_finupdate(context) -> None:
    """Tear down finupdate without relying on qecore's `close_via_shortcut`.

    Ctrl+Q is bound (app.rs:414) but qecore's shortcut path waits 30s for the
    AT-SPI process to vanish, and that wait races against the GTK main loop —
    if a live GHCR fetch is mid-flight when Quit fires, the close can stall
    long enough for qecore to assert-fail, marking the scenario broken even
    when the test itself passed. SIGTERM via `pkill -x` is reliable; the next
    `Start application "finupdate" via "command"` step creates a fresh
    process either way.
    """
    import subprocess
    subprocess.run(["pkill", "-x", "finupdate"], check=False)
    # Wait briefly for the AT-SPI tree to drop the app so the next launch
    # doesn't race against a still-disappearing handle.
    deadline = time.time() + 5
    while time.time() < deadline:
        try:
            from dogtail.tree import root
            root.application("finupdate")
        except Exception:
            break
        time.sleep(0.2)
    # Invalidate qecore's cached handle so the next launch is clean.
    if getattr(context, "finupdate", None) and hasattr(context.finupdate, "instance"):
        context.finupdate.instance = None


# ── Dev-mode scenario setup ───────────────────────────────────────────────

@step('Application "{app_id}" is in developer mode with scenario "{scenario}"')
def set_dev_scenario(context, app_id, scenario):
    """Set Settings::dev_mode = true and the simulation scenario before launch.

    The app reads ~/.var/app/<app-id>/config/finupdate/settings.json on
    startup; writing it here is the most reliable way to gate the simulator
    path without driving the Preferences dialog every scenario.
    """
    assert app_id == "finupdate", f"Unknown app_id: {app_id}"
    assert scenario in ("Success", "AlreadyUpToDate", "Failure"), \
        f"Unknown scenario: {scenario}"

    # The app must not be running when we touch settings; otherwise it may
    # overwrite our changes when closing. qecore tracks this via context.
    _force_close_finupdate(context)

    _write_settings(dev_mode=True, sim_scenario=scenario)

    # Re-launch with new settings in effect.
    context.execute_steps('\n'.join([
        '* Start application "finupdate" via "command"',
        '* Wait until window "Finupdate" appears in "finupdate"',
    ]))


# ── Custom window detection ───────────────────────────────────────────────

@step('Wait until window "{name}" appears in "{app_id}"')
def wait_for_window(context, name, app_id):
    """Wait for the application window to appear by name, regardless of role.

    GTK4 windows may report as 'filler' or other roles, not 'frame'.
    """
    app = getattr(context, app_id)
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            # Look for any widget with this name, any role
            matches = app.instance.findChildren(
                lambda n: n.name == name
            )
            if matches:
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise AssertionError(f"Window {name!r} did not appear in {app_id}")


# ── Friendlier wait-and-click ─────────────────────────────────────────────

@step('Activate "{name}" "{role}" in "{app_id}"')
def activate_widget(context, name, role, app_id):
    """Wait until the named widget appears, then click it.

    Useful for buttons that appear partway through an async flow (e.g. the
    "Install Updates" button shown only after the check completes).
    """
    context.execute_steps('\n'.join([
        f'* Wait until "{name}" "{role}" appears in "{app_id}"',
        f'* Left click "{name}" "{role}" in "{app_id}"',
    ]))


# ── Bounded waits ─────────────────────────────────────────────────────────

@step('Wait until "{text}" appears in "{app_id}" within {seconds:d} seconds')
def wait_for_text(context, text, app_id, seconds):
    """Poll the AT-SPI tree for any node whose name or description contains
    `text`. Replaces noisy `findChild` regex with a bounded retry loop.

    Useful for matching status-page strings ("Ready to install", "Update
    Complete", "Up to Date", "Update Failed") which may appear in any role.
    """
    app = getattr(context, app_id)
    deadline = time.time() + seconds
    needle = text.lower()
    while time.time() < deadline:
        try:
            matches = app.instance.findChildren(
                lambda n: needle in ((n.name or "") + (n.description or "")).lower()
            )
            if matches:
                return
        except Exception:
            pass  # AT-SPI tree may transiently not be queryable
        time.sleep(0.5)

    # Debug: dump what IS in the tree
    found_texts = []
    try:
        def collect(n):
            name = n.name or ""
            desc = n.description or ""
            if name or desc:
                found_texts.append(f"[{n.roleName}] name={name!r} desc={desc!r}")
        app.instance.findChildren(lambda n: (collect(n), False)[1])
    except Exception:
        pass
    found_summary = "\n      ".join(found_texts[:30]) if found_texts else "(nothing with name/desc found)"
    raise AssertionError(
        f"Timed out after {seconds}s waiting for {text!r} in {app_id}.\n"
        f"    Visible text nodes:\n      {found_summary}"
    )


# ── Real mode setup ───────────────────────────────────────────────────────

@step('Application "{app_id}" is in real mode (developer mode disabled)')
def disable_dev_mode(context, app_id):
    """Disable developer mode to test real update paths.

    The Devel build defaults to dev_mode=true for safety during development.
    This step disables it so we can test the real update flow with actual
    system checks (Flatpak, Homebrew, Distrobox, bootc).

    SAFETY: dry_run is forced to True alongside dev_mode=False. Otherwise a
    follow-up scenario that activates Restart / Powerwash / Factory reset
    would actually reboot or reset the host machine — disable_dev_mode is
    only ever meaningful when paired with dry_run, never on its own.
    """
    assert app_id == "finupdate", f"Unknown app_id: {app_id}"

    _force_close_finupdate(context)

    _write_settings(dev_mode=False, dry_run=True)

    context.execute_steps('\n'.join([
        '* Start application "finupdate" via "command"',
        '* Wait until window "Finupdate" appears in "finupdate"',
    ]))


# ── Mock identity for image-family matrix tests ──────────────────────────

@step('Mock identity "{full_ref}" is configured')
def configure_mock_identity(context, full_ref):
    """Pretend the system is booted on the given image ref.

    Writes settings.json with:
      - mock_identity = parsed {registry, org, image, tag}
      - dry_run = true   (block real bootc switch / reboot / timer toggle)
      - dev_mode = true  (route update worker through simulator)

    Then closes the app (if running) and relaunches so the new settings take
    effect from process start. The app honours mock_identity in:
      - RegistryClient::detect (history fetch, rebase dialog)
      - status_view::detect_bootc_image_info (image source / history / changelog)
    while still hitting real GHCR + GitHub APIs for downstream data.

    Example: "ghcr.io/ublue-os/bluefin:stable"
    """
    # Parse: registry/org/image:tag
    if "/" not in full_ref or ":" not in full_ref:
        raise AssertionError(f"Invalid full_ref {full_ref!r}: expected registry/org/image:tag")
    registry, rest = full_ref.split("/", 1)
    org, rest = rest.split("/", 1)
    image, tag = rest.rsplit(":", 1)

    _force_close_finupdate(context)

    _write_settings(
        dev_mode=True,
        dry_run=True,
        mock_identity={
            "registry": registry,
            "org": org,
            "image": image,
            "tag": tag,
        },
    )

    context.execute_steps('\n'.join([
        '* Start application "finupdate" via "command"',
        '* Wait until window "Finupdate" appears in "finupdate"',
    ]))


# ── Image history strict-count assertion ───────────────────────────────────

@step('Image history shows at least {n:d} entries in "{app_id}" within {seconds:d} seconds')
def assert_history_count(context, n, app_id, seconds):
    """Assert the home-page "N images" label has reached at least n.

    Polls the AT-SPI tree for labels matching `\\d+ images$` and checks that
    the max observed count is >= n. The history list grows asynchronously as
    the live GHCR fetch returns, so we time-bound the wait.
    """
    import re
    app = getattr(context, app_id)
    deadline = time.time() + seconds
    count_re = re.compile(r"^(\d+)\s*images?$")
    observed_max = 0

    while time.time() < deadline:
        try:
            matches = app.instance.findChildren(
                lambda node: node.roleName in ("label", "list item")
                and count_re.match(node.name or "") is not None
            )
            for m in matches:
                hit = count_re.match(m.name or "")
                if hit:
                    val = int(hit.group(1))
                    if val > observed_max:
                        observed_max = val
            if observed_max >= n:
                return
        except Exception:
            pass
        time.sleep(0.5)

    # Diagnostic: dump any image-history-related labels we saw.
    seen_history_labels = []
    try:
        def collect(node):
            name = node.name or ""
            if "image" in name.lower() or "version" in name.lower() or count_re.match(name):
                seen_history_labels.append(f"[{node.roleName}] {name!r}")
            return False
        app.instance.findChildren(collect)
    except Exception:
        pass
    summary = "\n      ".join(seen_history_labels[:20]) if seen_history_labels else "(none)"
    raise AssertionError(
        f"Image history reached only {observed_max} entries in {app_id} "
        f"(wanted ≥ {n} within {seconds}s).\n    Labels seen:\n      {summary}"
    )


# ── Rebase calendar helpers ───────────────────────────────────────────────
# The rebase dialog renders a 7×6 calendar of integer-labeled buttons. Days
# with a published image are sensitive; other days (no version, or future) are
# insensitive. Tests can't predict which date will be available because that
# depends on what GHCR returns at runtime, so we provide a step that finds the
# first sensitive day button and clicks it.

@step('Click any available day in the rebase calendar')
def click_any_available_day(context):
    """Click the first sensitive day button in the rebase dialog's calendar.

    Day buttons are gtk::Button with integer labels "1".."31" and the CSS
    class `day-btn`. Available days are sensitive; unavailable / future days
    are insensitive. We scan the AT-SPI tree for push buttons with an integer
    name in [1, 31] that are currently sensitive, and click the first one.

    Fails noisily with a diagnostic dump of what calendar buttons were visible
    so a flaky run is debuggable.
    """
    app = context.finupdate.instance

    def is_sensitive_day(node):
        if node.roleName != "push button":
            return False
        name = (node.name or "").strip()
        if not name.isdigit():
            return False
        n = int(name)
        if n < 1 or n > 31:
            return False
        try:
            return bool(node.sensitive)
        except Exception:
            return False

    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            buttons = app.findChildren(is_sensitive_day)
            if buttons:
                buttons[0].click()
                return
        except Exception:
            pass
        time.sleep(0.5)

    # Diagnostic: what day buttons did we see at all?
    seen = []
    try:
        def collect(node):
            if node.roleName == "push button":
                name = (node.name or "").strip()
                if name.isdigit():
                    sens = "sensitive" if getattr(node, "sensitive", False) else "insensitive"
                    seen.append(f"{name} ({sens})")
            return False
        app.findChildren(collect)
    except Exception:
        pass
    summary = ", ".join(seen[:40]) if seen else "(no day buttons found)"
    raise AssertionError(f"No sensitive day buttons in rebase calendar. Saw: {summary}")


@step('Count of sensitive day buttons in the rebase calendar is at least {n:d}')
def assert_rebase_day_count(context, n):
    """Verify the rebase dialog's calendar has at least N selectable day
    buttons — proves the live fetch surfaced enough rollback targets to
    actually be useful (user direction: ≥4, ideally 8). Sensitive = the
    underlying image version exists; insensitive grey-out cells don't count.
    """
    app = context.finupdate.instance
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            buttons = app.findChildren(
                lambda node: node.roleName == "push button"
                and (node.name or "").strip().isdigit()
                and 1 <= int((node.name or "").strip()) <= 31
                and bool(getattr(node, "sensitive", False))
            )
            if len(buttons) >= n:
                print(f"[debug] rebase calendar: {len(buttons)} sensitive days (need ≥{n})")
                return
        except Exception:
            pass
        time.sleep(0.5)
    visible = []
    try:
        def collect(node):
            if node.roleName == "push button":
                name = (node.name or "").strip()
                if name.isdigit():
                    sens = "✓" if getattr(node, "sensitive", False) else "✗"
                    visible.append(f"{sens}{name}")
            return False
        app.findChildren(collect)
    except Exception:
        pass
    raise AssertionError(
        f"Rebase calendar reached only {len(visible)} day buttons "
        f"(wanted ≥ {n} sensitive within 30s). Seen: {' '.join(visible[:60])}"
    )


@step('Click button whose label starts with "{prefix}" in "{app_id}"')
def click_button_with_prefix(context, prefix, app_id):
    """Click the first push button whose name starts with `prefix`.

    Useful for buttons whose label depends on runtime state — the rebase
    dialog's "Rebase to Mar 18…" button label varies by the selected date.
    """
    app = getattr(context, app_id).instance
    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            matches = app.findChildren(
                lambda n: n.roleName == "push button"
                and (n.name or "").startswith(prefix)
            )
            if matches:
                matches[0].click()
                return
        except Exception:
            pass
        time.sleep(0.3)
    raise AssertionError(f"No push button starting with {prefix!r} found in {app_id}")


# ── Diagnostics ───────────────────────────────────────────────────────────

@step('Dump AT-SPI tree of "{app_id}" to artifact')
def dump_tree(context, app_id):
    """Useful when developing new selectors. Captures the full a11y tree."""
    app = getattr(context, app_id)
    try:
        tree = app.instance.dump()
    except AttributeError:
        # Fallback: build a simple textual tree.
        out = []
        def walk(node, depth=0):
            try:
                out.append("  " * depth + f"[{node.roleName}] {node.name!r}")
                for child in node.children:
                    walk(child, depth + 1)
            except Exception:
                pass
        walk(app.instance)
        tree = "\n".join(out)
    target = Path(os.environ.get("DOGTAIL_ARTIFACTS", "/tmp")) / f"{app_id}-tree.txt"
    target.write_text(tree)
    print(f"Tree dumped to {target}")
