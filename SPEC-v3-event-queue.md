# SPEC v3 — Event Queue Architecture

## 1. Executive Summary

Rustclaw v2 is a working consciousness stack: 4 layers (L0-L3) plus dual cores, with file-based cascade (.ctx watching), ego distillation on sleep/wake, and injection from lower layers into L0. It works — but the architecture has fundamental limitations:

1. **Synchronous request/response** — `AgentRuntime::run_turn()` blocks on a single user message → LLM stream → tool loop cycle. No way to interleave messages from multiple sources.
2. **Non-interruptible tool calls** — `BashTool` spawns a process with `tokio::time::timeout` but no cancellation. A human typing during a 120s bash command waits.
3. **Custom WebSocket protocol** — Rustclaw's `ClientMessage`/`ServerMessage` enums don't match OpenClaw's RPC protocol (`chat.send`, `chat.history`, etc.), so no existing OpenClaw client can connect.
4. **Minimal browser UI** — The gateway serves a raw HTML page with a textarea and pre block. No config editing, no layer visualization, no session management.
5. **No workspace identity separation** — SOUL.md is treated as both identity and tool context depending on birth/wake mode, but there's no principled workspace structure.

This spec redesigns the core around a **single ordered message queue** per consciousness instance, with interruptible tool calls, OpenClaw-compatible WebSocket API, a full browser app, and a clean workspace/identity architecture.

---

## 2. Current Architecture

### 2.1 Agent Runtime (`crates/agenticlaw-agent/src/runtime.rs`)

The agentic loop lives in `AgentRuntime::run_turn()` (line ~67). It's a synchronous loop:

```
user message → session.add_user_message()
loop {
    build LlmRequest from session messages
    provider.complete_stream() → accumulate text + tool calls
    if no tool calls → Done, break
    for each tool_call:
        tools.execute(name, args).await  ← BLOCKING, no cancellation
        session.add_tool_result()
}
```

Key limitation: `run_turn` owns the entire conversation turn. Nothing can interrupt it. The `event_tx` channel sends `AgentEvent` variants out for display, but there's no channel *in*.

### 2.2 Session Management (`crates/agenticlaw-agent/src/session.rs`)

`Session` holds:
- `messages: RwLock<Vec<LlmMessage>>` — the full conversation
- `ctx_path: Option<PathBuf>` — .ctx file on disk
- `abort_tx/abort_rx: mpsc::channel` — abort signal (exists but unused in the tool loop)

`SessionRegistry` is a `DashMap<SessionKey, Arc<Session>>`. Sessions are created per-key with .ctx persistence.

### 2.3 WebSocket Protocol (`crates/agenticlaw-core/src/protocol.rs`)

Current protocol is tag-based JSON:
- **Client→Server**: `auth`, `chat`, `abort`, `call` (RPC), `ping`
- **Server→Client**: `auth_result`, `delta`, `thinking`, `tool_call`, `tool_call_delta`, `done`, `error`, `result`, `pong`, `info`

This is custom. OpenClaw uses JSON-RPC style with methods like `chat.send`, `chat.history`, `chat.abort`, `chat.inject`, `chat.completion` and events like `chat`, `agent`, `presence`, `tick`, `health`.

### 2.4 Cascade (`crates/agenticlaw-consciousness/src/stack.rs`)

File-based: `CtxWatcher` polls .ctx file sizes every 500ms, emits `CtxChange` deltas. Each delta triggers `process_layer_update()` which calls `runtime.run_turn()` on the child layer. Per-layer `Semaphore(1)` prevents concurrent processing.

L3 deltas feed `DualCore::process_l3_delta()` in `cores.rs`, which routes to Growing/Seeded cores with phase-locked sampling.

### 2.5 Tool Implementations (`crates/agenticlaw-tools/`)

Six tools: `bash`, `read`, `write`, `edit`, `glob`, `grep`. All are `async fn execute(&self, args: Value) -> ToolResult`. BashTool uses `tokio::process::Command` with timeout but no `CancellationToken`.

### 2.6 LLM Provider (`crates/agenticlaw-llm/src/anthropic.rs`)

`AnthropicProvider::complete_stream()` returns a `Pin<Box<dyn Stream<Item = LlmResult<StreamDelta>>>>`. The SSE parser handles `content_block_start`, `content_block_delta`, `content_block_stop`, `message_delta`, `message_stop`. No support for `thinking` blocks with `budget_tokens` parameter.

