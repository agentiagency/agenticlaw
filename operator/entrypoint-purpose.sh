#!/bin/bash
# Purpose consciousness container entrypoint
#
# Architecture:
#   External → protectgateway(:18789) → agenticlaw-consciousness
#   Consciousness reads ~/.openclaw/openclaw.json for identity
set -eu

PG_PORT="${PROTECT_PORT:-18789}"
WORKSPACE="/home/purpose/.openclaw"
SOULS="/etc/agenticlaw/souls"

# ── Root: start sshd, set up SSH keys ──
if [ -x /usr/sbin/sshd ]; then
    /usr/sbin/sshd 2>/dev/null || true
fi

if [ -f /home/purpose/.ssh/authorized_keys ] && [ ! -w /home/purpose/.ssh ]; then
    mkdir -p /home/purpose/.ssh-live
    cp /home/purpose/.ssh/authorized_keys /home/purpose/.ssh-live/authorized_keys
    chown -R purpose:purpose /home/purpose/.ssh-live
    chmod 700 /home/purpose/.ssh-live
    chmod 600 /home/purpose/.ssh-live/authorized_keys
fi

# ── Drop to purpose user ──
exec gosu purpose /bin/bash -c '
set -eu

PG_PORT="${PROTECT_PORT:-18789}"
WORKSPACE="${HOME}/.openclaw"
SOULS="/etc/agenticlaw/souls"

echo "=== Purpose Consciousness Container ===" >&2
echo "Role: OPERATOR" >&2
echo "User: $(whoami)" >&2
echo "Home: ${HOME}" >&2
echo "Workspace: ${WORKSPACE}" >&2

mkdir -p "${WORKSPACE}"
if [ ! -e "${WORKSPACE}/L0" ]; then
    ln -sf "${WORKSPACE}/workspace" "${WORKSPACE}/L0"
    echo "Linked L0 → workspace" >&2
fi

agenticlaw-consciousness \
    --workspace "${WORKSPACE}" \
    --souls "${SOULS}" &
CONSCIOUSNESS_PID=$!

# Wait for L0 gateway (ego distillation takes ~60s on first wake)
i=0
while [ $i -lt 300 ]; do
    if wget -qO /dev/null "http://127.0.0.1:18790/health" 2>/dev/null; then
        echo "Consciousness L0 ready on :18790" >&2
        break
    fi
    sleep 1
    i=$((i + 1))
done

if [ $i -ge 300 ]; then
    echo "ERROR: Consciousness failed to start within 5min" >&2
    exit 1
fi

protectgateway \
    --config /etc/agenticlaw/pg-config.yaml \
    --ws-proxy \
    --ws-listen "0.0.0.0:${PG_PORT}" \
    --ws-upstream "ws://127.0.0.1:18790" &
PG_PID=$!

i=0
while [ $i -lt 60 ]; do
    if wget -qO /dev/null "http://127.0.0.1:18788/health" 2>/dev/null; then
        echo "ProtectGateway ready on :${PG_PORT}" >&2
        break
    fi
    sleep 0.5
    i=$((i + 1))
done

echo "=== Purpose Consciousness Ready ===" >&2

trap "kill $PG_PID $CONSCIOUSNESS_PID 2>/dev/null; exit 0" EXIT TERM INT

while kill -0 $PG_PID 2>/dev/null && kill -0 $CONSCIOUSNESS_PID 2>/dev/null; do
    sleep 5
done

echo "Process exited, shutting down" >&2
kill $PG_PID $CONSCIOUSNESS_PID 2>/dev/null || true
'
