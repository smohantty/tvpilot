#!/usr/bin/env bash
# scripts/setup.sh
#
# One-time per-boot setup: launch `org.tizen.aurum-bootstrap` on the target.
# The bootstrap process owns the a11y bus; without it, AT-SPI clients either
# see no tree at all or get a stripped-down skeleton. Run this once after a
# TV reboot, before `compare.sh` or any tvpilot snap.
#
# Idempotent — does nothing if aurum-bootstrap is already running.
#
# Env knobs:
#   TARGET — rsdb target addr (default 192.168.0.234)

set -euo pipefail

TARGET="${TARGET:-192.168.0.234}"
XDG="/run/user/5001"
BOOTSTRAP_BIN="/usr/apps/org.tizen.aurum-bootstrap/bin/aurum-bootstrap"
BOOTSTRAP_PKG="org.tizen.aurum-bootstrap"

shell() { rsdb shell --target "$TARGET" -- "$@"; }
as_owner() {
    local cmd="$1"
    shell "su - owner -c \"export XDG_RUNTIME_DIR=$XDG; $cmd\""
}

bootstrap_running() {
    shell "pgrep -f $BOOTSTRAP_BIN >/dev/null"
}

echo "==> Target: $TARGET"

if bootstrap_running; then
    pid=$(shell "pgrep -f $BOOTSTRAP_BIN" | head -1 | tr -d '\r')
    echo "==> aurum-bootstrap already running (pid $pid) — nothing to do"
    exit 0
fi

echo "==> Launching aurum-bootstrap via app_launcher..."
as_owner "app_launcher -s $BOOTSTRAP_PKG" || true

# Poll up to ~5 s for service registration on the a11y bus.
for _ in 1 2 3 4 5; do
    sleep 1
    if bootstrap_running; then
        pid=$(shell "pgrep -f $BOOTSTRAP_BIN" | head -1 | tr -d '\r')
        echo "==> aurum-bootstrap is up (pid $pid)"
        exit 0
    fi
done

echo "ERROR: aurum-bootstrap did not come up within 5s" >&2
exit 1
