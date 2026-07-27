"""
Finupdate smoke-test environment — qecore TestSandbox for the GTK4 frontend.

Driven by `behave` with the GUI exposed over AT-SPI via dogtail. Designed to be
runnable inside `qecore-headless --session-type wayland` on CI (the same
toolchain projectbluefin/testing-lab uses), or locally in an existing
GNOME Wayland session.

The application under test is the Devel-channel Flatpak we install via
`just flatpak`, so this matches what real users see.
"""
import sys
import traceback

from qecore.sandbox import TestSandbox
from qecore.common_steps import *  # noqa: F401,F403


APP_ID = "org.tunaos.finupdate.Devel"
# AT-SPI exposes the app under its binary name, not the Flatpak app-id.
A11Y_APP_NAME = "finupdate"


def before_all(context) -> None:
    try:
        context.sandbox = TestSandbox("finupdate", context=context)
        # Don't auto-attach faf (Fedora-only failure analyzer) reports; we
        # capture our own journal slices in the steps.
        context.sandbox.attach_faf = False
        context.sandbox.production = False

        # qecore knows how to launch a Flatpak by app-id.
        context.finupdate = context.sandbox.get_flatpak(
            flatpak_id=[APP_ID],
            a11y_app_name=A11Y_APP_NAME,
        )
        # Ctrl+Q is the documented quit shortcut (see README).
        context.finupdate.exit_shortcut = "<Ctrl>Q"
    except Exception as error:
        # Capture diagnostics, then bail out — continuing past a broken
        # setup cascades into misleading per-scenario failures.
        print(f"Environment error: before_all: {error}")
        context.failed_setup = traceback.format_exc()
        raise RuntimeError("Smoke environment initialization failed") from error


def before_scenario(context, scenario) -> None:
    try:
        context.sandbox.before_scenario(context, scenario)
    except Exception:
        context.embed("text/plain", traceback.format_exc(), "Before Scenario Error")
        sys.exit(1)


def after_scenario(context, scenario) -> None:
    context.sandbox.after_scenario(context, scenario)
