#!/usr/bin/env bash
# Build + run the FFI demo harness against the local libfinupdate.so.
# Used by `just panel-demo` from the repo root.
#
# Builds into /tmp/finupdate-dev so the dev's $PREFIX stays clean.
# Re-running this picks up source changes in src/ffi.rs, src/changelog_widget.rs,
# and src/rebase_widget.rs — the install script skips a cargo rebuild when
# the artifacts are already fresher than src/, so iterating on widget code
# is just `edit → just panel-demo`.

set -euo pipefail

REPO="$(git -C "$(dirname "$0")/../.." rev-parse --show-toplevel)"
PREFIX="${PREFIX:-/tmp/finupdate-dev}"

cd "$REPO"

echo "==> Installing libfinupdate to $PREFIX…"
PROFILE=debug build-aux/install-libfinupdate.sh "$PREFIX"

echo "==> Compiling demo harness…"
gcc \
  $(PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:${PKG_CONFIG_PATH:-}" pkg-config --cflags --libs libadwaita-1 libfinupdate) \
  -Wall \
  -o "$PREFIX/bin-panel-demo" \
  examples/panel-demo/panel-demo.c

echo "==> Launching demo (libfinupdate from $PREFIX/lib)…"
exec env LD_LIBRARY_PATH="$PREFIX/lib:${LD_LIBRARY_PATH:-}" "$PREFIX/bin-panel-demo" "$@"
