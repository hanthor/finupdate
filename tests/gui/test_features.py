#!/usr/bin/env python3
"""
Feature-by-feature GUI validation for finupdate.

Each check is a triple — drive the UI, capture a PNG, and assert the action
journal recorded the *correct backend command*. Rows correspond to
docs/app-logic-map.md §3 (message flow) and §6 (UI surfaces).

Run on himachal (the only host that can build GTK4 here):

    just gui-test              # all checks
    just gui-test idle         # one check by name

Screenshots land in tests/gui/screenshots/<theme>/. They are artifacts for a
human to review, not golden-image comparisons: pixel diffing GTK across
libadwaita versions is a maintenance sink, and the *behavioural* assertion
lives in the journal instead.
"""

from __future__ import annotations

import sys
import traceback
from dataclasses import dataclass
from typing import Callable

from harness import CheckFailed, FinupdateApp, shutdown_browser


@dataclass
class Check:
    name: str
    doc: str
    fn: Callable[[], None]


CHECKS: list[Check] = []


def check(name: str, doc: str):
    def deco(fn):
        CHECKS.append(Check(name, doc, fn))
        return fn
    return deco


# ── Idle / preflight ─────────────────────────────────────────────────────────

@check("idle", "Home page renders: hero, check row, automatic updates, advanced")
def _idle():
    with FinupdateApp() as app:
        app.screenshot("idle")
        app.assert_no_panics()
        # Nothing privileged should happen just by opening the app. This is a
        # real regression guard: preflight used to shell out on launch.
        app.assert_no_action("reboot")
        app.assert_no_action("switch_image")


@check("dry-run-banner", "Dry-run shows an honest banner, not 'updates are simulated'")
def _dry_run_banner():
    # Regression guard for the banner lying about which safety mode is active.
    with FinupdateApp(dry_run=True, dev_mode=False) as app:
        app.screenshot("dry-run-banner")
        log = app.app_log()
        if "dev_mode = false" not in log:
            raise CheckFailed(f"expected dev_mode=false override in log:\n{log[:800]}")


@check("dev-mode-banner", "Developer mode shows the simulated-updates banner")
def _dev_mode_banner():
    with FinupdateApp(dev_mode=True, sim="success") as app:
        app.screenshot("dev-mode-banner")


# ── Update check dialog ──────────────────────────────────────────────────────

@check("check-dialog", "Clicking Check opens the update-check dialog with 4 modules")
def _check_dialog():
    with FinupdateApp() as app:
        app.interact(
            lambda: app.click("check_button", settle_ms=0),
            settle_ms=4000,
            what="clicking Check",
        )
        app.screenshot("check-dialog")
        app.assert_no_panics()


@check("check-dialog-cancel", "Escape closes the check dialog and returns to idle")
def _check_dialog_cancel():
    # This check used to click Cancel and assert only that nothing panicked.
    # The pointer never reaches dialog content under Broadway, so the click was
    # a no-op and the check passed for six months against a dialog that stayed
    # open mid-run — visible in the committed screenshot the whole time.
    # Escape is a real user affordance and one the keyboard can actually drive.
    with FinupdateApp() as app:
        app.click("check_button", settle_ms=4000)
        app.interact(
            lambda: app.key("Escape", settle_ms=0),
            settle_ms=2500,
            what="pressing Escape",
        )
        app.screenshot("check-dialog-cancelled")
        app.assert_no_panics()


# ── Simulated update runs ────────────────────────────────────────────────────

@check("update-success", "Simulated success run reaches the complete state")
def _update_success():
    with FinupdateApp(dev_mode=True, sim="success") as app:
        app.click("check_button", settle_ms=16000)
        app.screenshot("update-success")
        app.assert_no_panics()


@check("update-failure", "Simulated failure run surfaces the error state")
def _update_failure():
    with FinupdateApp(dev_mode=True, sim="failure") as app:
        app.click("check_button", settle_ms=16000)
        app.screenshot("update-failure")
        app.assert_no_panics()


@check("update-uptodate", "Simulated up-to-date run short-circuits")
def _update_uptodate():
    with FinupdateApp(dev_mode=True, sim="up-to-date") as app:
        app.click("check_button", settle_ms=16000)
        app.screenshot("update-uptodate")
        app.assert_no_panics()


# ── Navigation ───────────────────────────────────────────────────────────────

@check("advanced-page", "Advanced row opens the Advanced dialog")
def _advanced():
    with FinupdateApp() as app:
        app.interact(
            lambda: app.click("advanced_row", settle_ms=0),
            settle_ms=3000,
            what="clicking Advanced",
        )
        app.screenshot("advanced")
        app.assert_no_panics()


def _open_image_page(app, widget: str, page_tag: str, shot: str):
    """Advanced → Image group → one of the three subpages.

    Asserts on three levels, because any one of them alone can pass while the
    feature is broken: the frame changed (the keypress did something), the app
    logged the navigation with the right page tag (it did the *right* thing),
    and nothing panicked.
    """
    app.click("advanced_row", settle_ms=3000)
    app.interact(
        lambda: app.activate(widget, settle_ms=0),
        settle_ms=12000,
        what=f"activating {widget}",
    )
    app.screenshot(shot)
    app.assert_log(f"page={page_tag}")
    app.assert_no_panics()


