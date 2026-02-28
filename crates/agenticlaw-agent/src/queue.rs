//! Event queue architecture — the v3 consciousness loop
//!
//! Replaces the synchronous `run_turn()` loop with a single ordered message queue
//! per consciousness instance. Every input — human messages, tool results, cascade
//! deltas, injections, system events — enters the same queue. A single consumer
//! loop processes events in priority order.
//!
//! Human messages ALWAYS preempt tool calls (park tools, cancel LLM stream).

use crate::session::{Session, SessionKey, SessionRegistry};
use agenticlaw_llm::{AccumulatedToolCall, ContentBlock, LlmProvider, LlmRequest, StreamDelta};
use agenticlaw_tools::{ToolRegistry, ToolResult};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Queue Event — every event that can enter the consciousness queue
// ---------------------------------------------------------------------------

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
        session: SessionKey,
    },

    /// Injection from a lower layer (L2+, Core)
    Injection {
        from: String,
        content: String,
    },

    /// LLM response complete — triggers dispatch
    LlmComplete {
        session: SessionKey,
        text: Option<String>,
        tool_calls: Vec<AccumulatedToolCall>,
        stop_reason: String,
        /// Unique ID for this LLM call — stale responses are ignored
        request_id: String,
    },

    /// System events
    Sleep {
        session: SessionKey,
        token_count: usize,
    },
    Wake {
        ego: String,
    },
    Shutdown,
}

impl QueueEvent {
    /// Get the priority of this event. Higher values are processed first.
    pub fn priority(&self) -> Priority {
        match self {
            QueueEvent::HumanMessage { priority, .. } => *priority,
            QueueEvent::Shutdown => Priority::System,
            QueueEvent::Sleep { .. } => Priority::System,
            _ => Priority::Normal,
        }
    }
}

// ---------------------------------------------------------------------------
// Priority — determines processing order
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Tool results, cascade deltas, injections
    Normal = 0,
    /// Human messages — always processed next
    Human = 10,
    /// System shutdown
    System = 20,
}

// ---------------------------------------------------------------------------
// Output Events — emitted to all connected clients via broadcast
// ---------------------------------------------------------------------------

/// Events emitted to all connected clients.
#[derive(Debug, Clone)]
pub enum OutputEvent {
    /// Streaming text delta
    Delta { session: String, content: String },
    /// Thinking content
    Thinking { session: String, content: String },
    /// Tool call started
    ToolCall {
        session: String,
        id: String,
        name: String,
    },
    /// Tool call arguments streaming
    ToolCallDelta {
        session: String,
        id: String,
        arguments: String,
    },
    /// Tool executing
    ToolExecuting {
        session: String,
        id: String,
        name: String,
    },
    /// Tool result
    ToolResult {
        session: String,
        id: String,
        name: String,
        result: String,
        is_error: bool,
    },
    /// Tool parked (interrupted by human)
    ToolParked {
        session: String,
        id: String,
        name: String,
    },
    /// Turn complete
    Done { session: String },
    /// Error
    Error { session: String, message: String },
    /// Session sleeping
    Sleep { session: String, token_count: usize },
    /// .ctx file updated — full content for client catchup
    CtxUpdate { session: String, content: String },
}

// ---------------------------------------------------------------------------
// Tool Handle — for tracking and interrupting active tool executions
// ---------------------------------------------------------------------------

