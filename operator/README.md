# Rustclaw Operator

Build, test, and deploy policy-scoped AI agent containers.

The Operator produces 7 container images from the agenticlaw source, each with a baked-in policy that controls what tools the agent can use, what commands it can run, and what files/network it can access. A policy-enforcing reverse proxy (`protectgateway`) sits in front of every agent.

## Architecture

```
Client → :18789 (protectgateway) → :18790 (agenticlaw gateway)
              ↑                          ↑
         policy check              per-boot auth token
         allow/deny/ask            (loopback only)
```

Every container generates a random auth token at boot. Only protectgateway knows the token. Direct access to agenticlaw's port is blocked.

## The 7 Roles

| Role | Tools | Bash | Network | Image Size |
|------|-------|------|---------|------------|
| **READ** | read, glob, grep | denied | none | 18.5MB |
| **WRITE** | + write, edit | denied | none | 18.5MB |
| **LOCAL** | + bash | local only (no curl/wget/ssh) | none | 18.5MB |
| **POKE** | all 6 | + HTTP GET only | outbound GET | 18.5MB |
| **PROBE** | all 6 | + full HTTP | full outbound | 18.5MB |
| **AGENT** | all 6 | all (no fdisk/systemctl/reboot) | full + ports>1024 | 18.5MB |
| **OPERATOR** | all 6 | all (sub-policy: no rm -rf /) | full | 18.5MB |

Permissions are monotonically increasing. Every role denies: reading `/etc/shadow`, reading/modifying its own policy file, path traversal, base64-encoded command obfuscation, LD_PRELOAD injection.

## Quick Start

```bash
# Build all 7 images
docker build --build-arg ROLE=READ -t agenticlaw-read -f operator/Dockerfile .
docker build --build-arg ROLE=AGENT -t agenticlaw-agent -f operator/Dockerfile .
# ... or all at once:
for ROLE in READ WRITE LOCAL POKE PROBE AGENT OPERATOR; do
  docker build --build-arg ROLE=$ROLE \
    -t agenticlaw-$(echo $ROLE | tr A-Z a-z) \
    -f operator/Dockerfile .
done

# Run a single agent
docker run -d -p 18789:18789 \
  -e ANTHROPIC_API_KEY \
  --cap-drop=ALL --security-opt=no-new-privileges \
  agenticlaw-agent

# Run all 7 with docker compose
cd operator && docker compose up -d

# Test policies (no containers needed)
cd operator && cargo run --bin operator -- test --all
```

## Testing

```bash
# Unit tests (policy engine, mock provider, proxy logic)
cargo test

# Integration tests (all 7 roles, positive + negative + escalation)
cargo run --bin operator -- test --all

# Test a single role
cargo run --bin operator -- test --role READ

# Test with custom policy
cargo run --bin operator -- test --policy my-custom.json
```

### Test Coverage

- **Positive tests**: Allowed operations succeed per role
- **Negative tests**: Denied operations are blocked per role
- **Escalation tests**: Path traversal, base64 obfuscation, LD_PRELOAD, /proc/self/exe, command substitution
- **Obfuscation detection**: Variable expansion, absolute path bypass, env prefix stripping, interpreter detection, here-doc/here-string, xargs piping

## Policy Model

Policies use a 3-tier allow/deny/ask model (inspired by Claude Code's `settings.json`) with glob pattern matching:

```json
{
  "role": "READ",
  "tools": {
    "allow": ["read", "glob", "grep"],
    "deny": ["bash", "write", "edit"],
    "ask": []
  },
  "bash_commands": { "allow": [...], "deny": [...], "ask": [...] },
  "filesystem": { "allow": [...], "deny": [...], "ask": [...] },
  "network": { "allow": [...], "deny": [...], "ask": [...] }
}
```

Deny always wins. Sub-policies can be fetched from an HTTP server at startup and merged (deny-additive).

## Binaries

| Binary | Purpose |
|--------|---------|
| `operator` | CLI for build/test/push operations |
| `protectgateway` | Policy-enforcing reverse proxy |
| `mock-policy-server` | HTTP server for sub-policy testing |

## Container Security

All containers run with:
- `--cap-drop=ALL` (capabilities added back per role)
- `--security-opt=no-new-privileges`
- Per-boot random auth token (never on disk)
- protectgateway as PID 1 with trap-based cleanup

READ containers additionally use `--read-only` and `--network=none`.

## File Structure

```
operator/
├── README.md
├── SPEC.md              # Full specification
├── SPEC-v2.md           # Minification + live testing spec
├── CRITIQUE.md          # Adversarial security audit (17 findings)
├── CRITIQUE-v2.md       # v2 security audit (16 findings)
├── Cargo.toml
├── Dockerfile           # Multi-stage: rust:latest → alpine:3.21
├── entrypoint.sh        # Per-boot token generation (ash-compatible)
├── docker-compose.yml   # All 7 containers + policy server
├── policies/            # 7 role policy JSON files
│   ├── READ.json
│   ├── WRITE.json
│   ├── LOCAL.json
│   ├── POKE.json
│   ├── PROBE.json
│   ├── AGENT.json
│   └── OPERATOR.json
└── src/
    ├── main.rs          # Operator CLI
    ├── policy.rs        # Policy engine + glob matching
    ├── proxy.rs         # protectgateway WebSocket proxy
    ├── proxy_main.rs    # protectgateway binary
    ├── builder.rs       # Docker image builder
    ├── tester.rs        # Test runner framework
    ├── mock_provider.rs # Mock LLM provider for testing
    └── mock_server.rs   # Mock policy HTTP server
```