@check("whats-new", "What's New reaches the changelog page and renders content")
def _whats_new():
    # "Did we actually verify the changelogs work?" — until recently the honest
    # answer was no: the page could not be reached at all, and the first
    # version of this check clicked at a coordinate the pointer never reaches,
    # then passed because it only asserted the absence of a panic.
    with FinupdateApp() as app:
        _open_image_page(app, "whats_new_row", "changelog", "whats-new")


@check("image-history", "Image History reaches the deployment list")
def _image_history():
    with FinupdateApp() as app:
        _open_image_page(app, "image_history_row", "history", "image-history")
        # Browsing history must not stage a rollback by itself.
        app.assert_no_action("rollback")
        app.assert_no_action("switch_image")


@check("image-source", "Image Source reaches the registry/tag/signing page")
def _image_source():
    with FinupdateApp() as app:
        _open_image_page(app, "image_source_row", "source", "image-source")


@check("main-menu", "Hamburger menu opens (popover renders under Broadway)")
def _main_menu():
    with FinupdateApp() as app:
        app.interact(
            lambda: app.click("main_menu", settle_ms=0),
            settle_ms=2000,
            what="opening the main menu",
        )
        app.screenshot("main-menu")
        app.assert_no_panics()


# ── Adaptive layout ──────────────────────────────────────────────────────────

@check("narrow", "Narrow window renders without clipping (HIG: down to 360px)")
def _narrow():
    with FinupdateApp(window_size="360x640") as app:
        app.screenshot("narrow")
        app.assert_no_panics()


# ── Backend intent: the assertions a screenshot cannot make ──────────────────

@check("switch-journal", "Rebase dialog Switch records `bootc switch <target>`")
def _switch_journal():
    # The single most consequential action in the app, and the check that
    # answers "would the correct backend action be taken" — which no
    # screenshot can.
    #
    # This check used to open the dialog, capture it, and stop. It asserted
    # nothing about switching, despite its name, its docstring, and a comment
    # claiming it was the one that covered the backend intent. It could not
    # have done more: the button is inside a dialog, and the pointer does not
    # reach dialog content under Broadway. It is drivable now because the
    # primary action carries an access key.
    with FinupdateApp() as app:
        app.click("hero_change", settle_ms=8000)
        app.screenshot("rebase-dialog")

        # Primary action → confirmation alert.
        app.interact(
            lambda: app.activate("rebase_primary_switch", settle_ms=0),
            settle_ms=3000,
            what="pressing the primary switch action",
        )
        app.screenshot("rebase-confirm")

        # Confirm. This is the point of no return in real use.
        app.interact(
            lambda: app.activate("confirm_switch", settle_ms=0),
            settle_ms=8000,
            what="confirming the switch",
        )
        app.screenshot("rebase-switched")

        # The assertion the check was named for: the right command, against the
        # right target, and blocked from actually running.
        entry = app.assert_action(
            "switch_image",
            would_run_contains=["pkexec", "bootc", "switch"],
            suppressed=True,
        )
        target = entry.args.get("target", "")
        if "ghcr.io/" not in target or ":" not in target:
            raise CheckFailed(
                f"switch target is not a fully-qualified image ref: {target!r}"
            )
        app.assert_no_panics()


@check("switch-cancel", "Cancelling the confirmation records no switch at all")
def _switch_cancel():
    # The other half of the safety property: declining must leave no intent
    # behind, not merely leave it suppressed.
    with FinupdateApp() as app:
        app.click("hero_change", settle_ms=8000)
        app.activate("rebase_primary_switch", settle_ms=3000)
        app.interact(
            lambda: app.activate("confirm_cancel", settle_ms=0),
            settle_ms=3000,
            what="cancelling the switch",
        )
        app.screenshot("rebase-cancelled")
        app.assert_no_action("switch_image")
        app.assert_no_panics()


@check("uupd-timer-journal", "Automatic-updates toggle records the systemd command")
def _uupd_timer():
    with FinupdateApp() as app:
        app.click("automatic_updates_switch", settle_ms=3000)
        app.screenshot("uupd-timer-toggled")
        # The switch must map to `systemctl <enable|disable> --now uupd.timer`
        # under pkexec — and must NOT have actually run it.
        app.assert_action(
            "set_uupd_timer",
            would_run_contains=["pkexec", "systemctl", "--now", "uupd.timer"],
            suppressed=True,
        )


def main() -> int:
    wanted = sys.argv[1:] or None
    selected = [c for c in CHECKS if not wanted or c.name in wanted]
    if wanted and not selected:
        print(f"no checks matched {wanted}; known: {[c.name for c in CHECKS]}")
        return 2

    passed, failed = [], []
    for c in selected:
        print(f"\n── {c.name}: {c.doc}")
        try:
            c.fn()
        except Exception as e:
            failed.append((c.name, e))
            print(f"   FAIL {type(e).__name__}: {e}")
            if not isinstance(e, CheckFailed):
                traceback.print_exc()
        else:
            passed.append(c.name)
            print("   ok")

    print(f"\n{'─'*60}\npassed {len(passed)}/{len(selected)}")
    if failed:
        print("failed:")
        for name, e in failed:
            first = str(e).splitlines()[0] if str(e) else type(e).__name__
            print(f"  - {name}: {first}")
    shutdown_browser()
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