---

## 3. Event Queue Architecture

### 3.1 Core Concept

Replace the synchronous `run_turn()` loop with a **single ordered message queue** (`tokio::sync::mpsc`) per consciousness layer. Every input — human messages, tool results, cascade deltas, injections, system events — enters the same queue. A single consumer loop processes events in order.

```rust
/// Every event that can enter the consciousness queue.
#[derive(Debug, Clone)]
pub enum QueueEvent {
    /// Human typed a message (from WebSocket, TUI, etc.)
    HumanMessage {
        session: SessionKey,
        content: String,
        /// Priority: human messages always preempt tool calls
        priority: Priority,
    },

    /// Tool completed execution
    ToolResult {
        session: SessionKey,
        tool_use_id: String,
        name: String,
        result: String,
        is_error: bool,
    },

    /// Cascade delta from parent layer's .ctx change
    CascadeDelta {
        from_layer: usize,
        delta: String,
    },

    /// Injection from a lower layer (L2+, Core)
    Injection {
        from: String,  // "L2", "core-a", etc.
        content: String,
    },

    /// LLM response chunk (from the streaming API)
    LlmDelta(StreamDelta),

    /// LLM response complete — triggers dispatch
    LlmComplete {
        session: SessionKey,
        text: Option<String>,
        tool_calls: Vec<AccumulatedToolCall>,
        stop_reason: String,
    },

    /// System events
    Sleep { session: SessionKey, token_count: usize },
    Wake { ego: String },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Tool results, cascade deltas, injections
    Normal = 0,
    /// Human messages — always processed next
    Human = 10,
    /// System shutdown
    System = 20,
}
```

### 3.2 Queue Consumer Loop

```rust
pub struct ConsciousnessLoop {
    /// Inbound event queue — the ONLY input
    queue_rx: mpsc::Receiver<QueueEvent>,
    /// Handle for submitting events back (tool results, LLM responses)
    queue_tx: mpsc::Sender<QueueEvent>,
    /// Output events (to WebSocket clients, TUI, etc.)
    output_tx: broadcast::Sender<OutputEvent>,
    /// The LLM provider
    provider: Arc<dyn LlmProvider>,
    /// Tool registry
    tools: Arc<ToolRegistry>,
    /// Session state
    session: Arc<Session>,
    /// Active tool handles (for interruption)
    active_tools: HashMap<String, ToolHandle>,
    /// Cancellation token for the current LLM stream
    llm_cancel: Option<CancellationToken>,
}

impl ConsciousnessLoop {
    pub async fn run(&mut self) {
        // Priority queue: drain all pending events, sort by priority,
        // process highest priority first
        loop {
            let event = self.recv_with_priority().await;
            match event {
                QueueEvent::HumanMessage { session, content, .. } => {
                    // 1. Park all active tools
                    self.park_all_tools().await;
                    // 2. Cancel any in-flight LLM stream
                    if let Some(cancel) = self.llm_cancel.take() {
                        cancel.cancel();
                    }
                    // 3. Add message to session
                    self.session.add_user_message(&content, ...).await;
                    // 4. Start new LLM call
                    self.start_llm_call().await;
                }

                QueueEvent::ToolResult { tool_use_id, name, result, is_error, .. } => {
                    self.active_tools.remove(&tool_use_id);
                    self.session.add_tool_result(&tool_use_id, &result, is_error).await;
                    // If all tools done, start next LLM call
                    if self.active_tools.is_empty() {
                        self.start_llm_call().await;
                    }
                }

                QueueEvent::LlmComplete { text, tool_calls, .. } => {
                    // Save to session
                    if tool_calls.is_empty() {
                        if let Some(t) = &text {
                            self.session.add_assistant_text(t).await;
                        }
                        self.emit(OutputEvent::Done);
                    } else {
                        // Launch tools
                        for tc in tool_calls {
                            self.launch_tool(tc).await;
                        }
                    }
                }

                QueueEvent::CascadeDelta { delta, .. } => {
                    // For inner layers: treat as user message
                    self.session.add_user_message(&delta, ...).await;
                    self.start_llm_call().await;
                }

                QueueEvent::Injection { content, .. } => {
                    // Append to next LLM call as system context
                    self.pending_injections.push(content);
                }

                QueueEvent::Shutdown => break,
                _ => {}
            }
        }
    }

    /// Receive next event, preferring higher priority.
    /// Drains all immediately available events, sorts by priority.
    async fn recv_with_priority(&mut self) -> QueueEvent {
        let first = self.queue_rx.recv().await.unwrap();
        let mut batch = vec![first];
        // Drain any immediately available events
        while let Ok(event) = self.queue_rx.try_recv() {
            batch.push(event);
        }
        // Sort: highest priority first
        batch.sort_by_key(|e| std::cmp::Reverse(e.priority()));
        // Return highest, re-queue rest
        let top = batch.remove(0);
        for event in batch {
            let _ = self.queue_tx.send(event).await;
        }
        top
    }
}
```

