//! Ego distillation — LLM-powered identity summarization for wake
//!
//! Each layer distills the ego of the layer it watches:
//!   L1 → L0's ego (L1 knows L0 best)
//!   L2 → L1's ego
//!   L3 → L2's ego
//!   Core → L3's ego + Core's own ego (for L0's wake)
//!
//! The distilled ego is written to `<layer>/ego.md` and becomes
//! byte 0 of that layer's context on wake.

use crate::config::ConsciousnessConfig;
use crate::stack::{extract_tail_paragraphs, find_latest_ctx, safe_byte_boundary};
use agenticlaw_llm::{
    AnthropicProvider, LlmContent, LlmMessage, LlmProvider, LlmRequest, StreamDelta,
};
use futures::StreamExt;
use std::path::Path;
use tracing::{error, info, warn};

/// Distill ego for a target layer by asking its watcher layer.
///
/// `watcher_sessions` — the .ctx sessions dir of the layer doing the watching
/// `target_name` — name of the layer being described (for logging)
/// `prompt` — the distillation prompt
/// `context_budget` — max chars of watcher .ctx to include as context
/// `max_tokens` — max output tokens for the LLM call
pub async fn distill_ego(
    api_key: &str,
    model: &str,
    watcher_sessions: &Path,
    target_name: &str,
    prompt: &str,
    context_budget: usize,
    max_tokens: usize,
) -> Option<String> {
    // Read the watcher's latest .ctx as context
    let ctx_path = find_latest_ctx(watcher_sessions)?;
    let content = std::fs::read_to_string(&ctx_path).ok()?;
    if content.trim().is_empty() {
        return None;
    }

    // Take the tail within budget
    let context = if content.len() > context_budget {
        let boundary = safe_byte_boundary(&content, content.len() - context_budget);
        &content[boundary..]
    } else {
        &content
    };

    let provider = AnthropicProvider::new(api_key);
    let request = LlmRequest {
        model: model.to_string(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: LlmContent::Text(format!(
                "{}\n\n--- Your context (what you've observed) ---\n\n{}",
                prompt, context
            )),
        }],
        max_tokens: Some(max_tokens as u32),
        ..Default::default()
    };

    let stream = match provider.complete_stream(request).await {
        Ok(s) => s,
        Err(e) => {
            error!("Ego distillation for {} failed: {}", target_name, e);
            return None;
        }
    };

    let mut text = String::new();
    tokio::pin!(stream);
    while let Some(delta_result) = stream.next().await {
        match delta_result {
            Ok(StreamDelta::Text(t)) => text.push_str(&t),
            Ok(StreamDelta::Done { .. }) => break,
            Ok(StreamDelta::Error(e)) => {
                error!("Ego distillation stream error for {}: {}", target_name, e);
                break;
            }
            Err(e) => {
                error!("Ego distillation stream error for {}: {}", target_name, e);
                break;
            }
            _ => {}
        }
    }

    if text.trim().is_empty() {
        warn!(
            "Ego distillation for {} returned empty response",
            target_name
        );
        None
    } else {
        info!("Distilled ego for {} ({} chars)", target_name, text.len());
        Some(text)
    }
}

/// Write an ego file to a layer's workspace.
pub fn write_ego(workspace: &Path, layer_dir: &str, ego: &str) -> std::io::Result<()> {
    let path = workspace.join(layer_dir).join("ego.md");
    std::fs::write(&path, ego)?;
    info!("Wrote ego to {}", path.display());
    Ok(())
}

/// Read an ego file from a layer's workspace.
pub fn read_ego(workspace: &Path, layer_dir: &str) -> Option<String> {
    let path = workspace.join(layer_dir).join("ego.md");
    let content = std::fs::read_to_string(&path).ok()?;
    if content.trim().is_empty() {
        None
    } else {
        Some(content)
    }
}

