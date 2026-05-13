#!/usr/bin/env bash
# scripts/compare.sh
#
# Perf comparison between tizen-aurum-cli and tvpilot snap on a Tizen target.
# Enforces the launching rule:
#   1. Aurum-bootstrap must already be running — run scripts/setup.sh once
#      per boot before this. This script will refuse to continue if it's
#      not up, because both clients depend on it for a useful a11y tree.
#   2. Run both tools strictly as user `owner` (uid 5001) with the right
#      XDG_RUNTIME_DIR so they can reach AT-SPI.
#   3. After the test, send `tvpilot close` so we don't leak a daemon.
#
# Env knobs:
#   TARGET     — rsdb target addr (default 192.168.0.234)
#   RUNS       — iterations per tool (default 3)
#   TVPILOT    — path to tvpilot on target (default /tmp/tvpilot)
#   AURUM_CLI  — path to aurum-cli on target
#                (default /opt/usr/share/tizen-tools/stable/cli/tizen-aurum-cli)

set -euo pipefail

TARGET="${TARGET:-192.168.0.234}"
RUNS="${RUNS:-3}"
TVPILOT="${TVPILOT:-/tmp/tvpilot}"
AURUM_CLI="${AURUM_CLI:-/opt/usr/share/tizen-tools/stable/cli/tizen-aurum-cli}"

XDG="/run/user/5001"

shell() {
    rsdb shell --target "$TARGET" -- "$@"
}

# Run a command on the target as user `owner` with XDG_RUNTIME_DIR set so
# AT-SPI is reachable. The argument is a single shell string.
as_owner() {
    local cmd="$1"
    shell "su - owner -c \"export XDG_RUNTIME_DIR=$XDG; $cmd\""
}

# Bootstrap is healthy when its process exists AND isn't in D-state. A
# D-state aurum-bootstrap looks alive to pgrep but its gRPC server is
# wedged; aurum-cli will hang for ~33 s on connect. Refuse to proceed in
# that case — only a TV reboot clears it.
bootstrap_healthy() {
    local pid state
    pid=$(shell "pgrep -f /usr/apps/org.tizen.aurum-bootstrap/bin/aurum-bootstrap" 2>/dev/null | head -1 | tr -d '\r' || true)
    [ -z "$pid" ] && return 1
    state=$(shell "awk '/^State:/ {print \$2; exit}' /proc/$pid/status 2>/dev/null" | tr -d '\r')
    [ "$state" != "D" ]
}

echo "==> Target: $TARGET"

# 1. Aurum-bootstrap must already be running AND responsive.
if ! bootstrap_healthy; then
    echo "ERROR: aurum-bootstrap is not running healthy on $TARGET" >&2
    echo "       run scripts/setup.sh first (once per boot); if that" >&2
    echo "       reports a D-state process, the TV needs a reboot." >&2
    exit 1
fi
echo "==> aurum-bootstrap healthy — proceeding"

# 2a. Time aurum-cli dump-tree.
echo "==> Timing tizen-aurum-cli dump-tree ($RUNS runs)"
for i in $(seq 1 "$RUNS"); do
    as_owner "(time $AURUM_CLI dump-tree > /tmp/aurum-out-$i.txt) 2>&1 | grep real" \
        || echo "    run $i: failed/timed out"
done

# 2b. Time tvpilot snap.
echo "==> Timing $TVPILOT snap ($RUNS runs)"
for i in $(seq 1 "$RUNS"); do
    as_owner "(time $TVPILOT snap > /tmp/tvpilot-out-$i.txt) 2>&1 | grep real" \
        || echo "    run $i: failed/timed out"
done

# 3. Output sizes for sanity (rendered tree).
echo "==> Output sizes"
shell "wc -l /tmp/aurum-out-1.txt /tmp/tvpilot-out-1.txt 2>&1" || true

# 4. Cleanup: shut tvpilot daemon down cleanly so we don't leak it.
echo "==> Closing tvpilot daemon"
as_owner "$TVPILOT close" || echo "    (close returned non-zero — daemon may already be gone)"

echo "==> Done."