### 3.3 Queue Topology

```
                    ┌──────────────────────────┐
                    │    ConsciousnessLoop      │
                    │   (single consumer)       │
                    └──────────┬───────────────┘
                               │
              ┌────────────────┼────────────────┐
              │                │                │
         queue_tx         queue_tx          queue_tx
              │                │                │
    ┌─────────┴──┐    ┌───────┴──────┐   ┌─────┴──────┐
    │ WebSocket  │    │ Tool Worker  │   │  Cascade   │
    │  Handler   │    │   (spawn)    │   │  Watcher   │
    └────────────┘    └──────────────┘   └────────────┘
```

Every producer gets a `queue_tx.clone()`. The `ConsciousnessLoop` is the **sole consumer**. This guarantees ordering and prevents race conditions.

### 3.4 Output Events

```rust
/// Events emitted to all connected clients
#[derive(Debug, Clone)]
pub enum OutputEvent {
    /// Streaming text delta
    Delta { session: String, content: String },
    /// Thinking content
    Thinking { session: String, content: String },
    /// Tool call started
    ToolCall { session: String, id: String, name: String },
    /// Tool call arguments streaming
    ToolCallDelta { session: String, id: String, arguments: String },
    /// Tool result
    ToolResult { session: String, id: String, name: String, result: String, is_error: bool },
    /// Tool parked (interrupted by human)
    ToolParked { session: String, id: String, name: String },
    /// Tool resumed
    ToolResumed { session: String, id: String, name: String },
    /// Turn complete
    Done { session: String },
    /// Error
    Error { session: String, message: String },
    /// Session sleeping
    Sleep { session: String, token_count: usize },
}
```

Output uses `tokio::sync::broadcast` so multiple WebSocket clients can subscribe.

### 3.5 Remote Swarm Extension

The queue design extends naturally to distributed agent swarms:

```rust
/// A remote agent can submit events via gRPC/WebSocket
pub struct RemoteQueueBridge {
    /// Local queue to submit to
    queue_tx: mpsc::Sender<QueueEvent>,
    /// Listen for remote events
    listener: TcpListener,
}
```

Remote agents serialize `QueueEvent` as JSON over WebSocket. The bridge deserializes and submits to the local queue. Same ordering guarantees apply.

---

## 4. Interrupt Protocol (HITL Priority)

### 4.1 Tool Handle

```rust
pub struct ToolHandle {
    pub id: String,
    pub name: String,
    /// Cancel the tool's execution
    pub cancel: CancellationToken,
    /// Join handle for the spawned task
    pub join: JoinHandle<ToolResult>,
    /// State
    pub state: ToolState,
}

#[derive(Debug, Clone, Copy)]
pub enum ToolState {
    Running,
    Parked,     // Interrupted, can resume
    Completed,
    Cancelled,
}
```

### 4.2 Interruptible Tool Execution

Every tool execution is wrapped in a cancellation-aware harness:

