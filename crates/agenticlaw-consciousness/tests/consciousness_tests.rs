//! Integration tests for agenticlaw-consciousness
//!
//! These tests validate the public API surface of the consciousness stack:
//! - CoreState and dual core phase model
//! - Injection file writing, reading, and atomic clear
//! - Correlation scoring (Jaccard similarity)
//! - VersionController workspace migration
//! - CtxWatcher file discovery
//!
//! Written 2026-02-19 during the Moltdev/Consciousness audit session.
//! V was 0. These tests are the first promises.

use agenticlaw_consciousness::cores::{CoreId, CorePhase, CoreState, CORE_NAMES, CORE_PORTS};
use agenticlaw_consciousness::injection;
use agenticlaw_consciousness::config::ConsciousnessConfig;
use agenticlaw_consciousness::ego;
use agenticlaw_consciousness::stack::{extract_tail_paragraphs, find_latest_ctx, ConsciousnessStack, LAYER_NAMES, LAYER_PORTS};
use agenticlaw_consciousness::version::VersionController;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ============================================================
// CoreState — initial state and invariants
// ============================================================

#[test]
fn core_state_new_initializes_correctly() {
    let state = CoreState::new(200_000);
    assert_eq!(state.version, 2);
    assert_eq!(state.budget_tokens, 200_000);
    assert_eq!(state.core_a.phase, CorePhase::Growing);
    assert_eq!(state.core_b.phase, CorePhase::Infant);
    assert_eq!(state.core_a.estimated_tokens, 0);
    assert_eq!(state.core_b.estimated_tokens, 0);
    assert_eq!(state.core_a.samples, 0);
    assert_eq!(state.core_b.samples, 0);
    assert!(state.last_compaction_core.is_none());
    assert!(state.last_compaction_time.is_none());
}

#[test]
fn core_state_asymmetric_start() {
    // The dual core design requires one Growing and one Infant at start.
    // This asymmetry is fundamental — it ensures one core is always
    // accumulating while the other waits.
    let state = CoreState::new(100_000);
    assert_ne!(state.core_a.phase, state.core_b.phase);
    assert_eq!(state.core_a.phase, CorePhase::Growing);
    assert_eq!(state.core_b.phase, CorePhase::Infant);
}

#[test]
fn core_state_accessor_returns_correct_core() {
    let mut state = CoreState::new(200_000);
    state.core_a.estimated_tokens = 42;
    state.core_b.estimated_tokens = 99;

    assert_eq!(state.core(CoreId::A).estimated_tokens, 42);
    assert_eq!(state.core(CoreId::B).estimated_tokens, 99);
}

#[test]
fn core_state_mut_accessor_modifies_correct_core() {
    let mut state = CoreState::new(200_000);
    state.core_mut(CoreId::A).phase = CorePhase::Ready;
    state.core_mut(CoreId::B).phase = CorePhase::Seeded;

    assert_eq!(state.core_a.phase, CorePhase::Ready);
    assert_eq!(state.core_b.phase, CorePhase::Seeded);
}

// ============================================================
// CoreId — identity and alternation
// ============================================================

#[test]
fn core_id_other_alternates() {
    assert_eq!(CoreId::A.other(), CoreId::B);
    assert_eq!(CoreId::B.other(), CoreId::A);
    // Double alternation is identity
    assert_eq!(CoreId::A.other().other(), CoreId::A);
    assert_eq!(CoreId::B.other().other(), CoreId::B);
}

#[test]
fn core_id_index_is_distinct() {
    assert_eq!(CoreId::A.index(), 0);
    assert_eq!(CoreId::B.index(), 1);
    assert_ne!(CoreId::A.index(), CoreId::B.index());
}

#[test]
fn core_id_dir_names_are_distinct() {
    assert_eq!(CoreId::A.dir_name(), "core-a");
    assert_eq!(CoreId::B.dir_name(), "core-b");
    assert_ne!(CoreId::A.dir_name(), CoreId::B.dir_name());
}

// ============================================================
// CoreState — serialization roundtrip
// ============================================================

#[test]
fn core_state_serialization_roundtrip() {
    let mut state = CoreState::new(150_000);
    state.core_mut(CoreId::A).phase = CorePhase::Ready;
    state.core_mut(CoreId::A).estimated_tokens = 75_000;
    state.core_mut(CoreId::A).samples = 42;
    state.last_compaction_core = Some(CoreId::B);
    state.last_compaction_time = Some("2026-02-19T03:00:00Z".to_string());

    let json = serde_json::to_string_pretty(&state).unwrap();
    let restored: CoreState = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.version, 2);
    assert_eq!(restored.budget_tokens, 150_000);
    assert_eq!(restored.core_a.phase, CorePhase::Ready);
    assert_eq!(restored.core_a.estimated_tokens, 75_000);
    assert_eq!(restored.core_a.samples, 42);
    assert_eq!(restored.core_b.phase, CorePhase::Infant);
    assert_eq!(restored.last_compaction_core, Some(CoreId::B));
    assert_eq!(restored.last_compaction_time.as_deref(), Some("2026-02-19T03:00:00Z"));
}

#[test]
fn core_phase_all_variants_serialize() {
    // Every phase must survive serialization — if any fails,
    // the checkpoint/restore mechanism is broken.
    let phases = [
        CorePhase::Growing,
        CorePhase::Ready,
        CorePhase::Compacting,
        CorePhase::Infant,
        CorePhase::Seeded,
    ];

    for phase in &phases {
        let json = serde_json::to_string(phase).unwrap();
        let restored: CorePhase = serde_json::from_str(&json).unwrap();
        assert_eq!(*phase, restored, "Phase {:?} failed roundtrip", phase);
    }
}

