#!/usr/bin/env bash
# Install libfinupdate.so + finupdate.h + libfinupdate.pc into a prefix
# so a downstream gnome-control-center build can `pkg-config --libs
# libfinupdate` to link against it.
#
# Usage:
#   build-aux/install-libfinupdate.sh [PREFIX]
#
# PREFIX defaults to /usr/local. The cc-panel build expects the .pc file
# in $PREFIX/lib/pkgconfig (the standard search path) — adjust
# PKG_CONFIG_PATH if you install elsewhere.

set -euo pipefail

PREFIX="${1:-/usr/local}"
PROFILE="${PROFILE:-release}"
REPO="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"

cd "$REPO"

VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
SRC_DIR="target/${PROFILE}"
HEADER="${SRC_DIR}/include/finupdate.h"

# Build the cdylib if it's missing or older than src/. The build needs
# libadwaita / gtk4 dev headers, which Bluefin/Dakota typically ship
# inside a `finupdate` toolbox rather than on the host — fall through
# to `toolbox run` if `cargo build` fails on the host. Skip rebuilding
# when the artifacts are already fresher than every source file.
needs_build=1
if [[ -f "$SRC_DIR/libfinupdate.so" && -f "$HEADER" ]]; then
  newest_src=$(find src Cargo.toml -type f -newer "$SRC_DIR/libfinupdate.so" -print -quit 2>/dev/null || true)
  if [[ -z "$newest_src" ]]; then
    needs_build=0
  fi
fi

if (( needs_build )); then
  echo "==> Building libfinupdate ($PROFILE)…"
  build_args=(--lib)
  [[ "$PROFILE" == release ]] && build_args+=(--release)

  if ! cargo build "${build_args[@]}" 2>/dev/null; then
    echo "==> Host cargo build failed (missing dev headers?), retrying in toolbox…"
    toolbox run --container finupdate cargo build "${build_args[@]}"
  fi
fi

if [[ ! -f "$SRC_DIR/libfinupdate.so" ]]; then
  echo "Error: $SRC_DIR/libfinupdate.so not produced — check the build output above." >&2
  exit 1
fi

echo "==> Installing into $PREFIX (version $VERSION)…"
install -d "$PREFIX/lib" "$PREFIX/include" "$PREFIX/lib/pkgconfig"

install -m 0755 "$SRC_DIR/libfinupdate.so" "$PREFIX/lib/libfinupdate.so.${VERSION}"
ln -sf "libfinupdate.so.${VERSION}" "$PREFIX/lib/libfinupdate.so.0"
ln -sf "libfinupdate.so.0"          "$PREFIX/lib/libfinupdate.so"

install -m 0644 "$HEADER" "$PREFIX/include/finupdate.h"

sed -e "s|@PREFIX@|$PREFIX|g" \
    -e "s|@VERSION@|$VERSION|g" \
    cc-panel/libfinupdate.pc.in \
    > "$PREFIX/lib/pkgconfig/libfinupdate.pc"

echo "==> Done. To verify:"
echo "    PKG_CONFIG_PATH=$PREFIX/lib/pkgconfig pkg-config --libs libfinupdate"