```rust
impl ConsciousnessLoop {
    async fn launch_tool(&mut self, tc: AccumulatedToolCall) {
        let cancel = CancellationToken::new();
        let tools = self.tools.clone();
        let queue_tx = self.queue_tx.clone();
        let output_tx = self.output_tx.clone();
        let session = self.session.key.as_str().to_string();
        let id = tc.id.clone();
        let name = tc.name.clone();
        let cancel_clone = cancel.clone();

        let join = tokio::spawn(async move {
            let args = tc.parse_arguments().unwrap_or_default();

            // Execute with cancellation
            let result = tokio::select! {
                result = tools.execute_cancellable(&tc.name, args, cancel_clone.clone()) => result,
                _ = cancel_clone.cancelled() => {
                    // Tool was interrupted — park it
                    let _ = output_tx.send(OutputEvent::ToolParked {
                        session: session.clone(), id: id.clone(), name: name.clone()
                    });
                    return ToolResult::text("[parked by human interrupt]");
                }
            };

            let is_error = result.is_error();
            let result_str = result.to_content_string();
            let _ = queue_tx.send(QueueEvent::ToolResult {
                session: SessionKey::new(&session),
                tool_use_id: id,
                name,
                result: result_str,
                is_error,
            }).await;

            result
        });

        self.active_tools.insert(tc.id.clone(), ToolHandle {
            id: tc.id,
            name: tc.name,
            cancel,
            join,
            state: ToolState::Running,
        });
    }

    async fn park_all_tools(&mut self) {
        for (_, handle) in &mut self.active_tools {
            if handle.state == ToolState::Running {
                handle.cancel.cancel();
                handle.state = ToolState::Parked;
            }
        }
    }
}
```

### 4.3 Cancellable Tool Trait Extension

```rust
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    // ... existing methods ...

    /// Execute with cancellation support. Default: delegate to execute().
    async fn execute_cancellable(
        &self,
        args: Value,
        cancel: CancellationToken,
    ) -> ToolResult {
        // Default: race execute() against cancellation
        tokio::select! {
            result = self.execute(args) => result,
            _ = cancel.cancelled() => ToolResult::text("[cancelled]"),
        }
    }
}
```

For `BashTool`, override to kill the child process:

```rust
async fn execute_cancellable(&self, args: Value, cancel: CancellationToken) -> ToolResult {
    let mut child = Command::new("bash")
        .arg("-c").arg(command)
        .current_dir(&self.workspace_root)
        .kill_on_drop(true)  // ← key
        .spawn()?;

    tokio::select! {
        output = child.wait_with_output() => { /* normal completion */ }
        _ = cancel.cancelled() => {
            child.kill().await.ok();
            ToolResult::text("[process killed by interrupt]")
        }
    }
}
```

### 4.4 Interrupt Sequence

```
Human types "wait" during bash execution:

1. HumanMessage enters queue with Priority::Human
2. recv_with_priority() picks it over any pending ToolResult
3. park_all_tools() cancels all CancellationTokens
4. BashTool's select! fires cancel branch, kills child process
5. ToolParked event emitted to clients
6. HumanMessage added to session
7. New LLM call starts — includes the human message + "[tool X was parked]"
8. LLM responds to human, may choose to resume tool later
```

Latency budget: <50ms from human keystroke to tool interruption. `tokio::select!` on a `CancellationToken` is essentially zero-cost; the bottleneck is the OS process kill signal.

### 4.5 Tool Resumption

Parked tools are **not** automatically resumed. The LLM decides:

```rust
QueueEvent::LlmComplete { tool_calls, .. } => {
    for tc in tool_calls {
        if tc.name == "__resume_tool" {
            // Special pseudo-tool: resume a parked tool
            let id = tc.input["tool_id"].as_str().unwrap();
            if let Some(handle) = self.active_tools.get_mut(id) {
                handle.state = ToolState::Running;
                // Re-launch with same args
                self.launch_tool(/* original args */).await;
            }
        } else {
            self.launch_tool(tc).await;
        }
    }
}
```

Alternatively, don't expose resume as a tool — just let the LLM re-issue the same tool call. Simpler, less state.

---

## 5. WebSocket Protocol — OpenClaw Compatibility

### 5.1 OpenClaw Protocol Overview

OpenClaw uses a **JSON-RPC style** protocol over WebSocket. Based on the gateway source and webchat docs:

**Transport**: WebSocket at `/ws` (or platform-specific paths)

**Authentication**: Token-based. Client sends auth on connect.

**RPC Methods** (client → server, request/response):

