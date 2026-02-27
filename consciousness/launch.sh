#!/bin/bash
# Launch the 5-layer consciousness stack
# Usage: ./consciousness/launch.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "Error: ANTHROPIC_API_KEY not set"
    exit 1
fi

echo "Building agenticlaw-consciousness..."
cargo build -p agenticlaw-consciousness --release 2>&1 | tail -3

echo ""
exec "${PROJECT_DIR}/target/release/agenticlaw-consciousness" \
    --workspace "${HOME}/.openclaw/consciousness" \
    --souls "${PROJECT_DIR}/consciousness/souls" \
    "$@"
