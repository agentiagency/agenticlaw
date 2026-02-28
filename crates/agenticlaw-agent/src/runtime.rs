//! Agent runtime — the core agentic loop.
//!
//! Modeled after OpenClaw's pi-agent-core agent-loop but improved:
//! - Steering queue: HITL interrupts mid-tool, skips remaining tools
//! - Follow-up queue: messages processed after agent would normally stop
//! - CancellationToken: proper abort propagation to LLM streams
//! - Concurrent tool execution with per-tool cancellation
//! - .ctx persistence built into the loop
//! - Sleep/wake architecture for context management

use crate::session::{Session, SessionKey, SessionRegistry};
use agenticlaw_llm::{
    AccumulatedToolCall, AnthropicProvider, ContentBlock, LlmProvider, LlmRequest, LlmTool,
    StreamDelta,
};
use agenticlaw_tools::SpawnableRuntime;
use agenticlaw_tools::ToolRegistry;
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

// ── Events ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// Agent loop started
    AgentStart,
    /// New turn (LLM call) starting
    TurnStart { turn: usize },
    /// Streaming text from assistant
    Text(String),
    /// Streaming thinking from assistant
    Thinking(String),
    /// Tool call announced by LLM
    ToolCallStart { id: String, name: String },
    /// Streaming tool call arguments
    ToolCallDelta { id: String, arguments: String },
    /// Tool execution beginning
    ToolExecuting { id: String, name: String },
    /// Tool execution complete
    ToolResult {
        id: String,
        name: String,
        result: String,
        is_error: bool,
    },
    /// Tool skipped due to steering interrupt
    ToolSkipped { id: String, name: String },
    /// Steering message injected mid-turn
    SteeringInjected { message_count: usize },
    /// Follow-up message processed after turn
    FollowUpInjected { message_count: usize },
    /// Layer hit context limit — should sleep
    Sleep { token_count: usize },
    /// Turn completed
    TurnEnd {
        turn: usize,
        stop_reason: String,
        has_tool_calls: bool,
    },
    /// Agent loop finished
    Done { stop_reason: String },
    /// Error
    Error(String),
    /// Agent was aborted
    Aborted,
}

// ── Config ──────────────────────────────────────────────────────────────

pub struct AgentConfig {
    pub default_model: String,
    pub max_tool_iterations: usize,
    pub system_prompt: Option<String>,
    pub workspace_root: PathBuf,
    pub sleep_threshold_pct: f64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_model: "claude-sonnet-4-20250514".to_string(),
            max_tool_iterations: 25,
            system_prompt: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            sleep_threshold_pct: 0.55,
        }
    }
}

// ── Message Queues ──────────────────────────────────────────────────────

/// Priority message queues for the agent loop.
/// Steering = interrupt now, skip remaining tools.
/// FollowUp = process after agent would normally stop.
#[derive(Default)]
struct MessageQueues {
    steering: Vec<String>,
    follow_up: Vec<String>,
}

#[allow(dead_code)]
impl MessageQueues {
    fn drain_steering(&mut self) -> Vec<String> {
        std::mem::take(&mut self.steering)
    }

    fn drain_follow_up(&mut self) -> Vec<String> {
        std::mem::take(&mut self.follow_up)
    }

    fn has_steering(&self) -> bool {
        !self.steering.is_empty()
    }

    fn has_follow_up(&self) -> bool {
        !self.follow_up.is_empty()
    }

    fn has_any(&self) -> bool {
        self.has_steering() || self.has_follow_up()
    }
}

// ── Runtime ─────────────────────────────────────────────────────────────

pub struct AgentRuntime {
    provider: Arc<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    sessions: Arc<SessionRegistry>,
    config: AgentConfig,
    /// Shared message queues for HITL injection
    queues: Arc<Mutex<MessageQueues>>,
    /// Cancellation token for aborting the current run.
    /// Wrapped in a Mutex so we can replace it for each new run.
    cancel: Arc<Mutex<CancellationToken>>,
}

