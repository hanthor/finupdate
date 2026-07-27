#!/usr/bin/env bash
# Build & run the finupdate "Updates" cc-panel against a fresh
# gnome-control-center checkout inside the dev toolbox. Produces:
#
#   - a working gnome-control-center binary at $WORKDIR/builddir/shell/
#     gnome-control-center that you can launch with the `updates` arg
#   - two .patch files (cc-panel-loader.patch, panels-meson.patch)
#     saved under $REPO/build-aux/cc-panel-patches/ that drop straight
#     into the dakota override element.
#
# This is the fast feedback loop for iterating on the panel before
# fighting the dakota BuildStream override. See cc-panel/README.md and
# projectbluefin/dakota PR #743.
#
# Idempotent: re-running picks up source edits and rebuilds incrementally.
# To start from scratch:  rm -rf $WORKDIR && just toolbox-test-cc-panel

set -euo pipefail

REPO="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
WORKDIR="${WORKDIR:-$REPO/target/cc-panel-toolbox}"
# Must match the GNOME platform of $TOOLBOX_IMAGE, not just its GTK version:
#   Fedora 43 -> GNOME 49  (GTK 4.20, libadwaita 1.8, gsettings-desktop-schemas 49)
#   Fedora 44 -> GNOME 50
# GTK 4.20 / libadwaita 1.8 are GNOME *49*, which is easy to misread as 50.
# gnome-control-center's gnome-50 branch requires gsettings-desktop-schemas
# >= 50.alpha and fails at meson configure on Fedora 43.
GBM_BRANCH="${GBM_BRANCH:-gnome-49}"
PATCH_OUT="$REPO/build-aux/cc-panel-patches"

mkdir -p "$WORKDIR" "$PATCH_OUT"

# Override both together for a GNOME 50 build:
#   TOOLBOX=finupdate-f44 GBM_BRANCH=gnome-50 build-aux/test-cc-panel-in-toolbox.sh
TOOLBOX="${TOOLBOX:-finupdate}"

echo "==> Building & installing libfinupdate into toolbox /usr/local"
# Host /usr/local is read-only on Bluefin/Dakota (OSTree). The toolbox
# has a writable /usr/local that's shared with subsequent toolbox runs,
# which is exactly what we need for the cc-panel build below.
toolbox run --container "$TOOLBOX" bash -c "
  set -euo pipefail
  cd '$REPO'
  sudo build-aux/install-libfinupdate.sh /usr/local
"

CC_DIR="$WORKDIR/gnome-control-center"
if [[ ! -d "$CC_DIR/.git" ]]; then
  echo "==> Cloning gnome-control-center ($GBM_BRANCH)"
  # gitlab.gnome.org resets connections intermittently — a single failed clone
  # would otherwise abort the whole build after libfinupdate had already been
  # installed. Retry a few times, and clean up a partial checkout between
  # attempts so the retry isn't refused for a non-empty target.
  cloned=0
  for attempt in 1 2 3 4 5; do
    if git clone --depth=1 --branch "$GBM_BRANCH" \
         https://gitlab.gnome.org/GNOME/gnome-control-center.git "$CC_DIR"; then
      cloned=1; break
    fi
    echo "    clone attempt $attempt failed; retrying in $((attempt * 5))s"
    rm -rf "$CC_DIR"
    sleep $((attempt * 5))
  done
  if [[ $cloned -ne 1 ]]; then
    echo "!! could not clone gnome-control-center after 5 attempts" >&2
    exit 1
  fi
fi

echo "==> Syncing vendored panel sources into panels/updates/"
mkdir -p "$CC_DIR/panels/updates"
cp -r "$REPO/cc-panel/panels/updates/." "$CC_DIR/panels/updates/"

# --- patch shell/cc-panel-loader.c ------------------------------------
# Idempotent: only insert if our marker isn't already present.
LOADER="$CC_DIR/shell/cc-panel-loader.c"
if ! grep -q "cc_updates_panel_get_type" "$LOADER"; then
  echo "==> Patching $LOADER"
  python3 - "$LOADER" <<'PY'
import re, sys
path = sys.argv[1]
src = open(path).read()

# 1. Add the extern declaration after the FIRST existing one — that's
#    always unconditional (cc_applications_panel_get_type). Inserting
#    after the *last* extern risks landing inside an #ifdef block.
extern_re = re.compile(r'(extern GType cc_\w+_panel_get_type \(void\);\n)')
m = extern_re.search(src)
if not m:
    sys.exit("could not find any extern GType cc_*_panel_get_type declarations")
insert_at = m.end()
src = (src[:insert_at]
       + 'extern GType cc_updates_panel_get_type (void);\n'
       + src[insert_at:])

# 2. Add the PANEL_TYPE entry inside default_panels[].
#    Insert just before the closing brace of that array.
arr_re = re.compile(r'(static CcPanelLoaderVtable default_panels\[\] =\s*\{[^}]*?)(\};)',
                    re.S)
m = arr_re.search(src)
if not m:
    sys.exit("could not locate default_panels[] array")
# PANEL_TYPE arity differs across cc branches: gnome-49 = 3 args
# (name, get_type, init_func), gnome-50 = 2 args. Detect from the macro.
if re.search(r'#define\s+PANEL_TYPE\s*\([^)]*init_func[^)]*\)', src):
    entry = '  PANEL_TYPE("updates",         cc_updates_panel_get_type,              NULL),\n'