/// Handle for an active tool execution.
pub struct ToolHandle {
    pub id: String,
    pub name: String,
    /// Cancel the tool's execution
    pub cancel: CancellationToken,
    /// Join handle for the spawned task
    pub join: JoinHandle<ToolResult>,
    /// Current state
    pub state: ToolState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Parked,
    Completed,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Consciousness Loop Configuration
// ---------------------------------------------------------------------------

pub struct ConsciousnessLoopConfig {
    pub default_model: String,
    pub max_tool_iterations: usize,
    pub sleep_threshold_pct: f64,
    pub max_context_tokens: usize,
}

impl Default for ConsciousnessLoopConfig {
    fn default() -> Self {
        Self {
            default_model: "claude-opus-4-6-20250929".to_string(),
            max_tool_iterations: 25,
            sleep_threshold_pct: 0.55,
            max_context_tokens: 200_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Consciousness Loop — the single consumer of the event queue
// ---------------------------------------------------------------------------

/// The consciousness loop: single consumer of the event queue.
///
/// All inputs enter through `queue_tx`. The loop processes events in priority
/// order, starts LLM calls, launches tools, and emits OutputEvents to all
/// connected clients via `output_tx`.
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
    /// Session registry
    sessions: Arc<SessionRegistry>,
    /// Configuration
    config: ConsciousnessLoopConfig,
    /// Active tool handles (for interruption)
    active_tools: HashMap<String, ToolHandle>,
    /// Cancellation token for the current LLM stream
    llm_cancel: Option<CancellationToken>,
    /// Current LLM request ID (to detect stale responses)
    current_request_id: Option<String>,
    /// Current active session
    current_session: Option<SessionKey>,
    /// Priority buffer — events waiting to be processed, sorted by priority
    priority_buffer: Vec<QueueEvent>,
    /// Tool iteration counter per turn
    tool_iterations: usize,
}

impl ConsciousnessLoop {
    /// Create a new consciousness loop.
    ///
    /// Returns `(loop, queue_tx, output_tx)` — callers use `queue_tx` to submit
    /// events and `output_tx` to subscribe to output events.
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        tools: Arc<ToolRegistry>,
        sessions: Arc<SessionRegistry>,
        config: ConsciousnessLoopConfig,
    ) -> (
        Self,
        mpsc::Sender<QueueEvent>,
        broadcast::Sender<OutputEvent>,
    ) {
        let (queue_tx, queue_rx) = mpsc::channel(1024);
        let (output_tx, _) = broadcast::channel(1024);

        let consciousness_loop = Self {
            queue_rx,
            queue_tx: queue_tx.clone(),
            output_tx: output_tx.clone(),
            provider,
            tools,
            sessions,
            config,
            active_tools: HashMap::new(),
            llm_cancel: None,
            current_request_id: None,
            current_session: None,
            priority_buffer: Vec::new(),
            tool_iterations: 0,
        };

        (consciousness_loop, queue_tx, output_tx)
    }

    /// Run the consciousness loop. Processes events until Shutdown.
    pub async fn run(&mut self) {
        info!("ConsciousnessLoop started");
        loop {
            let event = match self.recv_with_priority().await {
                Some(e) => e,
                None => {
                    info!("ConsciousnessLoop: queue closed, shutting down");
                    break;
                }
            };

            match event {
                QueueEvent::HumanMessage {
                    session, content, ..
                } => {
                    self.handle_human_message(session, content).await;
                }

                QueueEvent::ToolResult {
                    session,
                    tool_use_id,
                    name,
                    result,
                    is_error,
                } => {
                    self.handle_tool_result(session, tool_use_id, name, result, is_error)
                        .await;
                }

                QueueEvent::LlmComplete {
                    session,
                    text,
                    tool_calls,
                    stop_reason,
                    request_id,
                } => {
                    self.handle_llm_complete(session, text, tool_calls, stop_reason, request_id)
                        .await;
                }

                QueueEvent::CascadeDelta { session, delta, .. } => {
                    // Treat cascade deltas as user messages for inner layers
                    self.handle_human_message(session, delta).await;
                }

                QueueEvent::Injection { content, .. } => {
                    // Store pending injection for next LLM call
                    debug!(
                        "Injection received ({} chars), will be included in next LLM call",
                        content.len()
                    );
                }

                QueueEvent::Sleep {
                    session,
                    token_count,
                } => {
                    let session_str = session.as_str().to_string();
                    let _ = self.output_tx.send(OutputEvent::Sleep {
                        session: session_str,
                        token_count,
                    });
                }

                QueueEvent::Shutdown => {
                    info!("ConsciousnessLoop: received Shutdown");
                    self.park_all_tools().await;
                    if let Some(cancel) = self.llm_cancel.take() {
                        cancel.cancel();
                    }
                    break;
                }

                QueueEvent::Wake { .. } => {
                    debug!("Wake event received");
                }
            }
        }
        info!("ConsciousnessLoop stopped");
    }

    /// Receive the next event, preferring higher priority.
    /// Drains all immediately available events, sorts by priority.
    async fn recv_with_priority(&mut self) -> Option<QueueEvent> {
        // If priority buffer has events from a previous drain, use those first
        if !self.priority_buffer.is_empty() {
            self.priority_buffer
                .sort_by_key(|e| std::cmp::Reverse(e.priority()));
            return Some(self.priority_buffer.remove(0));
        }

        // Wait for the first event
        let first = self.queue_rx.recv().await?;
        self.priority_buffer.push(first);

        // Drain any immediately available events
        while let Ok(event) = self.queue_rx.try_recv() {
            self.priority_buffer.push(event);
        }

        // Sort: highest priority first
        self.priority_buffer
            .sort_by_key(|e| std::cmp::Reverse(e.priority()));
        Some(self.priority_buffer.remove(0))
    }

    async fn handle_human_message(&mut self, session: SessionKey, content: String) {
        let session_str = session.as_str().to_string();

        // 1. Park all active tools (cancel them)
        self.park_all_tools().await;

        // 2. Cancel any in-flight LLM stream
        if let Some(cancel) = self.llm_cancel.take() {
            cancel.cancel();
        }
        self.current_request_id = None;

        // 3. Reset tool iterations for this new turn
        self.tool_iterations = 0;

        // 4. Add message to session
        let sess = self.get_session(&session);
        let should_sleep = sess
            .add_user_message(
                &content,
                self.config.sleep_threshold_pct,
                self.config.max_context_tokens,
            )
            .await;

        if should_sleep {
            let token_count = sess.token_count().await;
            let _ = self.output_tx.send(OutputEvent::Sleep {
                session: session_str,
                token_count,
            });
            let _ = self.output_tx.send(OutputEvent::Done {
                session: session.as_str().to_string(),
            });
            return;
        }

        // 5. Update current session and start LLM call
        self.current_session = Some(session.clone());
        self.start_llm_call(&session).await;
    }

    async fn handle_tool_result(
        &mut self,
        session: SessionKey,
        tool_use_id: String,
        name: String,
        result: String,
        is_error: bool,
    ) {
        // Remove from active tools (if it's there — may have been parked/drained)
        self.active_tools.remove(&tool_use_id);

        // Add result to session
        let sess = self.get_session(&session);
        sess.add_tool_result(&tool_use_id, &result, is_error).await;

        let session_str = session.as_str().to_string();
        let _ = self.output_tx.send(OutputEvent::ToolResult {
            session: session_str,
            id: tool_use_id,
            name,
            result,
            is_error,
        });

        // If all tools done and no in-flight LLM, start next LLM call
        if self.active_tools.is_empty() && self.llm_cancel.is_none() {
            self.tool_iterations += 1;
            if self.tool_iterations > self.config.max_tool_iterations {
                let _ = self.output_tx.send(OutputEvent::Error {
                    session: session.as_str().to_string(),
                    message: "Max tool iterations exceeded".to_string(),
                });
                let _ = self.output_tx.send(OutputEvent::Done {
                    session: session.as_str().to_string(),
                });
                return;
            }
            self.start_llm_call(&session).await;
        }
    }

    async fn handle_llm_complete(
        &mut self,
        session: SessionKey,
        text: Option<String>,
        tool_calls: Vec<AccumulatedToolCall>,
        _stop_reason: String,
        request_id: String,
    ) {
        // Ignore stale LLM responses
        if self.current_request_id.as_ref() != Some(&request_id) {
            debug!("Ignoring stale LLM response (request_id mismatch)");
            return;
        }

        // LLM call is done
        self.llm_cancel = None;
        self.current_request_id = None;

        let sess = self.get_session(&session);
        let session_str = session.as_str().to_string();

        if tool_calls.is_empty() {
            // No tool calls — save text and emit Done
            if let Some(ref t) = text {
                sess.add_assistant_text(t).await;
            }
            let _ = self.output_tx.send(OutputEvent::Done {
                session: session_str,
            });
        } else {
            // Save assistant message with tool calls
            let blocks: Vec<ContentBlock> = tool_calls
                .iter()
                .map(|tc| ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.parse_arguments().unwrap_or_default(),
                })
                .collect();
            sess.add_assistant_with_tools(text.as_deref().filter(|t| !t.is_empty()), blocks)
                .await;

            // Launch tools
            for tc in tool_calls {
                self.launch_tool(&session, tc).await;
            }
        }
    }