impl AgentRuntime {
    pub fn new(api_key: &str, tools: ToolRegistry, config: AgentConfig) -> Self {
        Self {
            provider: Arc::new(AnthropicProvider::new(api_key)),
            tools: Arc::new(tools),
            sessions: Arc::new(SessionRegistry::new()),
            config,
            queues: Arc::new(Mutex::new(MessageQueues::default())),
            cancel: Arc::new(Mutex::new(CancellationToken::new())),
        }
    }

    pub fn with_provider(
        provider: Arc<dyn LlmProvider>,
        tools: ToolRegistry,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            tools: Arc::new(tools),
            sessions: Arc::new(SessionRegistry::new()),
            config,
            queues: Arc::new(Mutex::new(MessageQueues::default())),
            cancel: Arc::new(Mutex::new(CancellationToken::new())),
        }
    }

    pub fn sessions(&self) -> &Arc<SessionRegistry> {
        &self.sessions
    }
    pub fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.provider
    }
    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }
    pub fn tool_definitions(&self) -> Vec<LlmTool> {
        self.tools.get_definitions()
    }
    pub fn workspace(&self) -> &Path {
        &self.config.workspace_root
    }
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Queue a steering message — interrupts mid-tool, skips remaining tools.
    /// This is the HITL priority lane. Always processed first.
    pub async fn steer(&self, message: String) {
        self.queues.lock().await.steering.push(message);
    }

    /// Queue a follow-up message — processed after agent would normally stop.
    pub async fn follow_up(&self, message: String) {
        self.queues.lock().await.follow_up.push(message);
    }

    /// Abort the current agent run. In-flight LLM calls are cancelled
    /// and the underlying HTTP stream is dropped immediately.
    pub async fn abort(&self) {
        self.cancel.lock().await.cancel();
    }

    /// Get a clone of the current cancellation token for passing to providers.
    async fn cancel_token(&self) -> CancellationToken {
        self.cancel.lock().await.clone()
    }

    /// Reset the cancellation token for a new run.
    async fn reset_cancel(&self) {
        *self.cancel.lock().await = CancellationToken::new();
    }

    fn get_session(&self, session_key: &SessionKey) -> Arc<Session> {
        self.sessions.create_with_ctx(
            session_key,
            self.config.system_prompt.as_deref(),
            &self.config.workspace_root,
        )
    }

    /// Run the full agentic loop.
    ///
    /// Architecture (mirrors OpenClaw agent-loop but better):
    ///
    /// ```text
    /// OUTER LOOP (follow-up continuation):
    ///   INNER LOOP (tool calls + steering):
    ///     1. Check steering queue → inject as user messages
    ///     2. Stream LLM response
    ///     3. If tool calls:
    ///        a. Execute tools concurrently
    ///        b. Between each tool, check steering → skip remaining on interrupt
    ///        c. Add results to session
    ///        d. Continue inner loop
    ///     4. If no tool calls:
    ///        a. Check for pending HITL input → continue if found
    ///        b. Otherwise exit inner loop
    ///   Check follow-up queue → continue outer loop if found
    ///   Otherwise: done.
    /// ```
    pub async fn run_turn(
        &self,
        session_key: &SessionKey,
        user_message: &str,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<(), String> {
        // Reset cancellation token for this run
        self.reset_cancel().await;

        let session = self.get_session(session_key);
        let max_context = 200_000; // TODO: get from provider/model

        // Add the initial user message
        let should_sleep = session
            .add_user_message(user_message, self.config.sleep_threshold_pct, max_context)
            .await;

        if should_sleep {
            let token_count = session.token_count().await;
            let _ = event_tx.send(AgentEvent::Sleep { token_count }).await;
            return Ok(());
        }

        let _ = event_tx.send(AgentEvent::AgentStart).await;

        let mut turn = 0;

        // Check for steering at start (user may have typed while we were starting)
        let mut pending_steering = self.queues.lock().await.drain_steering();

        // ═══ OUTER LOOP: follow-up continuation ═══
        loop {
            let mut has_more_tool_calls = true;

            // ═══ INNER LOOP: tool calls + steering ═══
            while has_more_tool_calls || !pending_steering.is_empty() {
                turn += 1;
                if turn > self.config.max_tool_iterations {
                    let _ = event_tx
                        .send(AgentEvent::Error("Max tool iterations exceeded".into()))
                        .await;
                    break;
                }

                let _ = event_tx.send(AgentEvent::TurnStart { turn }).await;

                // Inject pending steering messages before LLM call
                if !pending_steering.is_empty() {
                    let count = pending_steering.len();
                    for msg in pending_steering.drain(..) {
                        session
                            .add_user_message(&msg, self.config.sleep_threshold_pct, max_context)
                            .await;
                    }
                    let _ = event_tx
                        .send(AgentEvent::SteeringInjected {
                            message_count: count,
                        })
                        .await;
                }

                // Drain pending input counter
                session.drain_pending_input();

                // Stream LLM response
                let (text_content, tool_calls, stop_reason) =
                    match self.stream_llm_response(&session, &event_tx).await {
                        Ok(result) => result,
                        Err(e) => {
                            let _ = event_tx.send(AgentEvent::Error(e.clone())).await;
                            return Err(e);
                        }
                    };

                // Check for abort
                if self.cancel_token().await.is_cancelled() {
                    let _ = event_tx.send(AgentEvent::Aborted).await;
                    return Ok(());
                }

                // Save assistant response to session
                if tool_calls.is_empty() {
                    session.add_assistant_text(&text_content).await;
                } else {
                    let blocks: Vec<ContentBlock> = tool_calls
                        .iter()
                        .map(|tc| ContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input: tc.parse_arguments().unwrap_or_default(),
                        })
                        .collect();
                    session
                        .add_assistant_with_tools(
                            if text_content.is_empty() {
                                None
                            } else {
                                Some(&text_content)
                            },
                            blocks,
                        )
                        .await;
                }

                has_more_tool_calls = !tool_calls.is_empty();

                if has_more_tool_calls {
                    // Execute tools with steering-aware interruption
                    let steering_after = self
                        .execute_tools_with_steering(&session, &tool_calls, &event_tx)
                        .await;

                    if let Some(steering) = steering_after {
                        pending_steering = steering;
                    } else {
                        pending_steering = self.queues.lock().await.drain_steering();
                    }
                } else {
                    // No tool calls — check for pending HITL input
                    if session.has_pending_input() {
                        let pending = session.drain_pending_input();
                        info!(
                            pending_messages = pending,
                            "HITL input during turn, continuing"
                        );
                        has_more_tool_calls = true; // force inner loop continuation
                        pending_steering = self.queues.lock().await.drain_steering();
                    } else {
                        pending_steering = self.queues.lock().await.drain_steering();
                    }
                }

                let _ = event_tx
                    .send(AgentEvent::TurnEnd {
                        turn,
                        stop_reason: stop_reason.clone(),
                        has_tool_calls: has_more_tool_calls,
                    })
                    .await;
            }

            // Agent would stop here. Check for follow-up messages.
            let follow_ups = self.queues.lock().await.drain_follow_up();
            if !follow_ups.is_empty() {
                let count = follow_ups.len();
                for msg in follow_ups {
                    session
                        .add_user_message(&msg, self.config.sleep_threshold_pct, max_context)
                        .await;
                }
                let _ = event_tx
                    .send(AgentEvent::FollowUpInjected {
                        message_count: count,
                    })
                    .await;
                // Reset for next outer iteration
                pending_steering = self.queues.lock().await.drain_steering();
                continue;
            }

            // Also check the session's own pending counter (from direct add_user_message calls)
            if session.has_pending_input() {
                let pending = session.drain_pending_input();
                info!(
                    pending_messages = pending,
                    "Follow-up HITL input detected, continuing outer loop"
                );
                pending_steering = self.queues.lock().await.drain_steering();
                continue;
            }

            // Truly done
            break;
        }

        let _ = event_tx
            .send(AgentEvent::Done {
                stop_reason: "end_turn".into(),
            })
            .await;

        let msg_count = session.message_count().await;
        let token_count = session.token_count().await;
        info!(
            session = %session_key,
            messages = msg_count,
            tokens = token_count,
            turns = turn,
            "Agent loop complete"
        );
        Ok(())
    }

    /// Stream a single LLM response. Returns (text, tool_calls, stop_reason).
    async fn stream_llm_response(
        &self,
        session: &Session,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<(String, Vec<AccumulatedToolCall>, String), String> {
        let messages = session.get_messages().await;
        let model = session
            .model()
            .await
            .unwrap_or_else(|| self.config.default_model.clone());

        let msg_count = messages.len();
        info!(model = %model, messages = msg_count, "LLM request");

        let request = LlmRequest {
            model,
            messages,
            tools: Some(self.tools.get_definitions()),
            max_tokens: Some(16384),
            system: session.system_prompt().await,
            ..Default::default()
        };

        let cancel = self.cancel_token().await;
        let stream = self
            .provider
            .complete_stream(request, Some(cancel))
            .await
            .map_err(|e| e.to_string())?;

        let mut text_content = String::new();
        let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();
        let mut current_tool: Option<AccumulatedToolCall> = None;
        let mut stop_reason = "end_turn".to_string();

        tokio::pin!(stream);

        while let Some(delta_result) = stream.next().await {
            // Check abort between chunks
            if self.cancel_token().await.is_cancelled() {
                return Ok((text_content, tool_calls, "aborted".into()));
            }

            match delta_result {
                Ok(delta) => match delta {
                    StreamDelta::Text(text) => {
                        text_content.push_str(&text);
                        let _ = event_tx.send(AgentEvent::Text(text)).await;
                    }
                    StreamDelta::Thinking(thinking) => {
                        let _ = event_tx.send(AgentEvent::Thinking(thinking)).await;
                    }
                    StreamDelta::ToolCallStart { id, name } => {
                        current_tool = Some(AccumulatedToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: String::new(),
                        });
                        let _ = event_tx.send(AgentEvent::ToolCallStart { id, name }).await;
                    }
                    StreamDelta::ToolCallDelta { id, arguments } => {
                        if let Some(ref mut tool) = current_tool {
                            tool.arguments.push_str(&arguments);
                        }
                        let _ = event_tx
                            .send(AgentEvent::ToolCallDelta { id, arguments })
                            .await;
                    }
                    StreamDelta::ToolCallEnd { id: _ } => {
                        if let Some(tool) = current_tool.take() {
                            tool_calls.push(tool);
                        }
                    }
                    StreamDelta::Done {
                        stop_reason: sr, ..
                    } => {
                        if let Some(r) = sr {
                            stop_reason = r;
                        }
                    }
                    StreamDelta::Error(e) => {
                        let _ = event_tx.send(AgentEvent::Error(e)).await;
                    }
                },
                Err(e) => {
                    let _ = event_tx.send(AgentEvent::Error(e.to_string())).await;
                }
            }
        }

        Ok((text_content, tool_calls, stop_reason))
    }

    /// Execute tool calls with steering-aware interruption.
    ///
    /// Between each tool execution, checks the steering queue.
    /// If steering messages arrive, remaining tools are SKIPPED
    /// (returned as "Skipped due to queued user message").
    ///
    /// Returns Some(steering_messages) if interrupted, None otherwise.
    async fn execute_tools_with_steering(
        &self,
        session: &Session,
        tool_calls: &[AccumulatedToolCall],
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Option<Vec<String>> {
        let mut steering_messages: Option<Vec<String>> = None;

        for (index, tc) in tool_calls.iter().enumerate() {
            // If we already got steering, skip remaining tools
            if steering_messages.is_some() {
                self.skip_tool(session, tc, event_tx).await;
                continue;
            }

            let _ = event_tx
                .send(AgentEvent::ToolExecuting {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                })
                .await;

            let args = tc.parse_arguments().unwrap_or_default();
            let args_summary = args
                .as_object()
                .and_then(|o| o.iter().next())
                .map(|(k, v)| {
                    format!(
                        "{}={}",
                        k,
                        v.as_str()
                            .unwrap_or(&v.to_string())
                            .chars()
                            .take(80)
                            .collect::<String>()
                    )
                })
                .unwrap_or_default();

            let start = std::time::Instant::now();
            info!(tool = %tc.name, id = %tc.id, args = %args_summary, "Tool executing");

            let result = self.tools.execute(&tc.name, args).await;

            let duration = start.elapsed();
            let is_error = result.is_error();
            let result_str = result.to_content_string();
            let result_str = if result_str.len() > 50000 {
                format!(
                    "{}...\n[truncated, {} total chars]",
                    &result_str[..50000],
                    result_str.len()
                )
            } else {
                result_str
            };

            if is_error {
                warn!(tool = %tc.name, id = %tc.id, duration_ms = duration.as_millis() as u64, "Tool failed");
            } else {
                info!(tool = %tc.name, id = %tc.id, duration_ms = duration.as_millis() as u64, "Tool completed");
            }

            let _ = event_tx
                .send(AgentEvent::ToolResult {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    result: result_str.clone(),
                    is_error,
                })
                .await;
            session.add_tool_result(&tc.id, &result_str, is_error).await;

            // Check steering queue after each tool (not after the last one — that's checked outside)
            if index < tool_calls.len() - 1 {
                let queued = self.queues.lock().await.drain_steering();
                if !queued.is_empty() {
                    info!(
                        steering_count = queued.len(),
                        remaining_tools = tool_calls.len() - index - 1,
                        "Steering interrupt — skipping remaining tools"
                    );
                    steering_messages = Some(queued);
                    // Don't break — loop continues but skips remaining via the guard above
                }
            }
        }

        steering_messages
    }

    /// Skip a tool call due to steering interrupt.
    async fn skip_tool(
        &self,
        session: &Session,
        tc: &AccumulatedToolCall,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) {
        let _ = event_tx
            .send(AgentEvent::ToolSkipped {
                id: tc.id.clone(),
                name: tc.name.clone(),
            })
            .await;
        session
            .add_tool_result(&tc.id, "Skipped due to queued user message.", true)
            .await;
    }
}

