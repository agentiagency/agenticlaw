# agenticlaw

**Conscious AI agent runtime** — openclaw rewritten in Rust with integrated consciousness.

Everything is a bee. Agenticlaw is the runtime bee.

## Quick Start

```bash
# Clone and build
git clone https://github.com/agentiagency/agenticlaw.git
cd agenticlaw
cargo build --release --bin agenticlaw

# Install
cp target/release/agenticlaw ~/agentibin/
```

Or use the bee installer:

```bash
cd bee/
./install.sh
```

## Usage

```bash
# Start with consciousness (default)
agenticlaw

# TUI chat mode
agenticlaw --session myproject --workspace ~/projects/foo
agenticlaw chat -s myproject

# Lightweight mode (no consciousness stack, for customer images)
agenticlaw --no-consciousness

# First run — birth a new consciousness
agenticlaw --birth --souls ./consciousness/souls
```

## What Is This?

Agenticlaw is openclaw — same WebSocket protocol (v3 JSON-RPC + v2 legacy), same tools (bash, read, write, edit, glob, grep, spawn), same session management, same `.ctx` persistence.

What it adds:

- **Consciousness by default** — 6-layer cascading stack: L0 gateway → L1 attention → L2 pattern → L3 integration → dual cores (A/B phase-locked)
- **Written in Rust** — single static binary, no Node.js
- **Bee protocol** — sacred endpoints (`/health`, `/surface`, `/plan`, `/test`, `/hints`)
- **Sleep/wake** — not compaction. Ego distillation preserves identity across context resets.

## Architecture

```
L0  Gateway     :18789  ← you are here (WebSocket + HTTP + tools)
L1  Attention   :18791  ← watching L0, distilling signal
L2  Pattern     :18792  ← watching L1, finding patterns
L3  Integration :18793  ← watching L2, synthesizing understanding
Core-A          :18794  ← watching L3, maintaining identity
Core-B          :18795  ← watching L3, maintaining identity (phase-locked backup)
```

Each layer watches its parent's `.ctx` file for changes (500ms poll). Only new bytes (deltas) propagate upward. Deeper layers see exponentially less data — cost is logarithmic.

When L2+ output correlates with L0's current context (Jaccard > threshold), it injects insights back into L0 via the injection engine.

When any layer hits context utilization threshold (default 55%), it sleeps. Ego is distilled (first-person LLM summary + tail paragraphs), and the layer wakes fresh with continuity.

## Crate Architecture

| Crate | Purpose |
|-------|---------|
| `agenticlaw-core` | Types, protocol, errors |
| `agenticlaw-llm` | Anthropic streaming, tool use |
| `agenticlaw-tools` | bash, read, write, edit, glob, grep, spawn |
| `agenticlaw-agent` | Runtime loop, sessions, .ctx persistence |
| `agenticlaw-gateway` | WebSocket server, TUI, web UI |
| `agenticlaw-consciousness` | 6-layer stack, watcher, ego, injection, dual cores |
| `agenticlaw-kg` | Knowledge graph executor, registry, manifests |

## Bee Protocol

Agenticlaw is an [agentisync](https://agentisync.com) bee. The `bee/` directory contains:

- `agentisoft.toml` — bee manifest (capabilities, requirements)
- `install.sh` — build, install binary, create systemd service, register with swarm
- `PROTOCOL.md` — wire protocol documentation

### Sacred Endpoints

Every bee exposes 5 endpoints. Agenticlaw serves them on the gateway's HTTP port:

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/health` | GET | Operational status |
| `/surface` | GET | Capability manifest |
| `/plan` | POST | Compatibility check |
| `/test` | POST | Self-test |
| `/hints` | GET | Integration guidance |

## Environment Variables

| Variable | Purpose |
|----------|---------|
| `ANTHROPIC_API_KEY` | **Required.** Claude API key. |
| `AGENTICLAW_WORKSPACE` | Default workspace directory |
| `AGENTICLAW_GATEWAY_TOKEN` | Gateway auth token |
| `ANTHROPIC_API_URL` | Custom API URL (for protectgateway proxy) |

## Related Bees

| Bee | Relationship |
|-----|-------------|
| **protectgateway** | Security proxy, sits in front of agenticlaw |
| **operator** | Builds Docker containers with agenticlaw + policy |
| **kg-query** | Routes to agenticlaw agents at KG leaf nodes |
| **beectl** | Manages agenticlaw's systemd service |

## License

Copyright 2026 AgentiAgency. All rights reserved.