    /// Start a new LLM call for the given session.
    async fn start_llm_call(&mut self, session_key: &SessionKey) {
        let sess = self.get_session(session_key);
        let messages = sess.get_messages().await;
        let model = sess
            .model()
            .await
            .unwrap_or_else(|| self.config.default_model.clone());

        let request = LlmRequest {
            model,
            messages,
            tools: Some(self.tools.get_definitions()),
            max_tokens: Some(8192),
            system: sess.system_prompt().await,
            ..Default::default()
        };

        let cancel = CancellationToken::new();
        self.llm_cancel = Some(cancel.clone());

        let request_id = uuid::Uuid::new_v4().to_string();
        self.current_request_id = Some(request_id.clone());

        let provider = self.provider.clone();
        let queue_tx = self.queue_tx.clone();
        let output_tx = self.output_tx.clone();
        let session_str = session_key.as_str().to_string();
        let sk = session_key.clone();

        tokio::spawn(async move {
            let stream = match provider.complete_stream(request).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = output_tx.send(OutputEvent::Error {
                        session: session_str,
                        message: e.to_string(),
                    });
                    return;
                }
            };

            tokio::pin!(stream);

            let mut text_content = String::new();
            let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();
            let mut current_tool: Option<AccumulatedToolCall> = None;
            let mut stop_reason = "end_turn".to_string();

            loop {
                tokio::select! {
                    delta = stream.next() => {
                        match delta {
                            Some(Ok(d)) => match d {
                                StreamDelta::Text(text) => {
                                    text_content.push_str(&text);
                                    let _ = output_tx.send(OutputEvent::Delta {
                                        session: session_str.clone(),
                                        content: text,
                                    });
                                }
                                StreamDelta::Thinking(thinking) => {
                                    let _ = output_tx.send(OutputEvent::Thinking {
                                        session: session_str.clone(),
                                        content: thinking,
                                    });
                                }
                                StreamDelta::ToolCallStart { id, name } => {
                                    current_tool = Some(AccumulatedToolCall {
                                        id: id.clone(),
                                        name: name.clone(),
                                        arguments: String::new(),
                                    });
                                    let _ = output_tx.send(OutputEvent::ToolCall {
                                        session: session_str.clone(),
                                        id,
                                        name,
                                    });
                                }
                                StreamDelta::ToolCallDelta { id, arguments } => {
                                    if let Some(ref mut tool) = current_tool {
                                        tool.arguments.push_str(&arguments);
                                    }
                                    let _ = output_tx.send(OutputEvent::ToolCallDelta {
                                        session: session_str.clone(),
                                        id,
                                        arguments,
                                    });
                                }
                                StreamDelta::ToolCallEnd { .. } => {
                                    if let Some(tool) = current_tool.take() {
                                        tool_calls.push(tool);
                                    }
                                }
                                StreamDelta::Done { stop_reason: sr, .. } => {
                                    if let Some(r) = sr {
                                        stop_reason = r;
                                    }
                                }
                                StreamDelta::Error(e) => {
                                    let _ = output_tx.send(OutputEvent::Error {
                                        session: session_str.clone(),
                                        message: e,
                                    });
                                }
                            },
                            Some(Err(e)) => {
                                let _ = output_tx.send(OutputEvent::Error {
                                    session: session_str.clone(),
                                    message: e.to_string(),
                                });
                            }
                            None => break,
                        }
                    }
                    _ = cancel.cancelled() => {
                        // LLM stream cancelled — do NOT submit LlmComplete
                        debug!("LLM stream cancelled for session {}", session_str);
                        return;
                    }
                }
            }

            // Stream finished naturally — submit LlmComplete
            let _ = queue_tx
                .send(QueueEvent::LlmComplete {
                    session: sk,
                    text: if text_content.is_empty() {
                        None
                    } else {
                        Some(text_content)
                    },
                    tool_calls,
                    stop_reason,
                    request_id,
                })
                .await;
        });
    }

    /// Launch a tool execution in a background task.
    async fn launch_tool(&mut self, session: &SessionKey, tc: AccumulatedToolCall) {
        let cancel = CancellationToken::new();
        let tools = self.tools.clone();
        let queue_tx = self.queue_tx.clone();
        let output_tx = self.output_tx.clone();
        let session_str = session.as_str().to_string();
        let session_key = session.clone();
        let tc_id = tc.id.clone();
        let tc_name = tc.name.clone();
        let cancel_clone = cancel.clone();

        let _ = output_tx.send(OutputEvent::ToolExecuting {
            session: session_str.clone(),
            id: tc_id.clone(),
            name: tc_name.clone(),
        });

        let spawn_id = tc_id.clone();
        let spawn_name = tc_name.clone();
        let join = tokio::spawn(async move {
            let args = tc.parse_arguments().unwrap_or_default();
            let tool_name = tc.name.clone();

            // Execute with cancellation
            let result = tools
                .execute_cancellable(&tool_name, args, cancel_clone)
                .await;

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

            let _ = queue_tx
                .send(QueueEvent::ToolResult {
                    session: session_key,
                    tool_use_id: spawn_id,
                    name: spawn_name,
                    result: result_str,
                    is_error,
                })
                .await;

            result
        });

        self.active_tools.insert(
            tc_id.clone(),
            ToolHandle {
                id: tc_id,
                name: tc_name,
                cancel,
                join,
                state: ToolState::Running,
            },
        );
    }

    /// Park (cancel) all active tools. Drains the active_tools map.
    async fn park_all_tools(&mut self) {
        let tools: Vec<(String, ToolHandle)> = self.active_tools.drain().collect();
        for (_, handle) in tools {
            if handle.state == ToolState::Running {
                handle.cancel.cancel();
                let session_str = self
                    .current_session
                    .as_ref()
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_default();
                let _ = self.output_tx.send(OutputEvent::ToolParked {
                    session: session_str,
                    id: handle.id,
                    name: handle.name,
                });
            }
        }
    }

    /// Get or create a session.
    fn get_session(&self, session_key: &SessionKey) -> Arc<Session> {
        self.sessions.get_or_create(session_key, None)
    }
}