/// Distill all egos for a full stack wake.
///
/// Each watcher distills the ego of the layer it watches:
///   L1 → L0's ego
///   L2 → L1's ego
///   L3 → L2's ego
///   Core → L3's ego
///   Core → Core's own ego (self-distill)
///
/// Returns: [L0_ego, L1_ego, L2_ego, L3_ego, core_ego]
pub async fn distill_all_egos(
    workspace: &Path,
    api_key: &str,
    config: &ConsciousnessConfig,
) -> [Option<String>; 5] {
    let mut egos: [Option<String>; 5] = Default::default();

    // L1 distills L0's ego (L1 watches L0, L1 knows L0 best)
    let l1_sessions = workspace.join("L1").join(".agenticlaw").join("sessions");
    if let Some(ego) = distill_ego(
        api_key,
        &config.models.l1,
        &l1_sessions,
        "L0",
        &config.ego.l1_distill_prompt,
        config.ego.layer_budget_chars,
        config.ego.l1_distill_budget,
    )
    .await
    {
        let _ = write_ego(workspace, "L0", &ego);
        egos[0] = Some(ego);
    }

    // L2 distills L1's ego (L2 watches L1)
    let l2_sessions = workspace.join("L2").join(".agenticlaw").join("sessions");
    if let Some(ego) = distill_ego(
        api_key,
        &config.models.l2,
        &l2_sessions,
        "L1",
        &config.ego.l2_distill_prompt,
        config.ego.layer_budget_chars,
        config.ego.l2_distill_budget,
    )
    .await
    {
        let _ = write_ego(workspace, "L1", &ego);
        egos[1] = Some(ego);
    }

    // L3 distills L2's ego (L3 watches L2)
    let l3_sessions = workspace.join("L3").join(".agenticlaw").join("sessions");
    if let Some(ego) = distill_ego(
        api_key,
        &config.models.l3,
        &l3_sessions,
        "L2",
        &config.ego.l3_distill_prompt,
        config.ego.layer_budget_chars,
        config.ego.l3_distill_budget,
    )
    .await
    {
        let _ = write_ego(workspace, "L2", &ego);
        egos[2] = Some(ego);
    }

    // Warm core distills L3's ego (core watches L3)
    let core_state = workspace.join("core-state.json");
    let warm_core_dir = warm_core_name(&core_state).unwrap_or("core-a");
    let core_sessions = workspace
        .join(warm_core_dir)
        .join(".agenticlaw")
        .join("sessions");

    if let Some(ego) = distill_ego(
        api_key,
        &config.models.core,
        &core_sessions,
        "L3",
        &config.ego.core_distill_prompt,
        config.ego.core_budget_chars,
        config.ego.core_distill_budget,
    )
    .await
    {
        let _ = write_ego(workspace, "L3", &ego);
        egos[3] = Some(ego);
    }

    // Warm core self-distills (for its own wake)
    if let Some(ego) = distill_ego(
        api_key,
        &config.models.core,
        &core_sessions,
        "Core (self)",
        &config.ego.core_self_distill_prompt,
        config.ego.core_budget_chars,
        config.ego.core_self_distill_budget,
    )
    .await
    {
        let _ = write_ego(workspace, warm_core_dir, &ego);
        egos[4] = Some(ego);
    }

    egos
}

