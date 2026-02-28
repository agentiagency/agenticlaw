#!/bin/bash
# Rustclaw test harness — runs all tests including real API integration tests
# Requires: ~/.keys.sh with ANTHROPIC_API_KEY exported
set -euo pipefail

cd "$(dirname "$0")"

echo "=== Loading API key ==="
if [ -f ~/.keys.sh ]; then
    source ~/.keys.sh
    if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
        echo "  ANTHROPIC_API_KEY loaded (${#ANTHROPIC_API_KEY} chars)"
        export ANTHROPIC_API_KEY
    else
        echo "  WARNING: ANTHROPIC_API_KEY not set in ~/.keys.sh — integration tests will skip"
    fi
else
    echo "  WARNING: ~/.keys.sh not found — integration tests will skip"
fi

echo ""
echo "=== Unit tests (all crates, offline) ==="
cargo test --workspace -- --test-threads=4 2>&1 | grep -E "^(test |running |test result|warning:.*test)"

echo ""
echo "=== Integration tests (real API + filesystem) ==="
echo "--- agenticlaw-core ---"
cargo test -p agenticlaw-core --test core_tests 2>&1 | grep -E "^(test |test result)"

echo "--- agenticlaw-llm ---"
cargo test -p agenticlaw-llm --test llm_tests 2>&1 | grep -E "^(test |test result)"

echo "--- agenticlaw-tools ---"
cargo test -p agenticlaw-tools --test tools_tests 2>&1 | grep -E "^(test |test result)"

echo "--- agenticlaw-agent ---"
cargo test -p agenticlaw-agent --test agent_tests 2>&1 | grep -E "^(test |test result)"

echo ""
echo "=== Summary ==="
cargo test --workspace --no-run 2>&1 | grep -c "Compiling" && echo "crates compiled" || true
TOTAL=$(cargo test --workspace 2>&1 | grep "^test result" | awk '{sum += $4} END {print sum}')
echo "Total tests passed: $TOTAL"