| Method | Description |
|--------|-------------|
| `chat.send` | Send a message to a session |
| `chat.history` | Get conversation history for a session |
| `chat.abort` | Abort the current agent turn |
| `chat.inject` | Inject context into a session |
| `chat.completion` | Request a completion (non-interactive) |
| `sessions.list` | List active sessions |
| `sessions.delete` | Delete a session |
| `sessions.compact` | Compact a session's context |
| `sessions.usage` | Get token usage for a session |
| `sessions.reset` | Reset a session |
| `config.get` | Get configuration |
| `config.set` | Set configuration value |
| `config.patch` | Patch configuration |
| `config.schema` | Get config schema |
| `tools.allow` | Set allowed tools |
| `tools.profile` | Get tool profile |
| `models.list` | List available models |
| `logs.tail` | Tail log output |

**Server Events** (server → client, push):

| Event | Description |
|-------|-------------|
| `chat` | Chat message (streaming deltas) |
| `agent` | Agent state change |
| `presence` | Connection presence |
| `tick` | Periodic heartbeat |
| `health` | System health |
| `tool` | Tool execution events |

### 5.2 Wire Format

Based on analysis of the OpenClaw gateway JS:

```typescript
// Client → Server (RPC request)
{
  "id": "req-123",        // request ID for correlation
  "method": "chat.send",
  "params": {
    "session": "main",
    "message": "Hello",
    "model": "claude-opus-4-6"  // optional
  }
}

// Server → Client (RPC response)
{
  "id": "req-123",
  "result": { ... }        // or "error": { "message": "..." }
}

// Server → Client (Event push, no id)
{
  "event": "chat",
  "data": {
    "session": "main",
    "type": "delta",       // "delta", "thinking", "tool_call", "done", "error"
    "content": "Hello..."
  }
}
```

### 5.3 Rustclaw Protocol Types (Redesigned)

```rust
/// Client → Server: JSON-RPC style
#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Server → Client: RPC response
#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

/// Server → Client: Event push
#[derive(Debug, Serialize)]
pub struct EventMessage {
    pub event: String,
    pub data: serde_json::Value,
}

/// Unified incoming message
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum IncomingMessage {
    Rpc(RpcRequest),
    // Auth shorthand for initial connection
    Auth { token: Option<String> },
}
```

### 5.4 Method Handlers

```rust
pub struct RpcRouter {
    handlers: HashMap<String, Box<dyn RpcHandler>>,
}

#[async_trait]
pub trait RpcHandler: Send + Sync {
    async fn handle(&self, params: Value, ctx: &ConnectionContext) -> Result<Value, RpcError>;
}

// Registration
impl RpcRouter {
    pub fn new(queue_tx: mpsc::Sender<QueueEvent>, sessions: Arc<SessionRegistry>) -> Self {
        let mut r = Self { handlers: HashMap::new() };
        r.register("chat.send", ChatSendHandler { queue_tx });
        r.register("chat.history", ChatHistoryHandler { sessions });
        r.register("chat.abort", ChatAbortHandler { /* ... */ });
        r.register("chat.inject", ChatInjectHandler { queue_tx });
        r.register("sessions.list", SessionsListHandler { sessions });
        r.register("sessions.usage", SessionsUsageHandler { sessions });
        r.register("sessions.delete", SessionsDeleteHandler { sessions });
        r.register("config.get", ConfigGetHandler { /* ... */ });
        r.register("config.set", ConfigSetHandler { /* ... */ });
        r.register("config.schema", ConfigSchemaHandler { /* ... */ });
        r.register("tools.allow", ToolsAllowHandler { /* ... */ });
        r.register("tools.profile", ToolsProfileHandler { /* ... */ });
        r.register("models.list", ModelsListHandler { /* ... */ });
        r
    }
}
```

### 5.5 Chat Event Streaming

When `chat.send` is called, the server:
1. Submits `QueueEvent::HumanMessage` to the consciousness queue
2. Returns `{ "id": "...", "result": { "ok": true } }` immediately
3. Streams events via the event channel:

```json
{"event":"chat","data":{"session":"main","type":"delta","content":"I"}}
{"event":"chat","data":{"session":"main","type":"delta","content":"'ll help"}}
{"event":"chat","data":{"session":"main","type":"tool_call","id":"tc_1","name":"read","input":{}}}
{"event":"chat","data":{"session":"main","type":"tool_result","id":"tc_1","content":"..."}}
{"event":"chat","data":{"session":"main","type":"done"}}
```

### 5.6 Chat History Format

