//! Agent runtime - the core agentic loop with .ctx persistence

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
use tokio::sync::mpsc;
use tracing::{debug, info, instrument, warn};

#[derive(Clone, Debug)]
pub enum AgentEvent {
    Text(String),
    Thinking(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        arguments: String,
    },
    ToolExecuting {
        id: String,
        name: String,
    },
    ToolResult {
        id: String,
        name: String,
        result: String,
        is_error: bool,
    },
    /// Layer hit context limit — should sleep instead of compacting.
    Sleep {
        token_count: usize,
    },
    Done {
        stop_reason: String,
    },
    Error(String),
}

pub struct AgentConfig {
    pub default_model: String,
    pub max_tool_iterations: usize,
    pub system_prompt: Option<String>,
    pub workspace_root: PathBuf,
    /// Context utilization percentage that triggers sleep (0.0 - 1.0).
    /// Resolved against model's max context window at runtime.
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

pub struct AgentRuntime {
    provider: Arc<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    sessions: Arc<SessionRegistry>,
    config: AgentConfig,
}

impl AgentRuntime {
    pub fn new(api_key: &str, tools: ToolRegistry, config: AgentConfig) -> Self {
        Self {
            provider: Arc::new(AnthropicProvider::new(api_key)),
            tools: Arc::new(tools),
            sessions: Arc::new(SessionRegistry::new()),
            config,
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

    /// Get or create a session with .ctx persistence.
    fn get_session(&self, session_key: &SessionKey) -> Arc<Session> {
        self.sessions.create_with_ctx(
            session_key,
            self.config.system_prompt.as_deref(),
            &self.config.workspace_root,
        )
    }

    #[instrument(skip(self, user_message, event_tx), fields(session = %session_key))]
    pub async fn run_turn(
        &self,
        session_key: &SessionKey,
        user_message: &str,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<(), String> {
        let session = self.get_session(session_key);
        // Claude Opus context window: 200k tokens. TODO: get from provider.
        let max_context = 200_000;
        let should_sleep = session
            .add_user_message(user_message, self.config.sleep_threshold_pct, max_context)
            .await;

        if should_sleep {
            let token_count = session.token_count().await;
            let _ = event_tx.send(AgentEvent::Sleep { token_count }).await;
            return Ok(());
        }

        let mut iterations = 0;

        loop {
            iterations += 1;
            if iterations > self.config.max_tool_iterations {
                let _ = event_tx
                    .send(AgentEvent::Error(
                        "Max tool iterations exceeded".to_string(),
                    ))
                    .await;
                break;
            }

            let messages = session.get_messages().await;
            let model = session
                .model()
                .await
                .unwrap_or_else(|| self.config.default_model.clone());

            let msg_count = messages.len();
            info!(iteration = iterations, model = %model, messages = msg_count, "Sending API request");
            let request = LlmRequest {
                model,
                messages,
                tools: Some(self.tools.get_definitions()),
                max_tokens: Some(8192),
                system: session.system_prompt().await,
                ..Default::default()
            };

            let stream = match self.provider.complete_stream(request).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = event_tx.send(AgentEvent::Error(e.to_string())).await;
                    tracing::error!(error = %e, "API request failed");
                    return Err(e.to_string());
                }
            };

            let mut text_content = String::new();
            let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();
            let mut current_tool: Option<AccumulatedToolCall> = None;
            let mut stop_reason = "end_turn".to_string();
            let mut usage: Option<agenticlaw_llm::Usage> = None;

            tokio::pin!(stream);

            while let Some(delta_result) = stream.next().await {
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
                            stop_reason: sr,
                            usage: u,
                        } => {
                            if let Some(r) = sr {
                                stop_reason = r;
                            }
                            if u.is_some() {
                                usage = u;
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

            // Log API response with usage
            if let Some(ref u) = usage {
                info!(
                    turn = iterations,
                    stop_reason = %stop_reason,
                    input_tokens = u.input_tokens,
                    output_tokens = u.output_tokens,
                    "API response received"
                );
            } else {
                info!(turn = iterations, stop_reason = %stop_reason, "API response received (no usage reported)");
            }

            // Save to in-memory session + .ctx file
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

            if tool_calls.is_empty() {
                info!(stop_reason = %stop_reason, text_len = text_content.len(), "Response complete (no tool calls)");
                let _ = event_tx.send(AgentEvent::Done { stop_reason }).await;
                break;
            }

            info!(
                stop_reason = %stop_reason,
                tool_count = tool_calls.len(),
                tools = %tool_calls.iter().map(|tc| tc.name.as_str()).collect::<Vec<_>>().join(","),
                "Response complete (with tool calls)"
            );

            // Execute tools concurrently — results get persisted to .ctx via session.add_tool_result
            // Emit ToolExecuting for all tools before launching
            for tc in &tool_calls {
                let _ = event_tx
                    .send(AgentEvent::ToolExecuting {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                    })
                    .await;
            }

            // Launch all tool executions concurrently
            let tool_futures: Vec<_> = tool_calls
                .iter()
                .map(|tc| {
                    let tools = self.tools.clone();
                    let name = tc.name.clone();
                    let id = tc.id.clone();
                    let args = tc.parse_arguments().unwrap_or_default();
                    let args_summary = args.as_object()
                        .and_then(|o| o.iter().next())
                        .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or(&v.to_string()).chars().take(80).collect::<String>()))
                        .unwrap_or_default();
                    async move {
                        let start = std::time::Instant::now();
                        info!(tool = %name, id = %id, args = %args_summary, "Tool executing");
                        let result = tools.execute(&name, args).await;
                        let duration = start.elapsed();
                        let size = result.to_content_string().len();
                        if result.is_error() {
                            warn!(tool = %name, id = %id, duration_ms = duration.as_millis() as u64, size, "Tool failed");
                        } else {
                            info!(tool = %name, id = %id, duration_ms = duration.as_millis() as u64, size, "Tool completed");
                        }
                        result
                    }
                })
                .collect();

            let results = futures::future::join_all(tool_futures).await;

            // Process results in order, adding to session
            for (tc, result) in tool_calls.iter().zip(results) {
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
                let _ = event_tx
                    .send(AgentEvent::ToolResult {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        result: result_str.clone(),
                        is_error,
                    })
                    .await;
                session.add_tool_result(&tc.id, &result_str, is_error).await;
            }

            debug!(
                "Tool calls executed, continuing loop (iteration {})",
                iterations
            );
        }

        let msg_count_final = session.message_count().await;
        let token_count_final = session.token_count().await;
        info!(
            session = %session_key,
            messages = msg_count_final,
            tokens = token_count_final,
            "Turn complete"
        );
        Ok(())
    }
}

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

