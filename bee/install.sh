#!/bin/bash
set -euo pipefail

AGENTICLAW_SRC="${HOME}/agentiagency/agenticlaw"
AGENTIBIN="${HOME}/agentibin"
AGENTISYNC_HOME="${HOME}/.agentisync"

echo "=== Installing agenticlaw bee ==="

# Step 1: Build release binary
echo "[1/4] Building agenticlaw..."
cd "$AGENTICLAW_SRC"
cargo build --release --bin agenticlaw 2>&1 | tail -5

# Step 2: Install to agentibin
echo "[2/4] Installing to ${AGENTIBIN}/agenticlaw..."
mkdir -p "$AGENTIBIN"
cp target/release/agenticlaw "${AGENTIBIN}/agenticlaw"
chmod +x "${AGENTIBIN}/agenticlaw"

# Step 3: Create systemd user service
echo "[3/4] Creating systemd user service..."
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/bee-agenticlaw.service <<EOF
[Unit]
Description=Agenticlaw AI Agent Runtime
After=network.target

[Service]
Type=simple
ExecStart=${AGENTIBIN}/agenticlaw --no-auth
Restart=on-failure
RestartSec=5
Environment=ANTHROPIC_API_KEY=%h/.agentisync/vault/anthropic_key
Environment=RUSTCLAW_WORKSPACE=%h/.agenticlaw/workspace

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
echo "  Service created: bee-agenticlaw.service"
echo "  Start with: systemctl --user start bee-agenticlaw"

# Step 4: Register with swarm if running
echo "[4/4] Registering with swarm..."
if curl -sf http://localhost:8080/health >/dev/null 2>&1; then
    curl -sf -X POST http://localhost:8080/register \
        -H 'Content-Type: application/json' \
        -d '{
            "bee": "agenticlaw",
            "port": 18789,
            "address": "127.0.0.1",
            "surface": {
                "name": "agenticlaw",
                "version": "0.2.0",
                "provides": ["runtime.agenticlaw","runtime.gateway","runtime.consciousness","agent.chat","agent.tools","agent.sessions"],
                "requires": ["runtime.rust"],
                "idempotence": "full"
            },
            "trust_level": "local",
            "registered_by": "install.sh"
        }' && echo "  Registered with swarm" || echo "  Swarm registration failed (non-fatal)"
else
    echo "  Swarm not running on :8080, skipping registration"
fi

echo ""
echo "=== agenticlaw installed ==="
echo "  Binary: ${AGENTIBIN}/agenticlaw"
echo "  Version: $(${AGENTIBIN}/agenticlaw version)"
echo ""
echo "Usage:"
echo "  agenticlaw                              # gateway + consciousness (default)"
echo "  agenticlaw --session X --workspace /p   # TUI chat"
echo "  agenticlaw --no-consciousness           # gateway only"
echo "  agenticlaw chat -s myproject            # TUI chat"
