#!/usr/bin/env bash
# Launch finupdate under gtk4-broadwayd inside the finupdate toolbox, detached
# so it survives the ssh session that started it.
#
# Uses an ISOLATED XDG_CONFIG_HOME so the suite never reads or writes the
# user's real ~/.config/finupdate/settings.json. That matters here: an older
# build persisted dev_mode=true into that file, which would silently turn every
# "real" run into a simulated one.
set -u
REPO="$HOME/dev/hanthor/finupdate"
PORT="${PORT:-8085}"
DISPLAY_ID="${DISPLAY_ID:-:5}"
# $HOME is shared between host and toolbox; /var/tmp is NOT. The app runs
# inside the toolbox, so a /var/tmp journal path would be written to the
# container's filesystem and be invisible to the harness reading it on the host.
JOURNAL="$HOME/.finupdate-test-journal.jsonl"
TESTCFG=/var/tmp/fin-test-config
ARGS="${APP_ARGS:---dry-run --no-dev-mode}"
WSIZE="${WSIZE:-}"
FIN_IMAGE="${FIN_IMAGE:-ghcr.io/ublue-os/bluefin:stable}"

# Match on the full command line: these are launched via `toolbox run`, so
# `pkill -x finupdate` misses them and stale instances accumulate. A leftover
# instance keeps the D-Bus name and the Broadway surface, which shows up as a
# blank screenshot rather than an obvious error.
pkill -9 -f "[t]arget/debug/finupdate" 2>/dev/null
pkill -9 -f "[g]tk4-broadwayd" 2>/dev/null
sleep 2
if pgrep -f "[t]arget/debug/finupdate" >/dev/null; then
    echo "WARNING: a finupdate instance survived cleanup" >&2
fi
rm -f "$JOURNAL" /var/tmp/finupdate-broadway.log /var/tmp/broadwayd.log
rm -rf "$TESTCFG"; mkdir -p "$TESTCFG"

setsid toolbox run --container finupdate \
    gtk4-broadwayd --port "$PORT" "$DISPLAY_ID" \
    </dev/null >/var/tmp/broadwayd.log 2>&1 &

# Poll for the listener rather than sleeping a fixed interval. Under load the
# daemon occasionally took longer than the old flat `sleep 3`, and the app then
# started against a socket that wasn't there yet — surfacing much later as
# Playwright's ERR_CONNECTION_REFUSED, which reads like a harness bug rather
# than a race.
for _ in $(seq 1 40); do
    ss -ltn | grep -q ":$PORT" && break
    sleep 0.25
done

WSIZE_LINE=""
if [ -n "$WSIZE" ]; then
    WSIZE_LINE="export FINUPDATE_WINDOW_SIZE=$WSIZE"
fi

setsid toolbox run --container finupdate bash -lc "
  export GDK_BACKEND=broadway BROADWAY_DISPLAY=$DISPLAY_ID
  export GTK_ENABLE_ANIMATIONS=0
  export DBUS_SESSION_BUS_ADDRESS=
  export RUST_LOG=info
  export XDG_CONFIG_HOME=$TESTCFG
  export FINUPDATE_ACTION_JOURNAL=$JOURNAL
  export FINUPDATE_IMAGE=$FIN_IMAGE
  export TMPDIR=/var/tmp/finupdate-build
  $WSIZE_LINE
  cd $REPO
  exec ./target/debug/finupdate $ARGS
" </dev/null >/var/tmp/finupdate-broadway.log 2>&1 &

sleep 8
if ss -ltn | grep -q ":$PORT"; then
    echo "broadway up on $PORT"
else
    echo "NO LISTENER on $PORT"
fi
grep -E "CLI override|Starting Finupdate|panicked" /var/tmp/finupdate-broadway.log | head -6
