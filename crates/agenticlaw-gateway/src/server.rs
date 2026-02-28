//! Gateway server with full agent runtime, broadcast output, and .ctx serving

use crate::auth::ResolvedAuth;
use crate::ws::{handle_connection, WsState};
use agenticlaw_agent::{AgentConfig, AgentRuntime, OutputEvent, SessionKey};
use agenticlaw_core::GatewayConfig;
use agenticlaw_tools::create_default_registry;
use axum::{
    extract::{Path as AxumPath, State, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

pub struct ExtendedConfig {
    pub gateway: GatewayConfig,
    pub anthropic_api_key: Option<String>,
    pub workspace_root: PathBuf,
    pub system_prompt: Option<String>,
}

impl Default for ExtendedConfig {
    fn default() -> Self {
        Self {
            gateway: GatewayConfig::default(),
            anthropic_api_key: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            system_prompt: None,
        }
    }
}

pub async fn start_gateway(config: ExtendedConfig) -> anyhow::Result<()> {
    let env_token = std::env::var("RUSTCLAW_GATEWAY_TOKEN")
        .or_else(|_| std::env::var("OPENCLAW_GATEWAY_TOKEN"))
        .ok();
    let auth = ResolvedAuth::from_config(&config.gateway.auth, env_token);

    let api_key = config
        .anthropic_api_key
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        .ok_or_else(|| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let layer = std::env::var("RUSTCLAW_LAYER")
        .or_else(|_| std::env::var("OPENCLAW_LAYER"))
        .ok();

    let tools = create_default_registry(&config.workspace_root);
    info!("Registered tools: {:?}", tools.list());

    let agent_config = AgentConfig {
        default_model: std::env::var("RUSTCLAW_MODEL")
            .or_else(|_| std::env::var("OPENCLAW_MODEL"))
            .unwrap_or_else(|_| "claude-opus-4-6-20250929".to_string()),
        max_tool_iterations: 25,
        system_prompt: config
            .system_prompt
            .or_else(|| std::env::var("RUSTCLAW_SYSTEM_PROMPT").ok()),
        workspace_root: config.workspace_root.clone(),
        sleep_threshold_pct: 1.0,
    };

    // If ANTHROPIC_API_URL is set, use it as the base URL (for protectgateway proxy)
    let agent = if let Ok(api_url) = std::env::var("ANTHROPIC_API_URL") {
        let provider = agenticlaw_llm::AnthropicProvider::new(&api_key)
            .with_base_url(format!("{}/v1/messages", api_url));
        info!("Using custom API URL: {}/v1/messages", api_url);
        Arc::new(AgentRuntime::with_provider(
            Arc::new(provider),
            tools,
            agent_config,
        ))
    } else {
        Arc::new(AgentRuntime::new(&api_key, tools, agent_config))
    };

    // Create broadcast channel for OutputEvents — fan-out to all WS clients
    let (output_tx, _) = broadcast::channel::<OutputEvent>(1024);

    let state = Arc::new(WsState {
        auth,
        agent,
        layer: layer.clone(),
        port: config.gateway.port,
        output_tx,
        consciousness_enabled: false,
        started_at: std::time::Instant::now(),
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .route("/surface", get(surface_handler))
        .route("/plan", post(plan_handler))
        .route("/test", post(test_handler))
        .route("/hints", get(hints_handler))
        .route("/ctx/{session}", get(ctx_handler))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any))
        .with_state(state);

    let bind_addr: SocketAddr =
        format!("{}:{}", config.gateway.bind.to_addr(), config.gateway.port)
            .parse()
            .expect("invalid bind address");

    info!("Rustclaw Gateway v{} starting", env!("CARGO_PKG_VERSION"));
    info!("  Listening on: {}", bind_addr);
    info!("  WebSocket: ws://{}/ws", bind_addr);
    info!("  Context:   http://{}/ctx/{{session}}", bind_addr);
    info!("  Auth mode: {:?}", config.gateway.auth.mode);
    info!("  Workspace: {:?}", config.workspace_root);
    if let Some(layer) = &layer {
        info!("  Layer: {}", layer);
    }

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<WsState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_connection(socket, state))
}

async fn health_handler(State(state): State<Arc<WsState>>) -> impl IntoResponse {
    serde_json::json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
        "layer": state.layer,
        "sessions": state.agent.sessions().list().len(),
        "tools": state.agent.tool_definitions().len(),
    })
    .to_string()
}