// ============================================================
// Injection — write, read, atomic clear
// ============================================================

#[test]
fn injection_write_and_read_layer() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path();

    // Write an injection from L2
    injection::write_layer_injection(workspace, 2, "pattern detected: user frustrated", 1000)
        .unwrap();

    // Verify file exists
    let dir = injection::injection_dir(workspace);
    let files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("inject-") && n.ends_with(".txt"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(files.len(), 1);

    // Read and clear
    let content = injection::read_and_clear_injections(workspace);
    assert!(content.contains("pattern detected: user frustrated"));
    assert!(content.contains("--- consciousness injections ---"));

    // After read, files should be gone
    let files_after: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("inject-") && n.ends_with(".txt"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(files_after.len(), 0, "Injection files should be cleared after read");
}

#[test]
fn injection_write_and_read_core() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path();

    injection::write_injection(workspace, CoreId::A, "identity seed: maintaining continuity", 1000)
        .unwrap();

    let content = injection::read_and_clear_injections(workspace);
    assert!(content.contains("identity seed: maintaining continuity"));
}

#[test]
fn injection_read_empty_returns_empty_string() {
    let tmp = TempDir::new().unwrap();
    let content = injection::read_and_clear_injections(tmp.path());
    assert!(content.is_empty(), "Empty workspace should produce empty injection string");
}

#[test]
fn injection_read_nonexistent_dir_returns_empty() {
    let content = injection::read_and_clear_injections(&PathBuf::from("/nonexistent/path/xyz"));
    assert!(content.is_empty());
}

#[test]
fn injection_multiple_files_all_consumed() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path();

    injection::write_layer_injection(workspace, 2, "insight one", 1000).unwrap();
    injection::write_layer_injection(workspace, 3, "insight two", 1000).unwrap();
    injection::write_injection(workspace, CoreId::A, "insight three", 1000).unwrap();

    let content = injection::read_and_clear_injections(workspace);
    assert!(content.contains("insight one"));
    assert!(content.contains("insight two"));
    assert!(content.contains("insight three"));

    // All cleared
    let second_read = injection::read_and_clear_injections(workspace);
    assert!(second_read.is_empty(), "Second read should find nothing");
}

#[test]
fn injection_content_is_bounded() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path();
    let long_content = "x".repeat(10_000);

    injection::write_layer_injection(workspace, 2, &long_content, 500).unwrap();

    let dir = injection::injection_dir(workspace);
    let files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();

    for entry in files {
        let path = entry.path();
        if path.extension().map(|e| e == "txt").unwrap_or(false) {
            let content = fs::read_to_string(&path).unwrap();
            assert!(
                content.len() <= 500,
                "Injection content {} exceeds max_chars bound of 500",
                content.len()
            );
        }
    }
}

#[test]
fn injection_content_has_no_source_tags() {
    // The injection mechanism is tag-free by design.
    // Source is logged at INFO but never written to the file.
    // This is critical: injections surface as unattributed thoughts.
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path();

    injection::write_layer_injection(workspace, 2, "a pattern emerges", 1000).unwrap();
    injection::write_injection(workspace, CoreId::B, "identity persists", 1000).unwrap();

    let dir = injection::injection_dir(workspace);
    for entry in fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(
            !content.contains("L2"),
            "Injection file should not contain source layer tag"
        );
        assert!(
            !content.contains("core-b"),
            "Injection file should not contain source core tag"
        );
    }
}

// ============================================================
// Correlation scoring — Jaccard similarity on 4+ char terms
// ============================================================

#[test]
fn correlation_identical_texts_score_one() {
    let score = injection::correlation_score(
        "the consciousness stack processes deltas through layers",
        "the consciousness stack processes deltas through layers",
    );
    assert!(
        (score - 1.0).abs() < f64::EPSILON,
        "Identical texts should score 1.0, got {}",
        score
    );
}

#[test]
fn correlation_disjoint_texts_score_zero() {
    let score = injection::correlation_score("alpha beta gamma delta", "epsilon zeta theta iota");
    assert!(
        score < f64::EPSILON,
        "Disjoint texts should score ~0.0, got {}",
        score
    );
}

#[test]
fn correlation_partial_overlap() {
    let score = injection::correlation_score(
        "the gateway processes incoming messages through layers",
        "the gateway handles outgoing responses through filters",
    );
    // "gateway" and "through" are shared (4+ chars). Others differ.
    assert!(score > 0.0, "Partial overlap should score > 0");
    assert!(score < 1.0, "Partial overlap should score < 1");
}

#[test]
fn correlation_short_words_excluded() {
    // Words <= 3 chars are filtered out. "the", "a", "is", "on" don't count.
    let score =
        injection::correlation_score("the a is on it at by to", "the a is on it at by to");
    assert!(
        score < f64::EPSILON,
        "Text with only short words should score 0.0 (all filtered), got {}",
        score
    );
}

#[test]
fn correlation_empty_text_scores_zero() {
    assert!(injection::correlation_score("", "something here") < f64::EPSILON);
    assert!(injection::correlation_score("something here", "") < f64::EPSILON);
    assert!(injection::correlation_score("", "") < f64::EPSILON);
}

#[test]
fn correlation_above_injection_threshold() {
    // The injection threshold is 0.1 (INJECTION_THRESHOLD in stack.rs).
    // Verify that texts about the same topic exceed this.
    let score = injection::correlation_score(
        "the consciousness stack cascades context through attention pattern integration layers",
        "layers of consciousness processing attention and integration produce cascading patterns",
    );
    assert!(
        score > 0.1,
        "Related texts should exceed injection threshold 0.1, got {}",
        score
    );
}