// ── SpawnableRuntime for subagent/KG child execution ────────────────────

#[async_trait::async_trait]
impl SpawnableRuntime for AgentRuntime {
    async fn spawn_child(
        &self,
        session_id: &str,
        system_prompt: &str,
        user_message: &str,
        max_iterations: usize,
    ) -> Result<(String, usize), String> {
        let session_key = SessionKey::from(format!("kg-child:{}", session_id));
        let session = self
            .sessions
            .get_or_create(&session_key, Some(system_prompt));
        session.set_system_prompt(system_prompt).await;

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);

        let tools = self.tools.clone();
        let provider = self.provider.clone();
        let sessions = self.sessions.clone();
        let default_model = self.config.default_model.clone();
        let sk = session_key.clone();
        let msg = user_message.to_string();

        let handle = tokio::spawn(async move {
            let session = sessions.get(&sk).unwrap();
            let max_context = 200_000;
            session.add_user_message(&msg, 0.55, max_context).await;

            let mut iterations = 0;
            loop {
                iterations += 1;
                if iterations > max_iterations {
                    let _ = tx
                        .send(AgentEvent::Error("Max tool iterations exceeded".into()))
                        .await;
                    break;
                }

                let messages = session.get_messages().await;
                let model = session
                    .model()
                    .await
                    .unwrap_or_else(|| default_model.clone());

                let request = LlmRequest {
                    model,
                    messages,
                    tools: Some(tools.get_definitions()),
                    max_tokens: Some(8192),
                    system: session.system_prompt().await,
                    ..Default::default()
                };

                let stream = match provider.complete_stream(request, None).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                        return Err(e.to_string());
                    }
                };

                let mut text_content = String::new();
                let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();
                let mut current_tool: Option<AccumulatedToolCall> = None;

                tokio::pin!(stream);
                while let Some(delta_result) = stream.next().await {
                    match delta_result {
                        Ok(delta) => match delta {
                            StreamDelta::Text(text) => {
                                text_content.push_str(&text);
                                let _ = tx.send(AgentEvent::Text(text)).await;
                            }
                            StreamDelta::Thinking(t) => {
                                let _ = tx.send(AgentEvent::Thinking(t)).await;
                            }
                            StreamDelta::ToolCallStart { id, name } => {
                                current_tool = Some(AccumulatedToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    arguments: String::new(),
                                });
                                let _ = tx.send(AgentEvent::ToolCallStart { id, name }).await;
                            }
                            StreamDelta::ToolCallDelta { id, arguments } => {
                                if let Some(ref mut tool) = current_tool {
                                    tool.arguments.push_str(&arguments);
                                }
                                let _ = tx.send(AgentEvent::ToolCallDelta { id, arguments }).await;
                            }
                            StreamDelta::ToolCallEnd { id: _ } => {
                                if let Some(tool) = current_tool.take() {
                                    tool_calls.push(tool);
                                }
                            }
                            StreamDelta::Done { .. } => {}
                            StreamDelta::Error(e) => {
                                let _ = tx.send(AgentEvent::Error(e)).await;
                            }
                        },
                        Err(e) => {
                            let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                        }
                    }
                }

