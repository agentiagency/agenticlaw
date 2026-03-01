#!/bin/bash
# Purpose consciousness container entrypoint
#
# Boots with systemd as PID 1. Agenticlaw runs as a systemd user service
# under the purpose user. SSH runs via systemd. `agenticlaw chat` connects
# via WebSocket to the running service.

# ── Set up SSH authorized_keys from ro mount ──
if [ -f /home/purpose/.ssh/authorized_keys ] && [ ! -w /home/purpose/.ssh ]; then
    mkdir -p /home/purpose/.ssh-live
    cp /home/purpose/.ssh/authorized_keys /home/purpose/.ssh-live/authorized_keys
    chown -R purpose:purpose /home/purpose/.ssh-live
    chmod 700 /home/purpose/.ssh-live
    chmod 600 /home/purpose/.ssh-live/authorized_keys
fi

# ── Write agenticlaw system service (runs as purpose) ──
cat > /etc/systemd/system/agenticlaw.service << UNIT
[Unit]
Description=Agenticlaw Consciousness Stack
After=network.target

[Service]
Type=simple
User=purpose
Group=purpose
ExecStart=/usr/local/bin/agenticlaw --workspace /home/purpose/.openclaw --souls /etc/agenticlaw/souls
Restart=on-failure
RestartSec=5
Environment=ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY:-}
Environment=RUST_LOG=${RUST_LOG:-info}
Environment=RUSTCLAW_MODEL=${RUSTCLAW_MODEL:-claude-opus-4-6}
Environment=HOME=/home/purpose
WorkingDirectory=/home/purpose

[Install]
WantedBy=multi-user.target
UNIT

# Enable agenticlaw and ssh
systemctl enable agenticlaw.service
systemctl enable ssh.service

# ── Boot systemd as PID 1 ──
exec /lib/systemd/systemd