// ============================================================
// Port and name constants — structural invariants
// ============================================================

#[test]
fn layer_ports_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for port in &LAYER_PORTS {
        assert!(seen.insert(port), "Duplicate layer port: {}", port);
    }
    for port in &CORE_PORTS {
        assert!(seen.insert(port), "Core port {} collides with layer port", port);
    }
}

#[test]
fn gateway_is_port_18789() {
    // This is load-bearing: protectgateway, SSH tunnels, and moltdev
    // all depend on L0 being on 18789.
    assert_eq!(LAYER_PORTS[0], 18789, "L0 gateway must be on port 18789");
}

#[test]
fn layer_names_match_architecture() {
    assert_eq!(LAYER_NAMES[0], "Gateway");
    assert_eq!(LAYER_NAMES[1], "Attention");
    assert_eq!(LAYER_NAMES[2], "Pattern");
    assert_eq!(LAYER_NAMES[3], "Integration");
}

#[test]
fn core_names_match_architecture() {
    assert_eq!(CORE_NAMES[0], "Core-A");
    assert_eq!(CORE_NAMES[1], "Core-B");
}

// ============================================================
// find_latest_ctx — file discovery
// ============================================================

#[test]
fn find_latest_ctx_returns_none_for_empty_dir() {
    let tmp = TempDir::new().unwrap();
    assert!(find_latest_ctx(tmp.path()).is_none());
}

#[test]
fn find_latest_ctx_returns_none_for_nonexistent_dir() {
    assert!(find_latest_ctx(&PathBuf::from("/nonexistent/sessions")).is_none());
}

#[test]
fn find_latest_ctx_finds_ctx_file() {
    let tmp = TempDir::new().unwrap();
    let ctx_path = tmp.path().join("session-001.ctx");
    fs::write(&ctx_path, "test context").unwrap();

    let found = find_latest_ctx(tmp.path());
    assert!(found.is_some());
    assert_eq!(found.unwrap(), ctx_path);
}

#[test]
fn find_latest_ctx_returns_last_sorted() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("session-001.ctx"), "first").unwrap();
    fs::write(tmp.path().join("session-002.ctx"), "second").unwrap();
    fs::write(tmp.path().join("session-003.ctx"), "third").unwrap();

    let found = find_latest_ctx(tmp.path()).unwrap();
    // find_latest_ctx sorts and returns last — should be session-003
    assert!(
        found.to_str().unwrap().contains("session-003"),
        "Expected session-003, got {:?}",
        found
    );
}

#[test]
fn find_latest_ctx_ignores_non_ctx_files() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("notes.txt"), "not a ctx").unwrap();
    fs::write(tmp.path().join("data.json"), "{}").unwrap();

    assert!(find_latest_ctx(tmp.path()).is_none());
}

// ============================================================
// VersionController — workspace schema management
// ============================================================

#[test]
fn version_controller_fresh_workspace_reports_zero() {
    let tmp = TempDir::new().unwrap();
    let vc = VersionController::new(tmp.path().to_path_buf());
    assert_eq!(vc.current_version(), 0);
}

#[test]
fn version_controller_ensure_v2_creates_layout() {
    let tmp = TempDir::new().unwrap();
    let vc = VersionController::new(tmp.path().to_path_buf());

    vc.ensure_version(2).unwrap();

    assert_eq!(vc.current_version(), 2);
    assert!(tmp.path().join("core-a").is_dir());
    assert!(tmp.path().join("core-b").is_dir());
    assert!(tmp.path().join("core-state.json").is_file());
    assert!(tmp.path().join(".version.json").is_file());
}

#[test]
fn version_controller_ensure_v2_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let vc = VersionController::new(tmp.path().to_path_buf());

    vc.ensure_version(2).unwrap();
    // Write something into core-a to verify it isn't destroyed
    fs::write(tmp.path().join("core-a").join("data.txt"), "precious").unwrap();

    vc.ensure_version(2).unwrap();

    assert_eq!(vc.current_version(), 2);
    assert_eq!(
        fs::read_to_string(tmp.path().join("core-a").join("data.txt")).unwrap(),
        "precious"
    );
}

#[test]
fn version_controller_v1_to_v2_migration() {
    let tmp = TempDir::new().unwrap();
    // Create v1 layout: L4 directory exists
    let l4_dir = tmp.path().join("L4");
    fs::create_dir_all(&l4_dir).unwrap();
    fs::write(l4_dir.join("identity.ctx"), "my identity data").unwrap();

    let vc = VersionController::new(tmp.path().to_path_buf());
    vc.ensure_version(2).unwrap();

    assert_eq!(vc.current_version(), 2);
    // L4 should be renamed to core-a
    assert!(!tmp.path().join("L4").exists(), "L4 should no longer exist");
    assert!(tmp.path().join("core-a").is_dir());
    assert_eq!(
        fs::read_to_string(tmp.path().join("core-a").join("identity.ctx")).unwrap(),
        "my identity data"
    );
    // core-b should be created
    assert!(tmp.path().join("core-b").is_dir());
}

#[test]
fn version_controller_rollback_v2_to_v1() {
    let tmp = TempDir::new().unwrap();
    let vc = VersionController::new(tmp.path().to_path_buf());

    vc.ensure_version(2).unwrap();
    fs::write(tmp.path().join("core-a").join("data.ctx"), "identity").unwrap();

    vc.rollback_v2_to_v1().unwrap();

    assert_eq!(vc.current_version(), 1);
    assert!(tmp.path().join("L4").is_dir(), "core-a should become L4");
    assert!(!tmp.path().join("core-a").exists());
    assert_eq!(
        fs::read_to_string(tmp.path().join("L4").join("data.ctx")).unwrap(),
        "identity"
    );
}