/// Distill ego for a layer that just went to sleep.
/// The watcher makes one LLM call to summarize who the layer is (first person),
/// then staples the sleeping layer's .ctx tail paragraphs after it.
/// Writes the result to ego.md. Synchronous — the layer is asleep anyway.
pub async fn distill_layer_ego_on_sleep(
    workspace: &Path,
    layer: usize,
    api_key: &str,
    config: &ConsciousnessConfig,
) -> Option<String> {
    let layer_dirs = ["L0", "L1", "L2", "L3"];
    if layer >= layer_dirs.len() {
        return None;
    }

    // Watcher for each layer: L1→L0, L2→L1, L3→L2
    let (watcher_sessions, prompt, budget, max_tokens) = match layer {
        0 => {
            let s = workspace.join("L1").join(".agenticlaw").join("sessions");
            (
                s,
                &config.ego.l1_distill_prompt,
                config.ego.layer_budget_chars,
                config.ego.l1_distill_budget,
            )
        }
        1 => {
            let s = workspace.join("L2").join(".agenticlaw").join("sessions");
            (
                s,
                &config.ego.l2_distill_prompt,
                config.ego.layer_budget_chars,
                config.ego.l2_distill_budget,
            )
        }
        2 => {
            let s = workspace.join("L3").join(".agenticlaw").join("sessions");
            (
                s,
                &config.ego.l3_distill_prompt,
                config.ego.layer_budget_chars,
                config.ego.l3_distill_budget,
            )
        }
        3 => {
            // L3's watcher is the warm core
            let warm_dir = warm_core_name(&workspace.join("core-state.json")).unwrap_or("core-a");
            let s = workspace
                .join(warm_dir)
                .join(".agenticlaw")
                .join("sessions");
            (
                s,
                &config.ego.core_distill_prompt,
                config.ego.core_budget_chars,
                config.ego.core_distill_budget,
            )
        }
        _ => return None,
    };

    // Determine model from watcher layer
    let model = match layer {
        0 => &config.models.l1,
        1 => &config.models.l2,
        2 => &config.models.l3,
        _ => &config.models.core,
    };

    // 1. Distill the ego summary (first person) from the watcher
    let ego_summary = distill_ego(
        api_key,
        model,
        &watcher_sessions,
        layer_dirs[layer],
        prompt,
        budget,
        max_tokens,
    )
    .await?;

    // 2. Extract tail paragraphs from the sleeping layer's own .ctx
    let sleeping_sessions = workspace
        .join(layer_dirs[layer])
        .join(".agenticlaw")
        .join("sessions");
    let tail = find_latest_ctx(&sleeping_sessions)
        .and_then(|p| std::fs::read_to_string(&p).ok())
        .map(|content| extract_tail_paragraphs(&content, config.ego.tail_paragraphs))
        .unwrap_or_default();

    // 3. Build wake context: ego + tail
    let ego_len = ego_summary.len();
    let tail_len = tail.len();
    let wake_context = if tail.is_empty() {
        ego_summary
    } else {
        format!(
            "{}\n\n--- Recent context ---\n\n{}",
            ego_summary.trim(),
            tail
        )
    };

    let _ = write_ego(workspace, layer_dirs[layer], &wake_context);
    info!(
        "Ego distillation for L{} on sleep complete ({} chars ego + {} chars tail)",
        layer, ego_len, tail_len
    );
    Some(wake_context)
}

/// Distill core's ego on sleep/wake. Core self-distills + staples its own .ctx tail.
pub async fn distill_core_ego_on_sleep(
    workspace: &Path,
    api_key: &str,
    config: &ConsciousnessConfig,
) -> Option<String> {
    let warm_dir = warm_core_name(&workspace.join("core-state.json")).unwrap_or("core-a");
    let core_sessions = workspace
        .join(warm_dir)
        .join(".agenticlaw")
        .join("sessions");

    let ego_summary = distill_ego(
        api_key,
        &config.models.core,
        &core_sessions,
        warm_dir,
        &config.ego.core_self_distill_prompt,
        config.ego.core_budget_chars,
        config.ego.core_self_distill_budget,
    )
    .await?;

    let tail = find_latest_ctx(&core_sessions)
        .and_then(|p| std::fs::read_to_string(&p).ok())
        .map(|content| extract_tail_paragraphs(&content, config.ego.tail_paragraphs))
        .unwrap_or_default();

    let wake_context = if tail.is_empty() {
        ego_summary
    } else {
        format!(
            "{}\n\n--- Recent context ---\n\n{}",
            ego_summary.trim(),
            tail
        )
    };

    let _ = write_ego(workspace, warm_dir, &wake_context);
    info!("Core ego distilled on sleep ({} chars)", wake_context.len());
    Some(wake_context)
}

/// Determine which core is warm (Growing phase).
fn warm_core_name(state_path: &Path) -> Option<&'static str> {
    let content = std::fs::read_to_string(state_path).ok()?;
    let state: serde_json::Value = serde_json::from_str(&content).ok()?;

    if state
        .get("core_a")
        .and_then(|c| c.get("phase"))
        .and_then(|p| p.as_str())
        == Some("Growing")
    {
        Some("core-a")
    } else if state
        .get("core_b")
        .and_then(|c| c.get("phase"))
        .and_then(|p| p.as_str())
        == Some("Growing")
    {
        Some("core-b")
    } else {
        Some("core-a") // fallback
    }
}
