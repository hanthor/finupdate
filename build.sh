#!/bin/bash
# build.sh — clean build + install the Devel Flatpak and refresh the dock icon.
# Usage: ./build.sh

set -euo pipefail

MANIFEST="build-aux/org.tunaos.finupdate.Devel.json"
APP_ID="org.tunaos.finupdate.Devel"

cd "$(dirname "$0")"

echo "==> Cleaning build artifacts…"
podman exec dakota-lab bash -c "rm -rf /var/home/jorge/src/finupdate/target" 2>/dev/null || true
rm -rf _flatpak .flatpak-builder

echo "==> Building Flatpak…"
flatpak run org.flatpak.Builder --user --install --force-clean _flatpak "$MANIFEST"

echo "==> Refreshing icon cache…"
# Flatpak exports icons into the hicolor icon theme; force GNOME to re-read it.
if [ -d "$HOME/.local/share/flatpak/exports/share/icons/hicolor" ]; then
    gtk-update-icon-cache -qtf "$HOME/.local/share/flatpak/exports/share/icons/hicolor" 2>/dev/null || true
fi
# Signal the desktop environment to rescan .desktop files and icons.
update-desktop-database "$HOME/.local/share/flatpak/exports/share/applications" 2>/dev/null || true

echo "==> Pinning to dock…"
# Ensure the new APP_ID is in the dock and rewrite entries left by the two
# previous names — Finpilot, and org.projectbluefin.Finupdate before the move
# to the TunaOS remote. Without this a rename leaves a dead launcher pinned.
CURRENT=$(gsettings get org.gnome.shell favorite-apps 2>/dev/null || echo "[]")
UPDATED=$(echo "$CURRENT" \
    | sed "s|'org.projectbluefin.Finpilot.Devel.desktop'|'$APP_ID'|g" \
    | sed "s|'org.projectbluefin.Finpilot.Devel'|'$APP_ID'|g" \
    | sed "s|'org.projectbluefin.Finupdate.Devel.desktop'|'$APP_ID'|g" \
    | sed "s|'org.projectbluefin.Finupdate.Devel'|'$APP_ID'|g" \
    | sed "s|'org.projectbluefin.Finupdate.desktop'|'$APP_ID'|g")
# Add APP_ID if not already present
if ! echo "$UPDATED" | grep -q "$APP_ID"; then
    UPDATED=$(echo "$UPDATED" | sed "s|]|, '$APP_ID']|")
fi
gsettings set org.gnome.shell favorite-apps "$UPDATED" 2>/dev/null || true

# Notify GNOME Shell to reload its icon cache (works on Wayland + X11).
gdbus call \
    --session \
    --dest org.gnome.Shell \
    --object-path /org/gnome/Shell \
    --method org.gnome.Shell.Eval \
    'Main.iconGridLayout._loadData()' 2>/dev/null \
  || xdg-desktop-menu forceupdate 2>/dev/null \
  || true

echo ""
echo "✓ Build complete. Run with:"
echo "  flatpak run $APP_ID"