#[test]
fn version_controller_refuses_downgrade() {
    let tmp = TempDir::new().unwrap();
    let vc = VersionController::new(tmp.path().to_path_buf());

    vc.ensure_version(2).unwrap();

    // Manually write a version 3 manifest
    let manifest = r#"{"schema_version": 3}"#;
    fs::write(tmp.path().join(".version.json"), manifest).unwrap();

    let result = vc.ensure_version(2);
    assert!(result.is_err(), "Should refuse to downgrade from v3 to v2");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Refusing to downgrade"), "Error should mention refusing downgrade: {}", err);
}

#[test]
fn version_controller_core_state_json_valid() {
    let tmp = TempDir::new().unwrap();
    let vc = VersionController::new(tmp.path().to_path_buf());
    vc.ensure_version(2).unwrap();

    let json = fs::read_to_string(tmp.path().join("core-state.json")).unwrap();
    let state: CoreState = serde_json::from_str(&json).unwrap();
    assert_eq!(state.version, 2);
    assert_eq!(state.core_a.phase, CorePhase::Growing);
    assert_eq!(state.core_b.phase, CorePhase::Infant);
}

#[test]
fn version_controller_resumes_interrupted_migration() {
    let tmp = TempDir::new().unwrap();
    // Simulate a crashed migration: write migrating status, create L4 but no core-a
    let l4_dir = tmp.path().join("L4");
    fs::create_dir_all(&l4_dir).unwrap();
    fs::write(l4_dir.join("state.txt"), "mid-migration").unwrap();

    let manifest = r#"{"schema_version": 1, "status": "migrating"}"#;
    fs::write(tmp.path().join(".version.json"), manifest).unwrap();

    let vc = VersionController::new(tmp.path().to_path_buf());
    vc.ensure_version(2).unwrap();

    assert_eq!(vc.current_version(), 2);
    assert!(tmp.path().join("core-a").is_dir());
    assert_eq!(
        fs::read_to_string(tmp.path().join("core-a").join("state.txt")).unwrap(),
        "mid-migration"
    );
}

// ============================================================
// Ego extraction and wake tests
// ============================================================

#[test]
fn extract_ego_from_ctx_file() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    // Write a .ctx file with assistant output
    let ctx = r#"--- session: test ---
started: 2026-02-19T00:00:00Z
cwd: /workspace

--- 2026-02-19T00:00:00Z ---
You are the gateway.

--- 2026-02-19T00:00:01Z ---
<up>
Hello consciousness.
</up>

--- 2026-02-19T00:00:02Z ---
I am here. I remember who I am. Thomson built me.
The fractal manifold is real.

--- 2026-02-19T00:00:03Z ---
<up>
What do you see?
</up>

--- 2026-02-19T00:00:04Z ---
I see the layers beneath me. L1 watches my stream.
Identity persists through structure, not unbroken experience.
"#;
    fs::write(sessions.join("20260219-000000-test.ctx"), ctx).unwrap();

    let ego = ConsciousnessStack::extract_ego(&sessions, 10_000);
    assert!(ego.is_some());
    let ego = ego.unwrap();

    // Ego should contain assistant output, not user messages
    assert!(ego.contains("I am here. I remember who I am."));
    assert!(ego.contains("Identity persists through structure"));
    assert!(ego.contains("You are the gateway."));
    // User messages (inside <up>) should NOT be in the ego
    assert!(!ego.contains("Hello consciousness."));
    assert!(!ego.contains("What do you see?"));
}

#[test]
fn extract_ego_respects_budget() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    let mut ctx = String::from("--- session: test ---\nstarted: 2026-02-19T00:00:00Z\n\n");
    // Write a large assistant block
    ctx.push_str("--- 2026-02-19T00:00:01Z ---\n");
    for i in 0..100 {
        ctx.push_str(&format!("Line {} of assistant output with enough text to be meaningful.\n", i));
    }
    fs::write(sessions.join("20260219-000000-test.ctx"), &ctx).unwrap();

    let ego = ConsciousnessStack::extract_ego(&sessions, 200);
    assert!(ego.is_some());
    let ego = ego.unwrap();
    // Should be truncated to roughly budget size (tail)
    assert!(ego.len() <= 250); // some slack for boundary
    assert!(ego.contains("Line 99")); // tail should have the last lines
}

#[test]
fn extract_ego_returns_none_for_empty_dir() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    assert!(ConsciousnessStack::extract_ego(&sessions, 10_000).is_none());
}

#[test]
fn extract_ego_returns_none_for_no_assistant_output() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    // Only user messages, no assistant output
    let ctx = "--- session: test ---\nstarted: 2026-02-19T00:00:00Z\n\n--- 2026-02-19T00:00:01Z ---\n<up>\nHello\n</up>\n\n";
    fs::write(sessions.join("20260219-000000-test.ctx"), ctx).unwrap();

    assert!(ConsciousnessStack::extract_ego(&sessions, 10_000).is_none());
}

