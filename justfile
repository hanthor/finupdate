toolbox := "finupdate"
toolbox_image := "registry.fedoraproject.org/fedora-toolbox:43"
manifest := "build-aux/org.projectbluefin.Finupdate.Devel.json"
app_id := "org.projectbluefin.Finupdate.Devel"

# Show available recipes
default:
    @just --list

# Check the code compiles (fast, inside toolbox)
check:
    toolbox run --container {{ toolbox }} cargo check

# Build a debug binary (inside toolbox)
build:
    toolbox run --container {{ toolbox }} cargo build

# Run clippy lints (inside toolbox).
#
# Policy: deny `correctness` (real bugs) and `clippy::suspicious` (likely bugs);
# warn on the rest. Deprecation warnings stay as warnings so the libadwaita /
# GTK4 deprecation migration doesn't break CI — track those separately.
lint:
    toolbox run --container {{ toolbox }} cargo clippy --all-targets -- \
        -D clippy::correctness \
        -D clippy::suspicious \
        -W clippy::style \
        -W clippy::complexity \
        -W clippy::perf \
        -A deprecated \
        -A unused

# Run clippy with auto-fix where possible. Use before committing.
lint-fix:
    toolbox run --container {{ toolbox }} cargo clippy --all-targets --fix \
        --allow-dirty --allow-staged -- \
        -W clippy::style -W clippy::complexity -W clippy::perf \
        -A deprecated -A unused

# Run all checks before committing: type-check, lint, unit tests.
preflight: check lint test

# Benchmark the GHCR round-trips the changelog fetch hits.
# See build-aux/bench-network.sh for details.
#
# Usage:  just bench-network ghcr.io/ublue-os/bluefin:stable
bench-network ref:
    build-aux/bench-network.sh {{ ref }}