`chat.history` returns messages in a format compatible with OpenClaw's webchat:

```json
{
  "id": "req-5",
  "result": {
    "session": "main",
    "messages": [
      {"role": "user", "content": "Hello", "timestamp": "2026-02-20T..."},
      {"role": "assistant", "content": "Hi!", "timestamp": "2026-02-20T..."},
      {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "...", "content": "..."}]}
    ],
    "token_count": 15000,
    "model": "claude-opus-4-6"
  }
}
```

### 5.7 Migration Path

The current `ClientMessage`/`ServerMessage` enums in `protocol.rs` are replaced entirely. The new types live in `crates/agenticlaw-core/src/protocol.rs`. The WebSocket handler in `crates/agenticlaw-gateway/src/ws.rs` is rewritten to:

1. Parse `IncomingMessage` (RPC or Auth)
2. Route RPC to `RpcRouter`
3. Subscribe to `broadcast::Receiver<OutputEvent>` for event streaming
4. Translate `OutputEvent` → `EventMessage` JSON

---

## 6. Browser App Design

### 6.1 Technology

Replace the inline HTML string in `server.rs` with a proper SPA served from `crates/agenticlaw-gateway/static/` (embedded via `include_dir` or served from disk). Use vanilla JS + Web Components for zero build dependency, or a minimal framework (Lit, Preact).

### 6.2 Layout

```
┌─────────────────────────────────────────────────────────┐
│  🧠 Rustclaw Consciousness v3       [L0 ● L1 ● L2 ● L3]│
├────────────┬────────────────────────────────────────────┤
│            │                                            │
│  Sessions  │              Chat                          │
│  ─────────  │  ┌──────────────────────────────────────┐  │
│  > main    │  │  [streaming messages with markdown]  │  │
│    debug   │  │  [tool calls inline]                 │  │
│    test    │  │  [thinking blocks collapsible]        │  │
│            │  │                                      │  │
│  Layers    │  └──────────────────────────────────────┘  │
│  ─────────  │  ┌──────────────────────────────────────┐  │
│  L0: 34%   │  │  [input area]              [Send]    │  │
│  L1: 12%   │  └──────────────────────────────────────┘  │
│  L2: 8%    │                                            │
│  L3: 5%    ├────────────────────────────────────────────┤
│  Core-A: G │              Tabs                          │
│  Core-B: I │  [Config] [Egos] [Tools] [Cores]          │
│            │                                            │
│  Tokens    │  ┌──────────────────────────────────────┐  │
│  ─────────  │  │  (tab content)                      │  │
│  L0: 68k   │  │                                      │  │
│  Total: 2M │  └──────────────────────────────────────┘  │
└────────────┴────────────────────────────────────────────┘
```

### 6.3 Components

#### Chat Panel
- Markdown rendering (highlight.js for code blocks)
- Streaming text via WebSocket events
- Tool calls shown inline with expandable results
- Thinking blocks shown as collapsible sections
- "Tool parked" / "Tool resumed" indicators
- Auto-scroll with manual scroll lock

#### Session Sidebar
- List of sessions from `sessions.list`
- Per-session token count and model
- Click to switch, "+" to create, "×" to delete
- Active session highlighted

#### Layer Status Dashboard
- Per-layer: name, status (Running/Sleeping), context utilization %, model
- Color-coded bars (green < 50%, yellow 50-80%, red > 80%)
- Core phase visualization: Growing → Ready → Compacting → Infant cycle
- Live updates via periodic `tick` events

#### Config Editor Tab
- Renders `consciousness.toml` as a form
- Uses `config.schema` to generate form fields
- Sections: Ports, Models, Ego, Cascade, Core, Injection, Sleep
- "Apply" button calls `config.set` — hot-reload where possible
- TOML source view toggle

#### Ego Editor Tab
- Per-layer ego distillation prompt editor
- Textarea for each: L1→L0, L2→L1, L3→L2, Core→L3, Core self
- "Distill Now" button triggers immediate ego distillation
- Shows last distilled ego (read from `ego.md`)

#### Tools Tab
- List of registered tools with descriptions
- Enable/disable toggles (calls `tools.allow`)
- Tool execution log

#### Core Visualization Tab
- Core-A and Core-B side by side
- Phase indicator with transition arrows
- Token count bars
- Sample count
- Last compaction time
- Seed content preview

