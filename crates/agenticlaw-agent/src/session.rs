//! Session management with .ctx file persistence

use crate::context::ContextManager;
use crate::ctx_file;
use dashmap::DashMap;
use agenticlaw_llm::{LlmMessage, LlmContent, ContentBlock};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

// Sleep threshold is configured via consciousness.toml [sleep] section

pub use agenticlaw_core::SessionKey;

pub struct SessionRegistry {
    sessions: DashMap<SessionKey, Arc<Session>>,
}

impl Default for SessionRegistry {
    fn default() -> Self { Self::new() }
}

impl SessionRegistry {
    pub fn new() -> Self { Self { sessions: DashMap::new() } }

    /// Create a session with .ctx persistence. Discovers SOUL.md/AGENTS.md in workspace.
    pub fn create_with_ctx(
        &self,
        key: &SessionKey,
        system_prompt: Option<&str>,
        workspace: &Path,
    ) -> Arc<Session> {
        self.sessions
            .entry(key.clone())
            .or_insert_with(|| {
                let session_id = key.as_str().to_string();
                let ctx_path = ctx_file::session_ctx_path(workspace, &session_id);
                let timestamp = ctx_file::now_timestamp();

                // Discover and load context files
                let preload = ctx_file::discover_preload_files(workspace);

                // Build system prompt from preloaded files
                let combined_system = if preload.is_empty() {
                    system_prompt.map(String::from)
                } else {
                    let mut sys = preload.join("\n\n");
                    if let Some(extra) = system_prompt {
                        sys.push_str("\n\n");
                        sys.push_str(extra);
                    }
                    Some(sys)
                };

                // Create .ctx file on disk
                if let Err(e) = ctx_file::create(
                    &ctx_path,
                    &session_id,
                    &timestamp,
                    Some(&workspace.to_string_lossy()),
                    &preload,
                ) {
                    tracing::error!("Failed to create .ctx file: {}", e);
                }

                info!("Session {} created: {} ({} preload files)", session_id, ctx_path.display(), preload.len());

                Arc::new(Session::new_with_ctx(
                    key.clone(),
                    combined_system.as_deref(),
                    Some(ctx_path),
                ))
            })
            .clone()
    }

    /// Resume a session from an existing .ctx file.
    pub fn resume_from_ctx(
        &self,
        resumed: &ctx_file::ResumedSession,
    ) -> Arc<Session> {
        let key = SessionKey::new(&resumed.session_id);
        self.sessions
            .entry(key.clone())
            .or_insert_with(|| {
                let session = Session::new_with_ctx(
                    key.clone(),
                    resumed.system_prompt.as_deref(),
                    Some(resumed.ctx_path.clone()),
                );

                // Hydrate messages from the parsed .ctx
                let messages = &resumed.messages;
                let mut msg_vec = Vec::new();
                for (role, content) in messages {
                    msg_vec.push(agenticlaw_llm::LlmMessage {
                        role: role.clone(),
                        content: agenticlaw_llm::LlmContent::Text(content.clone()),
                    });
                }

                // We need to set messages synchronously during construction.
                // Since Session uses RwLock, we'll use blocking_write via a helper.
                let session = Arc::new(session);
                let s = session.clone();
                let count = msg_vec.len();
                // Use tokio's Handle to run async in sync context
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.block_on(async {
                        let mut lock = s.messages_mut().await;
                        *lock = msg_vec;
                    });
                }

                info!("Resumed session {} from {} ({} messages)", resumed.session_id, resumed.ctx_path.display(), count);
                session
            })
            .clone()
    }

    pub fn get_or_create(&self, key: &SessionKey, system_prompt: Option<&str>) -> Arc<Session> {
        self.sessions
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Session::new(key.clone(), system_prompt)))
            .clone()
    }

    pub fn get(&self, key: &SessionKey) -> Option<Arc<Session>> {
        self.sessions.get(key).map(|s| s.clone())
    }

    pub fn list(&self) -> Vec<SessionKey> {
        self.sessions.iter().map(|e| e.key().clone()).collect()
    }

    pub fn remove(&self, key: &SessionKey) -> Option<Arc<Session>> {
        self.sessions.remove(key).map(|(_, s)| s)
    }
}

pub struct Session {
    pub key: SessionKey,
    system_prompt: RwLock<Option<String>>,
    messages: RwLock<Vec<LlmMessage>>,
    context: RwLock<ContextManager>,
    model: RwLock<Option<String>>,
    ctx_path: Option<PathBuf>,
    abort_tx: mpsc::Sender<()>,
    abort_rx: RwLock<Option<mpsc::Receiver<()>>>,
}

impl Session {
    pub fn new(key: SessionKey, system_prompt: Option<&str>) -> Self {
        Self::new_with_ctx(key, system_prompt, None)
    }

    pub fn new_with_ctx(key: SessionKey, system_prompt: Option<&str>, ctx_path: Option<PathBuf>) -> Self {
        let (abort_tx, abort_rx) = mpsc::channel(1);
        let mut context = ContextManager::new(128_000);
        if let Some(sys) = system_prompt { context.set_system(sys); }
        Self {
            key,
            system_prompt: RwLock::new(system_prompt.map(String::from)),
            messages: RwLock::new(Vec::new()),
            context: RwLock::new(context),
            model: RwLock::new(None),
            ctx_path,
            abort_tx,
            abort_rx: RwLock::new(Some(abort_rx)),
        }
    }