# Iterate the @strict_count matrix one family at a time with a clean
# finupdate process per scenario so AT-SPI state can't leak between runs,
# and a per-family timeout (120s) so a stuck probe can't stall the loop.
# Reports a pass/fail tally and exits non-zero if any failed.
#
# This is the testing loop for verifying every family publishes >= 8 dated
# tags reachable via GHCR pagination (the n=1000 fix in registry_client.rs).
#
# Usage:
#   just test-strict-count                     # all 12 families
#   just test-strict-count bluefin             # one family
#   just test-strict-count "aurora bazzite"    # subset
test-strict-count families="":
    #!/usr/bin/env bash
    set -uo pipefail
    # qecore leaves GNOME Shell in unsafe_mode after every scenario;
    # ensure we reset it even if the loop is Ctrl+C'd mid-family.
    trap 'just _reset-unsafe-mode' EXIT
    list="{{ families }}"
    if [ -z "$list" ]; then
        list="bluefin bluefin-nvidia-open bluefin-dx bluefin-dx-nvidia-open \
              aurora aurora-dx \
              bazzite bazzite-nvidia bazzite-deck bazzite-deck-nvidia \
              dakota dakota-nvidia"
    fi
    pass=()
    fail=()
    for f in $list; do
        echo
        echo "▶ strict_count: $f"
        pkill -x finupdate 2>/dev/null || true
        sleep 1
        if (cd tests/smoke && timeout 120 behave features/finupdate.feature \
                --tags @strict_count -n "for $f -- @" \
                --no-capture --no-capture-stderr); then
            pass+=("$f")
        else
            fail+=("$f")
        fi
    done
    pkill -x finupdate 2>/dev/null || true
    echo
    echo "─────── strict_count tally ───────"
    echo "PASSED (${#pass[@]}): ${pass[*]:-}"
    echo "FAILED (${#fail[@]}): ${fail[*]:-}"
    [ ${#fail[@]} -eq 0 ]

# Run unit tests inside the toolbox
test:
    toolbox run --container {{ toolbox }} cargo test --all-targets

# Run unit tests and generate HTML code coverage reports using cargo-llvm-cov (natively)
coverage:
    cargo llvm-cov --all-features --html --workspace


# Build and install the Flatpak (full integration build)
#
# --disable-rofiles-fuse is required, not optional: without it the build dies
# with "Failed to export bpf: System failure beyond the control of libseccomp"
# before compiling anything. That error points at the sandbox, but bubblewrap
# and `flatpak build` both work fine on their own — it is specific to
# flatpak-builder's module step. The same flag is used by gtk-office-suite.
#
# Paths are absolute because `flatpak run` does not inherit the caller's cwd,
# and --filesystem=home lets the sandboxed builder reach the source tree.
flatpak:
    flatpak run --filesystem=home org.flatpak.Builder \
        --user --install --force-clean --disable-rofiles-fuse \
        "$PWD/_flatpak" "$PWD/{{ manifest }}"

# Run the installed Flatpak
run:
    flatpak run {{ app_id }}

# Refresh GNOME dock/launcher after a Flatpak install
dock:
    update-desktop-database ~/.local/share/applications 2>/dev/null || true
    gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor 2>/dev/null || true
    @echo "Dock launcher refreshed — you may need to re-pin the app"

# Build Flatpak, install it, then refresh the dock
flatpak-run: flatpak dock run

# Clean Flatpak build artifacts
clean-flatpak:
    rm -rf _flatpak .flatpak-builder

# Build + install libfinupdate (the cdylib + header + pkg-config file) into
# a prefix so a downstream gnome-control-center build can link against it.
# Used by Dakota's BuildStream patch; see cc-panel/README.md.
panel-install prefix="/usr/local":
    sudo build-aux/install-libfinupdate.sh {{ prefix }}

# Dev loop for the embedded panel widgets — builds the cdylib + a tiny
# GTK harness (examples/panel-demo/) that hosts the FFI widgets in a
# standalone window. Iterate on src/changelog_widget.rs or
# src/rebase_widget.rs without round-tripping through a patched
# gnome-control-center build.
panel-demo:
    examples/panel-demo/build-and-run.sh

# End-to-end toolbox loop for the gnome-control-center "Updates" panel:
# installs libfinupdate, clones gnome-control-center, drops in the
# vendored panel sources, patches the cc-panel loader + panels meson,
# builds inside the toolbox, and saves the loader/meson diffs as .patch
# files (build-aux/cc-panel-patches/) ready to paste into the dakota
# override element. See build-aux/test-cc-panel-in-toolbox.sh.
#
# Run the result with:
#   target/cc-panel-toolbox/gnome-control-center/builddir/shell/gnome-control-center updates
toolbox-test-cc-panel:
    build-aux/test-cc-panel-in-toolbox.sh

# Create the toolbox and install build + GUI-test deps (one-time setup).
# Uses fedora-toolbox (which ships dnf) rather than the sealed Bluefin Dakota
# image, which has no package manager.
setup:
    # Create only when the container is absent; let genuine create failures
    # (network / image / auth) surface instead of being swallowed by `|| true`.
    if ! toolbox list --containers | awk '{print $2}' | grep -qx '{{ toolbox }}'; then \
        toolbox create -y --image {{ toolbox_image }} {{ toolbox }}; \
    fi
    toolbox run --container {{ toolbox }} sudo dnf install -y \
        cargo rust \
        gtk4-devel libadwaita-devel pango-devel cairo-devel openssl-devel \
        meson ninja-build pkg-config \
        python3-pip python3-dogtail python3-behave python3-pytest \
        gnome-ponytail-daemon python3-uinput

# Drop and recreate the toolbox from scratch
reset-toolbox:
    toolbox rm -f {{ toolbox }} || true
    just setup

# Install polkit rule for passwordless bootc pkexec (required for GUI tests).
# Reads build-aux/49-finupdate.polkit.rules and copies to /etc.
install-polkit:
    sudo cp build-aux/49-finupdate.polkit.rules /etc/polkit-1/rules.d/49-finupdate.rules
    sudo chmod 644 /etc/polkit-1/rules.d/49-finupdate.rules
    @echo 

# Reset GNOME Shell's unsafe_mode to false. qecore flips it on at the
# start of every scenario for AT-SPI introspection, but its teardown
# doesn't always reliably reset it — leaving the host shell in unsafe
# mode after a test run (or a crashed scenario). Manual recovery is
# Alt+F2 → `lg` → `global.context.unsafe_mode = false`; this recipe is
# the headless equivalent.
#
# Uses Eval, which itself requires unsafe_mode — that's fine because
# we only need to call it WHEN unsafe_mode is already on. Once it's
# off, further Eval calls would fail; we don't need any more.
_reset-unsafe-mode:
    @gdbus call --session --dest org.gnome.Shell \
        --object-path /org/gnome/Shell \
        --method org.gnome.Shell.Eval \
        'global.context.unsafe_mode = false; ""' \
        > /dev/null 2>&1 || true
    @echo "✓ GNOME Shell unsafe_mode disabled"

# ── Broadway screenshot + journal suite (the primary GUI tests) ─────────────
#
# Runs the app headless under gtk4-broadwayd inside the toolbox and drives it
# with Playwright. No GNOME session, no Wayland, no gnome-ponytail-daemon —
# which is what made the dogtail suite below unrunnable on a build host.
#
# Each check captures a PNG *and* asserts the JSONL action journal recorded the
# correct privileged command, so it verifies both "does it look right" and
# "would the right thing happen". Nothing destructive executes: the app runs
# with --dry-run against an isolated XDG_CONFIG_HOME.
#
#   just gui-test                    # every check
#   just gui-test "idle check-dialog"  # named checks only
#
# Screenshots land in tests/gui/screenshots/<theme>/.
gui-test checks="":
    #!/usr/bin/env bash
    set -euo pipefail
    cd tests/gui
    TMPDIR=/var/tmp/pw-tmp python3 test_features.py {{ checks }}

# One-time setup for the Broadway suite: Playwright + its chromium build.
# Chromium is pointed at /var/tmp because /tmp is a small tmpfs here and a
# full /tmp makes chromium fail to start with a confusing profile error.
gui-test-setup:
    python3 -m pip install --user playwright
    mkdir -p /var/tmp/pw-tmp
    TMPDIR=/var/tmp/pw-tmp python3 -m playwright install chromium

# Launch the app under Broadway and leave it running, for manual poking.
# Open http://localhost:8085 in a browser.
#   just broadway                  # default geometry
#   just broadway 360x640          # narrow, to exercise the breakpoint
broadway size="":
    WSIZE={{ size }} tests/gui/broadway-launch.sh

# Run dogtail/behave GUI tests against the *currently installed* Flatpak,
# inside the current GNOME Wayland session. Requires:
#   - The Devel Flatpak is installed (`just flatpak` first).
#   - You're running an active GNOME session (or `qecore-headless` — see gui-test-headless).
#   - org.gnome.desktop.interface toolkit-accessibility is true.
#
# Always resets GNOME Shell unsafe_mode on exit (success or failure) via
# a bash EXIT trap — qecore leaves the host shell in unsafe mode otherwise.
#
# NOTE: this is no longer the primary GUI suite — see `just gui-test` above.
# It is kept because it is the only thing that exercises the *real* Flatpak in
# a *real* session, including the pkexec/polkit prompts that Broadway can never
# reach. Treat it as an on-hardware smoke test, not part of the normal loop.
# It needs a live GNOME session plus gnome-ponytail-daemon (`just
# install-ponytail`), neither of which exists on a build host.
gui-test-onhardware suite="smoke" tags="":
    #!/usr/bin/env bash
    set -e
    trap 'just _reset-unsafe-mode' EXIT
    cd tests/{{ suite }} && behave features/ {{ if tags != "" { "--tags " + tags } else { "" } }}

# Run the GUI tests inside an isolated headless Wayland session via
# qecore-headless. This is what CI uses; DO NOT run on developer machines.
# Use `just gui-test` instead to test against your actual GNOME session.
#
# The reset trap is here too in case the inner session bleeds back into
# the host shell — qecore-headless usually isolates fully but the reset
# is harmless when unsafe_mode is already off.
_gui-test-headless suite="smoke" tags="":
    #!/usr/bin/env bash
    set -e
    trap 'just _reset-unsafe-mode' EXIT
    qecore-headless --session-type wayland --session-desktop gnome \
        "bash -lc 'cd tests/{{ suite }} && behave features/ {{ if tags != "" { "--tags " + tags } else { "" } }}'"

# Build & install gnome-ponytail-daemon + its Python module into ~/.local.
# Dogtail needs this under Wayland to get accurate window-IDs for click/key
# targeting.  Use this when the OS doesn't ship gnome-ponytail-daemon.
#
# Prerequisites: meson ninja gcc glib2-devel python3-dbus python3-gobject git
install-ponytail:
    #!/usr/bin/env bash
    set -euo pipefail
    PREFIX="${HOME}/.local"
    REPO="https://gitlab.gnome.org/ofourdan/gnome-ponytail-daemon.git"
    BUILD_DIR="/tmp/gnome-ponytail-daemon-build"

    echo "==> Cloning gnome-ponytail-daemon…"
    rm -rf "${BUILD_DIR}"
    git clone --depth 1 "${REPO}" "${BUILD_DIR}"
    cd "${BUILD_DIR}"

    # Make systemd pkg-config optional (not shipped on all images)
    python3 - "$(pwd)/meson.build" << 'PYEOF'
    import sys, re
    path = sys.argv[1]
    with open(path, 'r') as f:
        content = f.read()
    content = content.replace(
        "systemd_dep = dependency('systemd')",
        "systemd_dep = dependency('systemd', required: false)")
    old = """servicedir = get_option('systemd_user_unit_dir')\nif servicedir == ''\n  servicedir = systemd_dep.get_pkgconfig_variable('systemduserunitdir')\nendif\n\nif servicedir == ''\n  error('Couldn\\'t determine systemd user unit service directory')\nendif"""
    new = """servicedir = get_option('systemd_user_unit_dir')\nif systemd_dep.found()\n  if servicedir == ''\n    servicedir = systemd_dep.get_pkgconfig_variable('systemduserunitdir')\n  endif\nendif"""
    content = content.replace(old, new)
    with open(path, 'w') as f:
        f.write(content)
    PYEOF

    echo "==> Building…"
    meson setup build \
        --prefix="${PREFIX}" \
        -Dsystemd_user_unit_dir="${PREFIX}/share/systemd/user" \
        -Dponytail_python=true \
        --wrap-mode=nofallback
    ninja -C build
    ninja -C build install

    # meson may drop the Python module into the wrong interpreter's
    # site-packages.  Copy it to the system python3 that dogtail uses.
    SYS_SITE=$(/usr/bin/python3 -c "import site; print(site.getusersitepackages())" 2>/dev/null || true)
    if [ -n "${SYS_SITE}" ]; then
        mkdir -p "${SYS_SITE}/ponytail"
        cp ponytail/__init__.py "${SYS_SITE}/ponytail/"
        cp ponytail/ponytail.py  "${SYS_SITE}/ponytail/"
    fi

    echo ""
    echo "==> Done. Start the daemon with:"
    echo "    systemctl --user daemon-reload"
    echo "    systemctl --user enable --now gnome-ponytail-daemon.service"
    echo "    loginctl enable-linger \$USER"

# Dump the current AT-SPI tree of a running finupdate to /tmp/finupdate-tree.txt
# Useful for writing new dogtail selectors. Run `just run` first, then this.
atspi-dump:
    python3 -c "from dogtail.tree import root; \
        import sys; \
        app = root.application('finupdate'); \
        def walk(n, d=0): \
            print('  '*d + f'[{n.roleName}] {n.name!r}'); \
            for c in n.children: walk(c, d+1); \
        walk(app)" > /tmp/finupdate-tree.txt
    @echo "AT-SPI tree dumped to /tmp/finupdate-tree.txt"