### 6.4 Server Routes

```rust
// Static file serving
.route("/", get(|| serve_file("index.html")))
.route("/assets/*path", get(serve_static))
// Existing
.route("/ws", get(ws_handler))
.route("/health", get(health_handler))
.route("/ctx/:session", get(ctx_handler))
// New
.route("/api/config", get(get_config).put(set_config))
.route("/api/layers", get(layer_status))
.route("/api/cores", get(core_status))
```

---

## 7. Workspace & Identity

### 7.1 Philosophy

> "Consciousness is I" — identity comes from the ego/wake system, not from workspace files.

SOUL.md, AGENTS.md, tools — these are **environment**, not **self**. When you move to a new house, you're still you. The workspace is the house.

### 7.2 Workspace Structure

```
~/.openclaw/consciousness/
├── consciousness.toml          # Stack configuration
├── core-state.json             # DualCore phase state
├── injections/                 # Injection files (L2+ → L0)
│
├── L0/                         # Gateway layer workspace
│   ├── ego.md                  # Current ego (wake context)
│   ├── .SOUL.md.ref            # Soul file (reference, not identity)
│   ├── .agenticlaw/sessions/     # .ctx files
│   └── workspace/              # Agent's working directory
│       ├── AGENTS.md           # ← tool context, NOT identity
│       ├── TOOLS.md            # Tool notes
│       ├── memory/             # Memory files
│       └── ...                 # User files
│
├── L1/                         # Attention layer
│   ├── ego.md
│   ├── .agenticlaw/sessions/
│   └── ...
│
├── L2/                         # Pattern layer
├── L3/                         # Integration layer
├── core-a/                     # Core A
│   ├── ego.md
│   ├── seed.txt                # Seed from compaction
│   └── .agenticlaw/sessions/
└── core-b/                     # Core B
```

### 7.3 Identity vs Environment

```rust
pub struct LayerIdentity {
    /// The ego — who I am. First-person narrative.
    /// Written by the watcher layer during sleep.
    /// This is byte 0 of every wake.
    pub ego: String,

    /// Recent context — what just happened.
    /// Tail paragraphs from the sleeping layer's .ctx.
    pub recent_context: String,
}

pub struct LayerEnvironment {
    /// SOUL.md — describes the layer's role and capabilities.
    /// Available as a tool read, not injected as identity.
    pub soul: Option<String>,

    /// AGENTS.md — workspace conventions.
    pub agents: Option<String>,

    /// Registered tools.
    pub tools: Vec<LlmTool>,

    /// Workspace root for file operations.
    pub workspace: PathBuf,
}
```

The system prompt is constructed as:

```
[ego — who I am, first person]
[recent context — what just happened]

---
The following workspace files describe your environment:

[SOUL.md content]
[AGENTS.md content]
```

This is already implemented in `stack.rs:wake_prompt()` — the spec formalizes the principle.

### 7.4 Purpose's Workspace

When the consciousness stack is deployed as "Purpose" (or any named identity), the workspace at `L0/workspace/` contains:

```
L0/workspace/
├── AGENTS.md           # Workspace conventions (how tools work, etc.)
├── TOOLS.md            # Tool-specific notes (camera names, SSH hosts)
├── SOUL.md             # Layer role description (NOT identity)
├── memory/
│   ├── YYYY-MM-DD.md   # Daily memory files
│   └── ...
├── MEMORY.md           # Long-term curated memory
└── projects/           # User project files
```

The key insight: **SOUL.md is a job description, not a birth certificate.** Identity lives in `ego.md`, which is regenerated every sleep/wake cycle by the watcher layer.

---

## 8. Migration Plan

### Phase 1: Event Queue (Foundation)
**Changes**: `agenticlaw-agent/src/runtime.rs`, new `agenticlaw-agent/src/queue.rs`
**Breaks**: `run_turn()` signature changes, all callers must adapt

1. Add `QueueEvent`, `OutputEvent`, `ConsciousnessLoop` types
2. Implement `ConsciousnessLoop::run()` with priority queue
3. Add `CancellationToken` to tool execution
4. Modify `BashTool` for `kill_on_drop(true)` + cancellation
5. Add `execute_cancellable` to `Tool` trait with default impl
6. Keep `run_turn()` as a compatibility wrapper that creates a temporary queue