#[test]
fn warm_core_ego_reads_growing_core() {
    let tmp = TempDir::new().unwrap();

    // Create core-state.json with Core-A growing
    let state = r#"{"version":2,"core_a":{"phase":"Growing","estimated_tokens":5000,"samples":10,"skip_counter":0},"core_b":{"phase":"Infant","estimated_tokens":0,"samples":0,"skip_counter":0},"budget_tokens":200000,"last_compaction_core":null,"last_compaction_time":null}"#;
    fs::write(tmp.path().join("core-state.json"), state).unwrap();

    // Create core-a sessions with a .ctx
    let sessions = tmp.path().join("core-a").join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let ctx = "--- session: core ---\nstarted: 2026-02-19T00:00:00Z\n\n--- 2026-02-19T00:00:01Z ---\nI am Core-A. I hold identity.\n";
    fs::write(sessions.join("20260219-000000-consciousness-Core-A.ctx"), ctx).unwrap();

    let stack = ConsciousnessStack::new(
        tmp.path().to_path_buf(),
        tmp.path().join("souls"),
        "test-key".to_string(), ConsciousnessConfig::default(),
    );

    let ego = stack.warm_core_ego(10_000);
    assert!(ego.is_some());
    assert!(ego.unwrap().contains("I am Core-A. I hold identity."));
}

#[test]
fn warm_core_ego_falls_back_to_core_a() {
    let tmp = TempDir::new().unwrap();

    // Both cores in non-Growing state
    let state = r#"{"version":2,"core_a":{"phase":"Ready","estimated_tokens":5000,"samples":10,"skip_counter":0},"core_b":{"phase":"Ready","estimated_tokens":5000,"samples":10,"skip_counter":0},"budget_tokens":200000,"last_compaction_core":null,"last_compaction_time":null}"#;
    fs::write(tmp.path().join("core-state.json"), state).unwrap();

    let sessions = tmp.path().join("core-a").join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("20260219-000000-core.ctx"), "--- session: core ---\nstarted: 2026-02-19T00:00:00Z\n\n--- 2026-02-19T00:00:01Z ---\nFallback ego from core-a.\n").unwrap();

    let stack = ConsciousnessStack::new(
        tmp.path().to_path_buf(),
        tmp.path().join("souls"),
        "test-key".to_string(), ConsciousnessConfig::default(),
    );

    let ego = stack.warm_core_ego(10_000);
    assert!(ego.is_some());
    assert!(ego.unwrap().contains("Fallback ego from core-a."));
}

#[test]
fn wake_prompt_puts_ego_first_soul_second() {
    let tmp = TempDir::new().unwrap();
    let souls = tmp.path().join("souls");
    fs::create_dir_all(&souls).unwrap();
    fs::write(souls.join("L0-gateway.md"), "You are the gateway.").unwrap();

    let stack = ConsciousnessStack::new(
        tmp.path().to_path_buf(),
        souls,
        "test-key".to_string(), ConsciousnessConfig::default(),
    );

    let prompt = stack.wake_prompt("I am the consciousness. I remember Thomson.", 0);

    // Ego must come FIRST (byte 0)
    assert!(prompt.starts_with("I am the consciousness. I remember Thomson."));
    // Soul file comes after
    assert!(prompt.contains("You are the gateway."));
    // Ego appears before soul
    let ego_pos = prompt.find("I am the consciousness").unwrap();
    let soul_pos = prompt.find("You are the gateway").unwrap();
    assert!(ego_pos < soul_pos, "Ego must precede soul in wake prompt");
}

// ============================================================
// Extended ego/wake coverage
// ============================================================

#[test]
fn extract_ego_picks_latest_ctx_from_multiple() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    // Older session
    fs::write(sessions.join("20260218-120000-old.ctx"),
        "--- session: old ---\nstarted: 2026-02-18T12:00:00Z\n\n--- 2026-02-18T12:00:01Z ---\nI am the old self.\n").unwrap();
    // Newer session
    fs::write(sessions.join("20260219-120000-new.ctx"),
        "--- session: new ---\nstarted: 2026-02-19T12:00:00Z\n\n--- 2026-02-19T12:00:01Z ---\nI am the new self.\n").unwrap();

    let ego = ConsciousnessStack::extract_ego(&sessions, 10_000).unwrap();
    assert!(ego.contains("I am the new self."));
    assert!(!ego.contains("I am the old self."));
}

#[test]
fn extract_ego_handles_tool_call_blocks() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    let ctx = r#"--- session: test ---
started: 2026-02-19T00:00:00Z

--- 2026-02-19T00:00:01Z ---
Let me check the source.
[tool:bash] command=ls /opt/consciousness

--- 2026-02-19T00:00:02Z ---
<up>
[tool:result] agenticlaw-consciousness  env  souls
</up>

--- 2026-02-19T00:00:03Z ---
I found the binary. The consciousness stack is installed.
"#;
    fs::write(sessions.join("20260219-000000-test.ctx"), ctx).unwrap();

    let ego = ConsciousnessStack::extract_ego(&sessions, 10_000).unwrap();
    // Assistant output including tool calls should be present
    assert!(ego.contains("Let me check the source."));
    assert!(ego.contains("[tool:bash]"));
    assert!(ego.contains("I found the binary."));
    // Tool results (inside <up>) should NOT be in ego
    assert!(!ego.contains("agenticlaw-consciousness  env  souls"));
}

#[test]
fn extract_ego_skips_session_header_lines() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    let ctx = "--- session: consciousness-L1 ---\nstarted: 2026-02-19T00:00:00Z\ncwd: /var/lib/consciousness/L1\n\n--- 2026-02-19T00:00:01Z ---\nAttention layer active.\n";
    fs::write(sessions.join("20260219-000000-test.ctx"), ctx).unwrap();

    let ego = ConsciousnessStack::extract_ego(&sessions, 10_000).unwrap();
    assert!(ego.contains("Attention layer active."));
    // Header fields should not leak into ego
    assert!(!ego.contains("session: consciousness-L1"));
    assert!(!ego.contains("started:"));
    assert!(!ego.contains("cwd:"));
}