    /// Get the .ctx file path, if persisted.
    pub fn ctx_path(&self) -> Option<&Path> {
        self.ctx_path.as_deref()
    }

    /// Read the full .ctx file contents from disk.
    pub fn read_ctx(&self) -> Option<String> {
        self.ctx_path.as_ref().and_then(|p| ctx_file::read(p).ok())
    }

    pub async fn system_prompt(&self) -> Option<String> { self.system_prompt.read().await.clone() }

    pub async fn set_system_prompt(&self, prompt: &str) {
        *self.system_prompt.write().await = Some(prompt.to_string());
        self.context.write().await.set_system(prompt);
    }

    /// Add a user message. Returns `true` if the session should sleep
    /// (context limit reached), `false` otherwise.
    /// Add a user message. Returns true if estimated tokens exceed the sleep threshold
    /// (pct * max_context_tokens), signaling the layer should sleep.
    pub async fn add_user_message(&self, content: &str, sleep_threshold_pct: f64, max_context_tokens: usize) -> bool {
        let message = LlmMessage { role: "user".to_string(), content: LlmContent::Text(content.to_string()) };
        let mut messages = self.messages.write().await;
        messages.push(message);

        // Persist to .ctx
        if let Some(ref path) = self.ctx_path {
            let _ = ctx_file::append_user_message(path, &ctx_file::now_timestamp(), content);
        }

        let context = self.context.read().await;
        let total = context.calculate_total(&messages);
        let sleep_threshold = (sleep_threshold_pct * max_context_tokens as f64) as usize;
        if total > sleep_threshold {
            info!("Session {} hit {}k tokens ({:.0}% of {}k) — signaling sleep",
                self.key, total / 1000, (total as f64 / max_context_tokens as f64) * 100.0, max_context_tokens / 1000);
            true
        } else {
            false
        }
    }

    pub async fn add_assistant_text(&self, content: &str) {
        let message = LlmMessage { role: "assistant".to_string(), content: LlmContent::Text(content.to_string()) };
        self.messages.write().await.push(message);

        if let Some(ref path) = self.ctx_path {
            let _ = ctx_file::append_assistant_text(path, &ctx_file::now_timestamp(), content);
        }
    }

    pub async fn add_assistant_with_tools(&self, text: Option<&str>, tool_calls: Vec<ContentBlock>) {
        let mut blocks = Vec::new();
        if let Some(t) = text {
            if !t.is_empty() { blocks.push(ContentBlock::Text { text: t.to_string() }); }
        }
        blocks.extend(tool_calls.clone());
        let message = LlmMessage { role: "assistant".to_string(), content: LlmContent::Blocks(blocks) };
        self.messages.write().await.push(message);

        // Persist: write text + tool calls to .ctx
        if let Some(ref path) = self.ctx_path {
            let ts = ctx_file::now_timestamp();
            let mut ctx_content = String::new();
            if let Some(t) = text {
                if !t.is_empty() {
                    ctx_content.push_str(t);
                    ctx_content.push('\n');
                }
            }
            for tc in &tool_calls {
                if let ContentBlock::ToolUse { name, input, .. } = tc {
                    let summary = input.as_object()
                        .and_then(|o| o.iter().next())
                        .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or(&v.to_string())))
                        .unwrap_or_default();
                    ctx_content.push_str(&format!("[tool:{}] {}\n", name, summary));
                }
            }
            let _ = ctx_file::append_assistant_text(path, &ts, ctx_content.trim());
        }
    }

    pub async fn add_tool_result(&self, tool_use_id: &str, content: &str, is_error: bool) {
        let block = ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error: if is_error { Some(true) } else { None },
        };

        let mut messages = self.messages.write().await;

        // Anthropic requires ALL tool_results for a turn in a SINGLE user message.
        // If the last message is already a user message with tool_result blocks,
        // append to it instead of creating a new message.
        let appended = if let Some(last) = messages.last_mut() {
            if last.role == "user" {
                if let LlmContent::Blocks(ref mut blocks) = last.content {
                    if blocks.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })) {
                        blocks.push(block.clone());
                        true
                    } else { false }
                } else { false }
            } else { false }
        } else { false };

        if !appended {
            messages.push(LlmMessage { role: "user".to_string(), content: LlmContent::Blocks(vec![block]) });
        }
        drop(messages);

        // Tool results are <up> in .ctx — they're input to the model from outside
        if let Some(ref path) = self.ctx_path {
            let _ = ctx_file::append_tool_result(path, &ctx_file::now_timestamp(), "result", content, is_error);
        }
    }

    pub async fn get_messages(&self) -> Vec<LlmMessage> { self.messages.read().await.clone() }
    pub async fn messages_mut(&self) -> tokio::sync::RwLockWriteGuard<'_, Vec<LlmMessage>> { self.messages.write().await }
    pub async fn message_count(&self) -> usize { self.messages.read().await.len() }

    pub async fn token_count(&self) -> usize {
        let messages = self.messages.read().await;
        self.context.read().await.calculate_total(&messages)
    }

    pub async fn model(&self) -> Option<String> { self.model.read().await.clone() }
    pub async fn set_model(&self, model: &str) { *self.model.write().await = Some(model.to_string()); }
    pub async fn abort(&self) { let _ = self.abort_tx.send(()).await; }

    pub async fn take_abort_rx(&self) -> Option<mpsc::Receiver<()>> {
        self.abort_rx.write().await.take()
    }

    pub async fn clear(&self) { self.messages.write().await.clear(); }
}