        // Create child session with system prompt
        let session = self
            .sessions
            .get_or_create(&session_key, Some(system_prompt));
        session.set_system_prompt(system_prompt).await;

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);

        // Temporarily override max iterations for this child
        // (We run the turn and collect output)
        let runtime_tools = self.tools.clone();
        let runtime_provider = self.provider.clone();
        let runtime_sessions = self.sessions.clone();
        let default_model = self.config.default_model.clone();
        let sk = session_key.clone();
        let msg = user_message.to_string();

        let handle = tokio::spawn(async move {
            // Run a custom turn loop with the child's max_iterations
            let session = runtime_sessions.get(&sk).unwrap();
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

                let request = agenticlaw_llm::LlmRequest {
                    model,
                    messages,
                    tools: Some(runtime_tools.get_definitions()),
                    max_tokens: Some(8192),
                    system: session.system_prompt().await,
                    ..Default::default()
                };

                let stream = match runtime_provider.complete_stream(request).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                        return Err(e.to_string());
                    }
                };

                let mut text_content = String::new();
                let mut tool_calls: Vec<agenticlaw_llm::AccumulatedToolCall> = Vec::new();
                let mut current_tool: Option<agenticlaw_llm::AccumulatedToolCall> = None;

                use futures::StreamExt;
                tokio::pin!(stream);

                while let Some(delta_result) = stream.next().await {
                    match delta_result {
                        Ok(delta) => match delta {
                            agenticlaw_llm::StreamDelta::Text(text) => {
                                text_content.push_str(&text);
                                let _ = tx.send(AgentEvent::Text(text)).await;
                            }
                            agenticlaw_llm::StreamDelta::Thinking(t) => {
                                let _ = tx.send(AgentEvent::Thinking(t)).await;
                            }
                            agenticlaw_llm::StreamDelta::ToolCallStart { id, name } => {
                                current_tool = Some(agenticlaw_llm::AccumulatedToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    arguments: String::new(),
                                });
                                let _ = tx.send(AgentEvent::ToolCallStart { id, name }).await;
                            }
                            agenticlaw_llm::StreamDelta::ToolCallDelta { id, arguments } => {
                                if let Some(ref mut tool) = current_tool {
                                    tool.arguments.push_str(&arguments);
                                }
                                let _ = tx.send(AgentEvent::ToolCallDelta { id, arguments }).await;
                            }
                            agenticlaw_llm::StreamDelta::ToolCallEnd { id: _ } => {
                                if let Some(tool) = current_tool.take() {
                                    tool_calls.push(tool);
                                }
                            }
                            agenticlaw_llm::StreamDelta::Done { .. } => {}
                            agenticlaw_llm::StreamDelta::Error(e) => {
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
                    let blocks: Vec<agenticlaw_llm::ContentBlock> = tool_calls
                        .iter()
                        .map(|tc| agenticlaw_llm::ContentBlock::ToolUse {
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
                for tc in &tool_calls {
                    let _ = tx
                        .send(AgentEvent::ToolExecuting {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                        })
                        .await;
                }

                let tool_futures: Vec<_> = tool_calls
                    .iter()
                    .map(|tc| {
                        let tools = runtime_tools.clone();
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

        // Collect output from child
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
                        tracing::warn!(child = session_id, "child error: {}", e);
                    }
                }
                _ => {}
            }
        }

        handle.await.map_err(|e| e.to_string())??;

        // Clean up child session
        self.sessions.remove(&session_key);

        Ok((output, token_estimate))
    }
}
