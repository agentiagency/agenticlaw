#!/bin/sh
# Rustclaw container entrypoint
#
# Architecture:
#   External client → protectgateway(:18789) ──scrub──► agenticlaw(:18790)
#                          ↕ scrubs WebSocket stream in both directions
#
# protectgateway intercepts the WS traffic between client and agenticlaw,
# scrubbing prompts inbound and tool responses outbound.
set -eu

PG_PORT="${PROTECT_PORT:-18789}"
RC_PORT="${RUSTCLAW_PORT:-18790}"
PG_CONFIG="/etc/agenticlaw/pg-config.yaml"

echo "=== Rustclaw Agent Container ===" >&2
echo "Role: ${ROLE:-unknown}" >&2
echo "ProtectGateway: :${PG_PORT} (WS proxy, scrubs both directions)" >&2
echo "Rustclaw:       :${RC_PORT} (agent)" >&2

# Start agenticlaw gateway first
agenticlaw gateway --port "${RC_PORT}" --bind loopback --no-auth &
RC_PID=$!

# Wait for agenticlaw ready
i=0
while [ $i -lt 30 ]; do
    if wget -qO /dev/null "http://127.0.0.1:${RC_PORT}/health" 2>/dev/null; then
        echo "Rustclaw ready on :${RC_PORT}" >&2
        break
    fi
    sleep 0.5
    i=$((i + 1))
done

# Start protectgateway in WS proxy mode — scrubs the stream between
# external clients and agenticlaw
protectgateway \
    --config "$PG_CONFIG" \
    --ws-proxy \
    --ws-listen "0.0.0.0:${PG_PORT}" \
    --ws-upstream "ws://127.0.0.1:${RC_PORT}" &
PG_PID=$!

# Wait for protectgateway ready
i=0
while [ $i -lt 30 ]; do
    if wget -qO /dev/null "http://127.0.0.1:18788/health" 2>/dev/null; then
        echo "ProtectGateway ready on :${PG_PORT}" >&2
        break
    fi
    sleep 0.5
    i=$((i + 1))
done

echo "=== Container ready ===" >&2

# If we get TERM/INT, kill both and exit
trap "kill $PG_PID $RC_PID 2>/dev/null; exit 0" EXIT TERM INT

# Wait forever — container stays alive as long as both processes run
while kill -0 $PG_PID 2>/dev/null && kill -0 $RC_PID 2>/dev/null; do
    sleep 5
done

echo "A process exited, shutting down" >&2
kill $PG_PID $RC_PID 2>/dev/null || true