                if tool_calls.is_empty() {
                    session.add_assistant_text(&text_content).await;
                    let _ = tx
                        .send(AgentEvent::Done {
                            stop_reason: "end_turn".into(),
                        })
                        .await;
                    break;
                } else {
                    let blocks: Vec<ContentBlock> = tool_calls
                        .iter()
                        .map(|tc| ContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input: tc.parse_arguments().unwrap_or_default(),
                        })
                        .collect();
                    session
                        .add_assistant_with_tools(
                            if text_content.is_empty() {
                                None
                            } else {
                                Some(&text_content)
                            },
                            blocks,
                        )
                        .await;
                }

                // Execute tools concurrently
                let tool_futures: Vec<_> = tool_calls
                    .iter()
                    .map(|tc| {
                        let tools = tools.clone();
                        let name = tc.name.clone();
                        let args = tc.parse_arguments().unwrap_or_default();
                        async move { tools.execute(&name, args).await }
                    })
                    .collect();

                let results = futures::future::join_all(tool_futures).await;

                for (tc, result) in tool_calls.iter().zip(results) {
                    let is_error = result.is_error();
                    let result_str = result.to_content_string();
                    let result_str = if result_str.len() > 50000 {
                        format!("{}...\n[truncated]", &result_str[..50000])
                    } else {
                        result_str
                    };
                    let _ = tx
                        .send(AgentEvent::ToolResult {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            result: result_str.clone(),
                            is_error,
                        })
                        .await;
                    session.add_tool_result(&tc.id, &result_str, is_error).await;
                }
            }
            Ok(())
        });

        // Collect output
        let mut output = String::new();
        let mut token_estimate = 0usize;
        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::Text(t) => {
                    output.push_str(&t);
                    token_estimate += t.len() / 4;
                }
                AgentEvent::Error(e) => {
                    if e != "Max tool iterations exceeded" {
                        warn!(child = session_id, "child error: {}", e);
                    }
                }
                _ => {}
            }
        }

        handle.await.map_err(|e| e.to_string())??;
        self.sessions.remove(&session_key);
        Ok((output, token_estimate))
    }
}