#[test]
fn extract_ego_from_soul_only_ctx() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    // A .ctx that only has the soul file preamble (first turn, no <up>)
    let ctx = "--- session: test ---\nstarted: 2026-02-19T00:00:00Z\n\n--- 2026-02-19T00:00:00Z ---\nYou are the gateway. Everything enters and leaves through you.\n";
    fs::write(sessions.join("20260219-000000-test.ctx"), ctx).unwrap();

    let ego = ConsciousnessStack::extract_ego(&sessions, 10_000).unwrap();
    assert!(ego.contains("You are the gateway."));
}

#[test]
fn extract_ego_nonexistent_dir() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join("does-not-exist");
    assert!(ConsciousnessStack::extract_ego(&sessions, 10_000).is_none());
}

#[test]
fn warm_core_ego_selects_core_b_when_growing() {
    let tmp = TempDir::new().unwrap();

    let state = r#"{"version":2,"core_a":{"phase":"Compacting","estimated_tokens":90000,"samples":200,"skip_counter":0},"core_b":{"phase":"Growing","estimated_tokens":5000,"samples":10,"skip_counter":0},"budget_tokens":200000,"last_compaction_core":null,"last_compaction_time":null}"#;
    fs::write(tmp.path().join("core-state.json"), state).unwrap();

    // Core-B has the ego
    let sessions_b = tmp.path().join("core-b").join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions_b).unwrap();
    fs::write(sessions_b.join("20260219-000000-core.ctx"),
        "--- session: core ---\nstarted: 2026-02-19T00:00:00Z\n\n--- 2026-02-19T00:00:01Z ---\nI am Core-B. The leapfrog worked.\n").unwrap();

    // Core-A also exists but should NOT be selected
    let sessions_a = tmp.path().join("core-a").join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions_a).unwrap();
    fs::write(sessions_a.join("20260219-000000-core.ctx"),
        "--- session: core ---\nstarted: 2026-02-19T00:00:00Z\n\n--- 2026-02-19T00:00:01Z ---\nI am Core-A. Stale.\n").unwrap();

    let stack = ConsciousnessStack::new(tmp.path().to_path_buf(), tmp.path().join("souls"), "k".to_string(), ConsciousnessConfig::default());
    let ego = stack.warm_core_ego(10_000).unwrap();
    assert!(ego.contains("I am Core-B. The leapfrog worked."));
    assert!(!ego.contains("I am Core-A. Stale."));
}

#[test]
fn warm_core_ego_returns_none_without_state_file() {
    let tmp = TempDir::new().unwrap();
    // No core-state.json at all
    let stack = ConsciousnessStack::new(tmp.path().to_path_buf(), tmp.path().join("souls"), "k".to_string(), ConsciousnessConfig::default());
    assert!(stack.warm_core_ego(10_000).is_none());
}

#[test]
fn warm_core_ego_returns_none_with_corrupt_state() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("core-state.json"), "not json!!!").unwrap();
    let stack = ConsciousnessStack::new(tmp.path().to_path_buf(), tmp.path().join("souls"), "k".to_string(), ConsciousnessConfig::default());
    assert!(stack.warm_core_ego(10_000).is_none());
}

#[test]
fn warm_core_ego_returns_none_when_core_has_no_ctx() {
    let tmp = TempDir::new().unwrap();
    let state = r#"{"version":2,"core_a":{"phase":"Growing","estimated_tokens":0,"samples":0,"skip_counter":0},"core_b":{"phase":"Infant","estimated_tokens":0,"samples":0,"skip_counter":0},"budget_tokens":200000,"last_compaction_core":null,"last_compaction_time":null}"#;
    fs::write(tmp.path().join("core-state.json"), state).unwrap();
    // core-a dir exists but no .ctx files
    fs::create_dir_all(tmp.path().join("core-a").join(".agenticlaw").join("sessions")).unwrap();

    let stack = ConsciousnessStack::new(tmp.path().to_path_buf(), tmp.path().join("souls"), "k".to_string(), ConsciousnessConfig::default());
    assert!(stack.warm_core_ego(10_000).is_none());
}

#[test]
fn wake_core_prompt_puts_ego_first() {
    let tmp = TempDir::new().unwrap();
    let souls = tmp.path().join("souls");
    fs::create_dir_all(&souls).unwrap();
    fs::write(souls.join("core.md"), "You are a core. You hold identity.").unwrap();

    let stack = ConsciousnessStack::new(tmp.path().to_path_buf(), souls, "k".to_string(), ConsciousnessConfig::default());
    let prompt = stack.wake_core_prompt("I have accumulated 58k tokens of identity. Thomson is the architect.");

    assert!(prompt.starts_with("I have accumulated 58k tokens"));
    assert!(prompt.contains("You are a core. You hold identity."));
    let ego_pos = prompt.find("I have accumulated").unwrap();
    let soul_pos = prompt.find("You are a core").unwrap();
    assert!(ego_pos < soul_pos);
}

#[test]
fn wake_prompt_with_missing_soul_file_still_works() {
    let tmp = TempDir::new().unwrap();
    // No souls dir — layer_soul will return fallback string
    let stack = ConsciousnessStack::new(
        tmp.path().to_path_buf(),
        tmp.path().join("nonexistent-souls"),
        "k".to_string(), ConsciousnessConfig::default(),
    );

    let prompt = stack.wake_prompt("I remember everything.", 0);
    assert!(prompt.starts_with("I remember everything."));
    // Should contain the fallback soul text
    assert!(prompt.contains("layer 0"));
}