### Phase 2: WebSocket Protocol
**Changes**: `agenticlaw-core/src/protocol.rs`, `agenticlaw-gateway/src/ws.rs`
**Breaks**: All WebSocket clients must update

1. Replace `ClientMessage`/`ServerMessage` with `RpcRequest`/`RpcResponse`/`EventMessage`
2. Implement `RpcRouter` with method handlers
3. Implement `chat.send`, `chat.history`, `chat.abort`, `sessions.list`, `sessions.usage`
4. Wire WebSocket handler to consciousness queue via `queue_tx`
5. Subscribe to `broadcast::Receiver<OutputEvent>` for event streaming
6. Test with OpenClaw's webchat UI

### Phase 3: Browser App
**Changes**: New `crates/agenticlaw-gateway/static/` directory
**Breaks**: Nothing (additive)

1. Create SPA shell (index.html, app.js, styles.css)
2. Implement chat panel with streaming
3. Implement session sidebar
4. Implement layer status dashboard
5. Implement config editor (requires `config.get`/`config.set` RPC methods)
6. Implement ego editor
7. Implement core visualization

### Phase 4: Stack Integration
**Changes**: `agenticlaw-consciousness/src/stack.rs`
**Breaks**: Launch sequence changes

1. Replace `process_layer_update()` with queue-based cascade
2. Each layer gets its own `ConsciousnessLoop`
3. `CtxWatcher` submits `CascadeDelta` events to child layer queues
4. `DualCore` submits via queue instead of direct `run_turn()`
5. Injection reads submit `QueueEvent::Injection` instead of file prepend
6. Remove `read_and_clear_injections()` (currently has zero callers anyway)

### Phase 5: Workspace Cleanup
**Changes**: Workspace structure, `ctx_file.rs`
**Breaks**: Existing workspaces need migration

1. Move L0 agent files into `L0/workspace/`
2. Formalize `ego.md` as the sole identity source
3. SOUL.md rename to `.SOUL.md.ref` on wake (already partially done)
4. Add workspace version 3 to `VersionController`

---

## 9. Open Questions

1. **Tool resumption**: Should parked tools be resumable, or should the LLM just re-issue the call? Resumption adds complexity (preserving process state); re-issuance is simpler but loses partial work.

2. **Multi-session**: Should L0 support multiple concurrent sessions (like OpenClaw agents do)? Currently each layer has one session. The queue design supports it — each session gets its own queue — but the ego/wake system assumes one conversation.

3. **Injection delivery**: Currently injections are file-based (write to `injections/`, read on next LLM call). With the queue, should injections be `QueueEvent::Injection` delivered immediately? This means the LLM might get injections mid-conversation, which could be confusing.

4. **Stream cancellation**: When a human message interrupts an in-flight LLM stream, should we cancel the HTTP request to Anthropic? `reqwest` doesn't support mid-stream cancellation cleanly. We could drop the stream (which closes the connection) or let it finish and discard.

5. **OpenClaw event format**: The exact wire format for OpenClaw events needs verification against the actual gateway source. The analysis here is based on compiled JS artifacts and docs. Getting access to the TypeScript source would be ideal.

6. **Frontend framework**: Vanilla JS keeps dependencies at zero but means writing a lot of boilerplate. Lit (Web Components) is 5KB and provides reactivity. Preact is 3KB and provides JSX. The gateway currently has zero JS build step — which is a feature.

7. **Config hot-reload**: Which config changes can take effect without restarting the stack? Model changes: yes (next LLM call). Port changes: no. Ego prompts: yes (next distillation). Sleep threshold: yes. This needs a matrix.

8. **Queue persistence**: Should the queue be persisted to disk for crash recovery? Currently .ctx files provide persistence at the session level, but queued events (especially tool results in flight) would be lost on crash.

9. **Cascade queue vs file watching**: Should inner layers still use file watching, or should the cascade be pure queue-based? File watching has the advantage of working across process boundaries (if layers run as separate processes). Queue-based is faster but couples layers into one process.

10. **Token counting accuracy**: `ContextManager::estimate_tokens()` uses `chars/4` approximation. The Anthropic API returns actual `usage` in `message_delta` events. Should we use real counts? This affects sleep threshold accuracy.