/// Serve the raw .ctx file for a session — the entire conversation visible at all times.
async fn ctx_handler(
    AxumPath(session): AxumPath<String>,
    State(state): State<Arc<WsState>>,
) -> impl IntoResponse {
    let session_key = SessionKey::new(&session);
    match state.agent.sessions().get(&session_key) {
        Some(sess) => match sess.read_ctx() {
            Some(content) => (
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                content,
            )
                .into_response(),
            None => (
                axum::http::StatusCode::NOT_FOUND,
                "Session exists but no .ctx file found",
            )
                .into_response(),
        },
        None => (
            axum::http::StatusCode::NOT_FOUND,
            format!("Session '{}' not found", session),
        )
            .into_response(),
    }
}

/// Sacred endpoint: /surface — bee manifest
async fn surface_handler(State(state): State<Arc<WsState>>) -> impl IntoResponse {
    let tools: Vec<String> = state
        .agent
        .tool_definitions()
        .into_iter()
        .map(|t| t.name)
        .collect();
    Json(serde_json::json!({
        "name": "agenticlaw",
        "version": env!("CARGO_PKG_VERSION"),
        "type": "runtime",
        "provides": [
            "runtime.agenticlaw",
            "runtime.gateway",
            "runtime.consciousness",
            "agent.chat",
            "agent.tools",
            "agent.sessions",
            "agent.spawn",
            "ws.json-rpc-v3",
            "ws.legacy-v2",
            "ctx.persistence",
            "consciousness.dual-core",
            "consciousness.ego-distill",
            "consciousness.injection",
            "consciousness.sleep-wake"
        ],
        "requires": ["runtime.rust"],
        "tools": tools,
        "sacred": {
            "health": "/health",
            "surface": "/surface",
            "plan": "/plan",
            "test": "/test",
            "hints": "/hints"
        }
    }))
}