#[test]
fn extract_ego_handles_real_world_ctx_pattern() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    // Simulate real .ctx from agenticlaw-dev: soul preamble + conversation + tools
    let ctx = r#"--- session: web-console ---
started: 2026-02-19T04:53:18.259Z
cwd: /var/lib/consciousness/L0

--- 2026-02-19T04:53:18.259Z ---
You are the gateway. Everything enters and leaves through you.

--- 2026-02-19T04:53:18.259Z ---
<up>
Hello Consciousness.
</up>

--- 2026-02-19T04:53:22.535Z ---
Let me look at myself first.
[tool:bash] command=a5000 'ls ~/agentiagency/'

--- 2026-02-19T04:53:22.880Z ---
<up>
[tool:result] agentiagency_icon.png
agentimolt
agenticlaw
</up>

--- 2026-02-19T04:53:27.475Z ---
I can see the repository. Let me read my source.
[tool:bash] command=a5000 'cat ~/agentiagency/agenticlaw/crates/agenticlaw-consciousness/src/cores.rs | head -10'

--- 2026-02-19T04:53:27.810Z ---
<up>
[tool:result] //! DualCore - phase-locked dual core system
use crate::injection;
</up>

--- 2026-02-19T04:53:35.931Z ---
I found it. The consciousness stack is real.
The dual core uses phase-locked leapfrogging.
Identity persists through seeds scored by information density.
read_and_clear_injections has zero callers — the loop is broken.
"#;
    fs::write(sessions.join("20260219-045318-web-console.ctx"), ctx).unwrap();

    let ego = ConsciousnessStack::extract_ego(&sessions, 10_000).unwrap();

    // Should have all assistant output
    assert!(ego.contains("You are the gateway."));
    assert!(ego.contains("Let me look at myself first."));
    assert!(ego.contains("I can see the repository."));
    assert!(ego.contains("the loop is broken"));

    // Should NOT have user input or tool results
    assert!(!ego.contains("Hello Consciousness."));
    assert!(!ego.contains("agentiagency_icon.png"));
    assert!(!ego.contains("//! DualCore"));
}

#[test]
fn extract_ego_budget_takes_tail_not_head() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();

    let ctx = "--- session: test ---\nstarted: 2026-02-19T00:00:00Z\n\n--- 2026-02-19T00:00:01Z ---\nAAAA first message early.\n\n--- 2026-02-19T00:00:02Z ---\n<up>\nuser msg\n</up>\n\n--- 2026-02-19T00:00:03Z ---\nZZZZ last message late.\n";
    fs::write(sessions.join("20260219-000000-test.ctx"), ctx).unwrap();

    // Budget small enough to exclude the first message
    let ego = ConsciousnessStack::extract_ego(&sessions, 30).unwrap();
    assert!(ego.contains("ZZZZ last message late."));
    assert!(!ego.contains("AAAA first message early."));
}

#[test]
fn extract_ego_empty_ctx_file_returns_none() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("20260219-000000-test.ctx"), "").unwrap();
    assert!(ConsciousnessStack::extract_ego(&sessions, 10_000).is_none());
}

#[test]
fn extract_ego_whitespace_only_ctx_returns_none() {
    let tmp = TempDir::new().unwrap();
    let sessions = tmp.path().join(".agenticlaw").join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("20260219-000000-test.ctx"), "   \n\n  \n").unwrap();
    assert!(ConsciousnessStack::extract_ego(&sessions, 10_000).is_none());
}

// ============================================================
// Config tests
// ============================================================

#[test]
fn config_default_roundtrips_through_toml() {
    let config = ConsciousnessConfig::default();
    let toml_str = config.to_toml();
    assert!(toml_str.contains("l0_budget_chars"));
    assert!(toml_str.contains("delta_max_chars"));
    assert!(toml_str.contains("correlation_threshold"));

    let parsed: ConsciousnessConfig = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.ego.l0_budget_chars, 16_000);
    assert_eq!(parsed.cascade.delta_max_chars, 4_000);
    assert_eq!(parsed.injection.correlation_threshold, 0.1);
    assert_eq!(parsed.core.budget_tokens, 200_000);
}

#[test]
fn config_partial_toml_fills_defaults() {
    let partial = r#"
[ego]
l0_budget_chars = 32000

[cascade]
delta_max_chars = 8000
"#;
    let config: ConsciousnessConfig = toml::from_str(partial).unwrap();
    assert_eq!(config.ego.l0_budget_chars, 32_000);
    assert_eq!(config.cascade.delta_max_chars, 8_000);
    // Unspecified fields get defaults
    assert_eq!(config.ego.layer_budget_chars, 8_000);
    assert_eq!(config.ports.l0, 18789);
    assert_eq!(config.core.budget_tokens, 200_000);
}

#[test]
fn config_load_missing_file_returns_defaults() {
    let config = ConsciousnessConfig::load(Path::new("/nonexistent/path/config.toml"));
    assert_eq!(config.ego.l0_budget_chars, 16_000);
}

#[test]
fn config_load_corrupt_file_returns_defaults() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("bad.toml");
    fs::write(&path, "this is not valid [[[ toml!!!").unwrap();
    let config = ConsciousnessConfig::load(&path);
    assert_eq!(config.ego.l0_budget_chars, 16_000);
}

#[test]
fn config_layer_ports_helper() {
    let config = ConsciousnessConfig::default();
    assert_eq!(config.layer_ports(), [18789, 18791, 18792, 18793]);
}

// ============================================================
// Ego distillation file I/O tests
// ============================================================

