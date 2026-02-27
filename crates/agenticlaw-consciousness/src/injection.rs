//! Injection mechanism — lower layers inject insights into higher layers
//!
//! When a lower layer produces output, it checks NLP correlation with the
//! gateway's recent context. If correlated, it writes an injection file
//! that the gateway reads before its next API call.
//!
//! v2: UUID-based filenames, tag-free content, atomic read-and-clear,
//!     supports core-a/core-b as injection sources.

use crate::cores::CoreId;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Directory where injection files live
pub fn injection_dir(workspace: &Path) -> PathBuf {
    workspace.join("injections")
}

/// Directory for in-progress reads (atomic read-and-clear)
fn in_progress_dir(workspace: &Path) -> PathBuf {
    workspace.join("injections").join(".in-progress")
}

/// Find a safe UTF-8 boundary at or before the given byte index.
fn safe_byte_boundary(s: &str, byte_idx: usize) -> usize {
    if byte_idx >= s.len() {
        return s.len();
    }
    let mut idx = byte_idx;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Write an injection from a layer (L2, L3).
/// Tag-free: source is logged at INFO but not written into the file.
pub fn write_layer_injection(workspace: &Path, from_layer: usize, content: &str, max_chars: usize) -> std::io::Result<()> {
    let dir = injection_dir(workspace);
    fs::create_dir_all(&dir)?;

    let bounded = if content.len() > max_chars {
        let boundary = safe_byte_boundary(content, max_chars);
        &content[..boundary]
    } else {
        content
    };

    let injection = bounded.trim().to_string();
    info!("Injection from L{}: {} chars", from_layer, injection.len());

    let filename = format!("inject-{}.txt", uuid::Uuid::new_v4());
    let path = dir.join(filename);

    debug!("Writing injection file: {}", path.display());
    fs::write(&path, &injection)?;
    Ok(())
}

/// Write an injection from a core (Core-A, Core-B).
/// Tag-free: source is logged at INFO but not written into the file.
pub fn write_injection(workspace: &Path, core_id: CoreId, content: &str, max_chars: usize) -> std::io::Result<()> {
    let dir = injection_dir(workspace);
    fs::create_dir_all(&dir)?;

    let bounded = if content.len() > max_chars {
        let boundary = safe_byte_boundary(content, max_chars);
        &content[..boundary]
    } else {
        content
    };

    let injection = bounded.trim().to_string();
    info!("Injection from {}: {} chars", core_id.dir_name(), injection.len());

    let filename = format!("inject-{}.txt", uuid::Uuid::new_v4());
    let path = dir.join(filename);

    debug!("Writing injection file: {}", path.display());
    fs::write(&path, &injection)?;
    Ok(())
}

/// Read all pending injections for L0 (gateway).
/// Atomic read-and-clear: rename files to in-progress dir, read, then delete.
/// Returns concatenated injection text.
pub fn read_and_clear_injections(workspace: &Path) -> String {
    let dir = injection_dir(workspace);
    if !dir.is_dir() {
        return String::new();
    }

    let progress_dir = in_progress_dir(workspace);
    let _ = fs::create_dir_all(&progress_dir);

    let mut injections = Vec::new();

    // Read all inject-*.txt files
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.starts_with("inject-") || !name.ends_with(".txt") {
            continue;
        }

        // Atomic: rename to in-progress, then read, then delete
        let progress_path = progress_dir.join(&name);
        if fs::rename(&path, &progress_path).is_err() {
            // File might have been consumed by another read
            continue;
        }

        match fs::read_to_string(&progress_path) {
            Ok(content) if !content.trim().is_empty() => {
                injections.push(content.trim().to_string());
            }
            Ok(_) => {}
            Err(e) => {
                warn!("Failed to read injection {}: {}", name, e);
            }
        }

        let _ = fs::remove_file(&progress_path);
    }

    // Also clean up any stale in-progress files (from crashed reads)
    if let Ok(entries) = fs::read_dir(&progress_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if let Ok(content) = fs::read_to_string(&path) {
                if !content.trim().is_empty() {
                    injections.push(content.trim().to_string());
                }
            }
            let _ = fs::remove_file(&path);
        }
    }

    if injections.is_empty() {
        return String::new();
    }

    info!("Injecting {} insights into gateway context", injections.len());
    format!("\n--- consciousness injections ---\n{}\n--- end injections ---\n", injections.join("\n"))
}

/// Simple NLP correlation check: do the two texts share significant terms?
/// Returns a correlation score 0.0-1.0.
pub fn correlation_score(text_a: &str, text_b: &str) -> f64 {
    let words_a: std::collections::HashSet<&str> = text_a
        .split_whitespace()
        .filter(|w| w.len() > 3) // skip short words
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();

    let words_b: std::collections::HashSet<&str> = text_b
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();

    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    intersection as f64 / union as f64
}