/// Sacred endpoint: /plan — compatibility check
async fn plan_handler(
    State(_state): State<Arc<WsState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let provides: Vec<&str> = vec![
        "runtime.agenticlaw",
        "runtime.gateway",
        "runtime.consciousness",
        "agent.chat",
        "agent.tools",
        "agent.sessions",
        "agent.spawn",
        "ws.json-rpc-v3",
        "ws.legacy-v2",
        "ctx.persistence",
        "consciousness.dual-core",
        "consciousness.ego-distill",
        "consciousness.injection",
        "consciousness.sleep-wake",
    ];

    let requested = body["requires"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();

    let missing: Vec<&str> = requested
        .iter()
        .filter(|r| !provides.contains(r))
        .copied()
        .collect();

    let compatible = missing.is_empty();

    Json(serde_json::json!({
        "compatible": compatible,
        "missing": missing,
        "provides": provides,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Sacred endpoint: /test — self-test
async fn test_handler(State(state): State<Arc<WsState>>) -> impl IntoResponse {
    let mut results = Vec::new();

    // Test 1: Tool registry loaded
    let tool_count = state.agent.tool_definitions().len();
    results.push(serde_json::json!({
        "test": "tool_registry",
        "pass": tool_count > 0,
        "detail": format!("{} tools loaded", tool_count)
    }));

    // Test 2: Sessions accessible
    let session_count = state.agent.sessions().list().len();
    results.push(serde_json::json!({
        "test": "sessions",
        "pass": true,
        "detail": format!("{} active sessions", session_count)
    }));

    // Test 3: API key present (don't test connectivity — that's expensive)
    let has_key = std::env::var("ANTHROPIC_API_KEY").is_ok();
    results.push(serde_json::json!({
        "test": "api_key",
        "pass": has_key,
        "detail": if has_key { "ANTHROPIC_API_KEY set" } else { "ANTHROPIC_API_KEY missing" }
    }));

    let all_pass = results.iter().all(|r| r["pass"].as_bool() == Some(true));

    Json(serde_json::json!({
        "pass": all_pass,
        "results": results,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Sacred endpoint: /hints — integration guidance
async fn hints_handler(State(_state): State<Arc<WsState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "usage": {
            "chat": "Connect via WebSocket to /ws, authenticate with {\"token\": \"...\"}, then send {\"id\": \"1\", \"method\": \"chat.send\", \"params\": {\"session\": \"name\", \"message\": \"hello\"}}",
            "health": "GET /health for status",
            "ctx": "GET /ctx/{session} for raw conversation context",
        },
        "related_bees": [
            { "name": "protectgateway", "role": "Transparent security proxy, sits in front of agenticlaw" },
            { "name": "operator", "role": "Container builder with policy-scoped tool access" },
            { "name": "beectl", "role": "Service lifecycle management" }
        ],
        "notes": [
            "Agenticlaw uses .ctx files, not JSONL, for session persistence",
            "Consciousness stack is enabled by default; use --no-consciousness for lightweight mode",
            "When protectgateway is installed, agenticlaw moves to port 18790"
        ]
    }))
}

async fn index_handler(State(state): State<Arc<WsState>>) -> Html<String> {
    let tools: Vec<String> = state
        .agent
        .tool_definitions()
        .into_iter()
        .map(|t| t.name)
        .collect();
    let sessions: Vec<String> = state
        .agent
        .sessions()
        .list()
        .into_iter()
        .map(|k| k.as_str().to_string())
        .collect();
    let session_links = if sessions.is_empty() {
        "<em>No active sessions. Send a chat message to create one.</em>".to_string()
    } else {
        sessions
            .iter()
            .map(|s| format!("<li><a href=\"/ctx/{}\">{}</a></li>", s, s))
            .collect::<Vec<_>>()
            .join("\n")
    };

    Html(format!(
        r#"<!DOCTYPE html><html><head><title>Rustclaw Gateway</title>
<style>
body {{ font-family: monospace; background: #1a1a2e; color: #eee; padding: 20px; max-width: 900px; margin: 0 auto; }}
h1 {{ color: #f39c12; }} h2 {{ color: #3498db; }}
a {{ color: #3498db; }} code {{ background: #0f3460; padding: 2px 6px; border-radius: 4px; }}
.info {{ background: #16213e; padding: 15px; border-radius: 8px; margin: 15px 0; }}
#output {{ background: #0f3460; padding: 15px; border-radius: 8px; min-height: 200px; max-height: 400px; overflow-y: auto; white-space: pre-wrap; font-size: 13px; }}
textarea {{ width: 100%; min-height: 60px; background: #0f3460; color: #eee; border: 1px solid #333; border-radius: 4px; padding: 10px; font-size: 14px; resize: vertical; }}
button {{ background: #f39c12; border: none; padding: 8px 16px; border-radius: 4px; cursor: pointer; font-size: 14px; margin: 5px 5px 5px 0; }}
button:hover {{ background: #e67e22; }}
.tool {{ color: #3498db; }} .error {{ color: #e74c3c; }}
</style></head><body>
<h1>Rustclaw Gateway v{version}</h1>
<div class="info">
<p>WebSocket: <code>ws://localhost:{port}/ws</code></p>
<p>Protocol: v3 JSON-RPC (with v2 legacy fallback)</p>
<p>Tools: {tools}</p>
<p>Workspace: <code>{workspace}</code></p>
</div>
<h2>Sessions</h2>
<ul>{session_links}</ul>
<h2>Chat</h2>
<div>
<textarea id="msg" placeholder="Type a message..."></textarea>
<button onclick="send()">Send</button>
<button onclick="document.getElementById('output').textContent=''">Clear</button>
</div>
<div id="output"></div>
<script>
let ws = null;
let reqId = 0;
function init() {{
    ws = new WebSocket('ws://'+location.host+'/ws');
    ws.onopen = () => {{ ws.send(JSON.stringify({{token: null}})); }};
    ws.onmessage = (e) => {{
        const d = JSON.parse(e.data);
        const out = document.getElementById('output');
        if (d.event === 'chat') {{
            const t = d.data.type;
            if (t === 'delta') out.textContent += d.data.content;
            else if (t === 'tool_call') out.textContent += '\n[tool:'+d.data.name+']\n';
            else if (t === 'tool_result') out.textContent += '[result: '+d.data.content.length+' chars]\n';
            else if (t === 'done') out.textContent += '\n--- done ---\n\n';
            else if (t === 'error') out.textContent += '\nERROR: '+d.data.message+'\n';
        }} else if (d.event === 'auth') {{
            if (d.data.ok) out.textContent += '[authenticated]\n';
            else out.textContent += '[auth failed: '+(d.data.error||'unknown')+']\n';
        }}
        out.scrollTop = out.scrollHeight;
    }};
    ws.onclose = () => {{ setTimeout(init, 1000); }};
}}
function send() {{
    if (!ws || ws.readyState !== 1) return;
    const msg = document.getElementById('msg').value;
    if (!msg.trim()) return;
    const out = document.getElementById('output');
    out.textContent += '\n>>> ' + msg + '\n\n';
    reqId++;
    ws.send(JSON.stringify({{id:'req-'+reqId, method:'chat.send', params:{{session:'web-console', message:msg}}}}));
    document.getElementById('msg').value = '';
}}
document.getElementById('msg').addEventListener('keydown', (e) => {{
    if (e.key==='Enter' && !e.shiftKey) {{ e.preventDefault(); send(); }}
}});
init();
</script></body></html>"#,
        version = env!("CARGO_PKG_VERSION"),
        port = state.port,
        tools = tools.join(", "),
        workspace = state.agent.workspace().display(),
        session_links = session_links,
    ))
}