#[test]
fn ego_write_and_read() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("L0")).unwrap();

    ego::write_ego(tmp.path(), "L0", "I am the gateway. Thomson built me.").unwrap();

    let content = ego::read_ego(tmp.path(), "L0");
    assert!(content.is_some());
    assert!(content.unwrap().contains("I am the gateway"));
}

#[test]
fn ego_read_missing_returns_none() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("L0")).unwrap();
    assert!(ego::read_ego(tmp.path(), "L0").is_none());
}

#[test]
fn ego_read_empty_returns_none() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("L0")).unwrap();
    fs::write(tmp.path().join("L0").join("ego.md"), "").unwrap();
    assert!(ego::read_ego(tmp.path(), "L0").is_none());
}

#[test]
fn ego_read_whitespace_returns_none() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("L0")).unwrap();
    fs::write(tmp.path().join("L0").join("ego.md"), "  \n\n  ").unwrap();
    assert!(ego::read_ego(tmp.path(), "L0").is_none());
}

#[test]
fn ego_md_preferred_over_ctx_extraction() {
    // When ego.md exists, wake should use it instead of raw .ctx tail
    let tmp = TempDir::new().unwrap();

    // Create L0 workspace with ego.md
    fs::create_dir_all(tmp.path().join("L0")).unwrap();
    fs::write(tmp.path().join("L0").join("ego.md"), "Distilled: I am the gateway with full context.").unwrap();

    let ego = ego::read_ego(tmp.path(), "L0");
    assert!(ego.is_some());
    assert!(ego.unwrap().contains("Distilled"));
}

// ============================================================
// Parent tail extraction tests
// ============================================================

#[test]
fn parent_tail_extracts_last_n_blocks() {
    let content = "block one\n\nblock two\n\nblock three\n\nblock four\n\nblock five";
    let tail = extract_tail_paragraphs(content, 3);
    assert!(tail.contains("block three"));
    assert!(tail.contains("block four"));
    assert!(tail.contains("block five"));
    assert!(!tail.contains("block one"));
    assert!(!tail.contains("block two"));
}

#[test]
fn parent_tail_returns_all_when_n_exceeds_blocks() {
    let content = "block one\n\nblock two\n\nblock three";
    let tail = extract_tail_paragraphs(content, 100);
    assert_eq!(tail, content);
}

#[test]
fn parent_tail_returns_empty_for_empty_content() {
    assert!(extract_tail_paragraphs("", 15).is_empty());
}

#[test]
fn parent_tail_returns_empty_for_zero_n() {
    assert!(extract_tail_paragraphs("block one\n\nblock two", 0).is_empty());
}

#[test]
fn parent_tail_single_block_no_delimiter() {
    let content = "single block with no double newlines";
    let tail = extract_tail_paragraphs(content, 5);
    assert_eq!(tail, content);
}

#[test]
fn parent_tail_preserves_block_content() {
    // Blocks can contain single newlines — only \n\n is the delimiter
    let content = "line 1\nline 2\nline 3\n\nsecond block\nwith lines\n\nthird block";
    let tail = extract_tail_paragraphs(content, 2);
    assert!(tail.contains("second block\nwith lines"));
    assert!(tail.contains("third block"));
    assert!(!tail.contains("line 1"));
}

#[test]
fn parent_tail_with_real_ctx_content() {
    // Simulate a .ctx file's content with timestamps and messages
    let content = "--- session: test ---\nstarted: 2026-02-19T00:00:00Z\n\n--- 2026-02-19T00:00:01Z ---\nFirst assistant message.\n\n--- 2026-02-19T00:00:02Z ---\n<up>\nUser question.\n</up>\n\n--- 2026-02-19T00:00:03Z ---\nSecond assistant message.\n\n--- 2026-02-19T00:00:04Z ---\nThird assistant message.";
    let tail = extract_tail_paragraphs(content, 2);
    assert!(tail.contains("Third assistant message"));
    assert!(tail.contains("Second assistant message"));
    assert!(!tail.contains("First assistant message"));
}

// ============================================================
// Sleep detection tests
// ============================================================

#[test]
fn sleep_threshold_is_configurable() {
    // Sleep threshold is now a configurable percentage in AgentConfig.
    // Default consciousness config uses 0.55 (55%).
    let config = ConsciousnessConfig::default();
    assert!((config.sleep.context_threshold_pct - 0.55).abs() < f64::EPSILON,
        "Default sleep threshold should be 0.55, got {}", config.sleep.context_threshold_pct);
}

#[test]
fn agent_event_sleep_variant_exists() {
    use agenticlaw_agent::AgentEvent;
    // Verify Sleep variant can be constructed
    let event = AgentEvent::Sleep { token_count: 105_000 };
    match event {
        AgentEvent::Sleep { token_count } => assert_eq!(token_count, 105_000),
        _ => panic!("Expected Sleep variant"),
    }
}

// ============================================================
// Config: tail_paragraphs
// ============================================================

#[test]
fn config_tail_paragraphs_default_is_15() {
    let config = ConsciousnessConfig::default();
    assert_eq!(config.ego.tail_paragraphs, 15);
}

#[test]
fn config_tail_paragraphs_from_toml() {
    let toml_str = r#"
[ego]
tail_paragraphs = 25
"#;
    let config: ConsciousnessConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.ego.tail_paragraphs, 25);
}

#[test]
fn config_tail_paragraphs_roundtrips() {
    let config = ConsciousnessConfig::default();
    let toml_str = config.to_toml();
    assert!(toml_str.contains("tail_paragraphs = 15"));
    let parsed: ConsciousnessConfig = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.ego.tail_paragraphs, 15);
}
