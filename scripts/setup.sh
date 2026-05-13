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

# Returns the bootstrap pid if it's alive AND not in D (disk sleep / kernel-
# stuck) state. D-state on this firmware happens when something kills the
# process mid AT-SPI / kdbus syscall — the process struct lingers but the
# gRPC server is unresponsive. Only a TV reboot clears it.
bootstrap_healthy_pid() {
    local pid state
    pid=$(shell "pgrep -f $BOOTSTRAP_BIN" 2>/dev/null | head -1 | tr -d '\r' || true)
    [ -z "$pid" ] && return 1
    state=$(shell "awk '/^State:/ {print \$2; exit}' /proc/$pid/status 2>/dev/null" | tr -d '\r')
    if [ "$state" = "D" ]; then
        echo "    pid $pid is in D (kernel-stuck) state — TV reboot required" >&2
        return 1
    fi
    echo "$pid"
    return 0
}

echo "==> Target: $TARGET"

if pid=$(bootstrap_healthy_pid); then
    echo "==> aurum-bootstrap already running (pid $pid) — nothing to do"
    exit 0
fi

# If a process exists but isn't healthy (e.g. D-state), bootstrap_healthy_pid
# already printed why. Bail rather than launching a second instance.
if bootstrap_running; then
    echo "ERROR: aurum-bootstrap is present but unhealthy — see message above" >&2
    exit 1
fi

echo "==> Launching aurum-bootstrap via app_launcher..."
as_owner "app_launcher -s $BOOTSTRAP_PKG" || true

# Poll up to ~5 s for service registration on the a11y bus.
for _ in 1 2 3 4 5; do
    sleep 1
    if pid=$(bootstrap_healthy_pid); then
        echo "==> aurum-bootstrap is up (pid $pid)"
        exit 0
    fi
done

echo "ERROR: aurum-bootstrap did not come up healthy within 5s" >&2
exit 1
