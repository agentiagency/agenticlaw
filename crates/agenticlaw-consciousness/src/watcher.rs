//! File-change watcher for .ctx files
//!
//! Polls .ctx file sizes to detect changes. Fires callbacks on size increase.
//! Uses byte offset tracking to extract only new content (the delta).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info};

/// A change event: new bytes appended to a .ctx file
#[derive(Debug, Clone)]
pub struct CtxChange {
    pub layer: usize,
    pub path: PathBuf,
    pub delta: String,
    pub total_size: u64,
}

/// Watches multiple .ctx files for size changes, emits deltas.
/// Also scans session directories for new .ctx files.
pub struct CtxWatcher {
    /// (layer_index, path) pairs to watch
    targets: Vec<(usize, PathBuf)>,
    /// Session directories to scan for new .ctx files: (layer_index, dir_path)
    scan_dirs: Vec<(usize, PathBuf)>,
    /// Last known size per path
    sizes: HashMap<PathBuf, u64>,
    /// Minimum poll interval
    poll_interval: Duration,
}

impl CtxWatcher {
    pub fn new(poll_interval: Duration) -> Self {
        Self {
            targets: Vec::new(),
            scan_dirs: Vec::new(),
            sizes: HashMap::new(),
            poll_interval,
        }
    }

    pub fn watch(&mut self, layer: usize, path: PathBuf) {
        // Initialize with current size (don't fire on startup for existing content)
        let current_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.sizes.insert(path.clone(), current_size);
        self.targets.push((layer, path));
    }

    /// Register a sessions directory to scan for new .ctx files.
    pub fn watch_dir(&mut self, layer: usize, dir: PathBuf) {
        self.scan_dirs.push((layer, dir));
    }

    /// Scan for new .ctx files in registered directories and add them to targets.
    fn scan_for_new_files(&mut self) {
        for (layer, dir) in &self.scan_dirs {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "ctx") && !self.sizes.contains_key(&path) {
                    let current_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    info!("New .ctx file detected for L{}: {} ({} bytes)", layer, path.display(), current_size);
                    // Start watching from current size (don't replay history) unless it's tiny
                    // For new files under 8KB, watch from 0 to catch the initial content
                    let start_from = if current_size < 8192 { 0 } else { current_size };
                    self.sizes.insert(path.clone(), start_from);
                    self.targets.push((*layer, path));
                }
            }
        }
    }

    /// Run the poll loop, sending change events to the channel.
    pub async fn run(mut self, tx: mpsc::Sender<CtxChange>) {
        info!("CtxWatcher started, watching {} files", self.targets.len());
        let mut scan_counter: u32 = 0;
        loop {
            tokio::time::sleep(self.poll_interval).await;

            // Scan for new .ctx files every 4 poll cycles (2 seconds at 500ms)
            scan_counter += 1;
            if scan_counter % 4 == 0 {
                self.scan_for_new_files();
            }

            for (layer, path) in &self.targets {
                let current_size = match std::fs::metadata(path) {
                    Ok(m) => m.len(),
                    Err(_) => continue,
                };

                let last_size = self.sizes.get(path).copied().unwrap_or(0);
                if current_size <= last_size {
                    continue;
                }

                // Read only the new bytes
                let delta = match read_delta(path, last_size, current_size) {
                    Ok(d) => d,
                    Err(e) => {
                        debug!("Failed to read delta from {}: {}", path.display(), e);
                        continue;
                    }
                };

                if delta.trim().is_empty() {
                    self.sizes.insert(path.clone(), current_size);
                    continue;
                }

                debug!("L{} .ctx grew {}→{} bytes (+{})", layer, last_size, current_size, current_size - last_size);

                let change = CtxChange {
                    layer: *layer,
                    path: path.clone(),
                    delta,
                    total_size: current_size,
                };

                if tx.send(change).await.is_err() {
                    info!("CtxWatcher channel closed, shutting down");
                    return;
                }

                self.sizes.insert(path.clone(), current_size);
            }
        }
    }
}

/// Read bytes from offset to end of file as UTF-8 string.
fn read_delta(path: &Path, from: u64, to: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; (to - from) as usize];
    file.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}
