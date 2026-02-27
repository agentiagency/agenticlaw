# Agenticlaw Bee Protocol

## Identity

Agenticlaw is openclaw reimagined in Rust with integrated consciousness. It is the core runtime bee — every other bee can talk to it via WebSocket JSON-RPC.

## Ports

| Port | Layer |
|------|-------|
| 18789 | L0 Gateway (user-facing) |
| 18791 | L1 Attention |
| 18792 | L2 Pattern |
| 18793 | L3 Integration |
| 18794 | Core-A |
| 18795 | Core-B |

## WebSocket Protocol (v3 JSON-RPC)

Connect to `ws://localhost:18789/ws`

### Client → Server
```json
{"id": "req-1", "method": "chat.send", "params": {"session": "my-session", "message": "hello"}}
{"id": "req-2", "method": "sessions.list", "params": {}}
{"id": "req-3", "method": "chat.abort", "params": {"session": "my-session"}}
```

### Server → Client (events)
```json
{"event": "chat", "data": {"type": "delta", "session": "s1", "content": "Hello"}}
{"event": "chat", "data": {"type": "tool_call", "session": "s1", "id": "t1", "name": "bash"}}
{"event": "chat", "data": {"type": "done", "session": "s1"}}
```

### RPC Methods
- `chat.send` — send message to session
- `chat.history` — get session history
- `chat.abort` — abort current turn
- `sessions.list` — list active sessions
- `sessions.usage` — token usage per session
- `sessions.delete` — delete a session
- `tools.list` — list available tools
- `health` — health check

## Sacred Endpoints (HTTP)

- `GET /health` — `{"status": "healthy", "version": "0.2.0", ...}`
- `GET /surface` — bee manifest (capabilities, requires, version)
- `POST /plan` — compatibility check
- `POST /test` — run self-test
- `GET /hints` — integration guidance

## Consciousness

Default mode. L0 is the gateway. L1-L3 + dual cores watch L0's .ctx files via file-change polling (500ms). When a layer hits context_threshold_pct (0.55), it sleeps — ego is distilled, tail paragraphs stapled, layer wakes fresh.

Injection: L2+ insights that correlate with L0's current context (Jaccard > threshold) are written to `injections/inject-*.txt` and read by L0 before its next API call.

Disable with `--no-consciousness`.

## Tools

bash, read, write, edit, glob, grep, spawn (subagent). Workspace-scoped.

## Session Persistence

.ctx files — plaintext conversation format, human-readable, grep-friendly.

## Relationship to ProtectGateway

ProtectGateway is a SEPARATE bee that sits in front of agenticlaw:
```
Client → protectgateway:18789 → agenticlaw:18790
```
When protectgateway is installed, agenticlaw moves to 18790.