else:
    entry = '  PANEL_TYPE("updates",         cc_updates_panel_get_type),\n'
src = src[:m.end(1)] + entry + src[m.end(1):]

open(path, 'w').write(src)
PY
else
  echo "==> $LOADER already patched, skipping"
fi

# --- patch panels/meson.build ----------------------------------------
PMESON="$CC_DIR/panels/meson.build"
if ! grep -q "subdir('updates')" "$PMESON"; then
  echo "==> Patching $PMESON"
  # Append to the panels list — order doesn't matter for the build.
  printf "\nsubdir('updates')\n" >> "$PMESON"
else
  echo "==> $PMESON already patched, skipping"
fi

# --- save patches for the dakota PR ----------------------------------
echo "==> Saving patch artifacts to $PATCH_OUT"
(cd "$CC_DIR" && git diff -- shell/cc-panel-loader.c > "$PATCH_OUT/cc-panel-loader.patch")
(cd "$CC_DIR" && git diff -- panels/meson.build      > "$PATCH_OUT/panels-meson.patch")

# --- build inside toolbox --------------------------------------------
# Override both together for a GNOME 50 build:
#   TOOLBOX=finupdate-f44 GBM_BRANCH=gnome-50 build-aux/test-cc-panel-in-toolbox.sh
TOOLBOX="${TOOLBOX:-finupdate}"
echo "==> Building gnome-control-center inside toolbox '$TOOLBOX'"
BLUEPRINT_DIR="$WORKDIR/blueprint-compiler"
toolbox run --container "$TOOLBOX" bash -c "
  set -euo pipefail
  # Install cc build deps once. dnf is fast when there's nothing to do.
  sudo dnf install -y 'dnf-command(builddep)' >/dev/null
  # --allowerasing: toolbox images ship systemd-standalone-tmpfiles instead of
  # full systemd, and gnome-control-center's build deps pull colord-devel ->
  # colord -> systemd, which conflicts with the stub. Letting dnf swap them is
  # harmless in a disposable toolbox, and without it builddep aborts on
  # Fedora 44 with a wall of "conflicting requests".
  sudo dnf builddep -y --allowerasing gnome-control-center >/dev/null

  # Fedora 43 ships blueprint-compiler 0.18 but cc wants >=0.19, and
  # cc's subproject fallback for blueprint-compiler is broken on this
  # meson combo. Install 0.20.x into /usr/local so cc finds it via PATH.
  if ! command -v /usr/local/bin/blueprint-compiler >/dev/null; then
    if [[ ! -d '$BLUEPRINT_DIR/.git' ]]; then
      git clone --depth=1 \
        https://gitlab.gnome.org/jwestman/blueprint-compiler.git \
        '$BLUEPRINT_DIR'
    fi
    cd '$BLUEPRINT_DIR'
    meson setup _build --prefix=/usr/local --wipe
    sudo meson install -C _build
  fi

  cd '$CC_DIR'
  if [[ ! -d builddir ]]; then
    PATH=/usr/local/bin:\$PATH \
    PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:\${PKG_CONFIG_PATH:-} \
      meson setup builddir
  fi
  PATH=/usr/local/bin:\$PATH meson compile -C builddir

  # cc resolves panel .desktop files + the org.gnome.Settings schema from
  # XDG_DATA_DIRS at runtime — neither lives in builddir/. Install to a
  # local staging prefix the launcher below can point at.
  rm -rf '$WORKDIR/staging'
  DESTDIR='$WORKDIR/staging' meson install -C builddir --quiet
  # meson's install trigger compiles schemas under the *system* prefix, not
  # the destdir, so the staged schemas stay as .xml. Compile in place.
  glib-compile-schemas '$WORKDIR/staging/usr/local/share/glib-2.0/schemas'
  # Recent GLib validates that Exec= resolves on PATH before
  # g_desktop_app_info_new_from_filename() will return non-NULL — so
  # without 'gnome-control-center' on PATH every panel desktop file is
  # treated as broken. Symlink the binary into /usr/local/bin.
  sudo ln -sf '$CC_DIR/builddir/shell/gnome-control-center' \
    /usr/local/bin/gnome-control-center
"

# Write a launcher that wires up the env vars and runs the binary.
LAUNCHER="$WORKDIR/run-cc.sh"
cat > "$LAUNCHER" <<EOF
#!/usr/bin/env bash
# Launch the patched gnome-control-center on the updates panel.
exec toolbox run --container "${TOOLBOX}" env \\
  XDG_CURRENT_DESKTOP=GNOME \\
  XDG_DATA_DIRS="$WORKDIR/staging/usr/local/share:/usr/share" \\
  GSETTINGS_SCHEMA_DIR="$WORKDIR/staging/usr/local/share/glib-2.0/schemas" \\
  LD_LIBRARY_PATH=/usr/local/lib \\
  "$CC_DIR/builddir/shell/gnome-control-center" "\${@:-updates}"
EOF
chmod +x "$LAUNCHER"

cat <<EOF

==> Done.

Binary:   $CC_DIR/builddir/shell/gnome-control-center
Run it:   $LAUNCHER          # opens the updates panel
          $LAUNCHER system   # or any other panel id

Patch artifacts (drop into dakota PR #743's override element):
  - $PATCH_OUT/cc-panel-loader.patch
  - $PATCH_OUT/panels-meson.patch
EOF
