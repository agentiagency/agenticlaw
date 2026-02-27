//! VersionController — non-destructive workspace migration for consciousness stack
//!
//! Handles v1→v2 migration (L4→dual core), rollback, and forward-compat checks.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionManifest {
    pub schema_version: u32,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub upgraded_from: Option<u32>,
    #[serde(default)]
    pub upgraded_at: Option<String>,
    #[serde(default)]
    pub layout: HashMap<String, String>,
}

pub struct VersionController {
    workspace: PathBuf,
}

impl VersionController {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }

    fn version_path(&self) -> PathBuf {
        self.workspace.join(".version.json")
    }

    fn read_manifest(&self) -> Option<VersionManifest> {
        let path = self.version_path();
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn write_manifest(&self, manifest: &VersionManifest) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(manifest)?;
        let tmp = self.version_path().with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, self.version_path())?;
        Ok(())
    }

    /// Ensure workspace is at the target schema version. Migrate if needed.
    pub fn ensure_version(&self, target: u32) -> anyhow::Result<()> {
        let current = self.read_manifest();

        // Check for incomplete migration
        if let Some(ref manifest) = current {
            if manifest.status.as_deref() == Some("migrating") {
                info!("Detected incomplete migration, resuming...");
                return self.resume_migration(target);
            }

            let current_version = manifest.schema_version;

            // Forward-compat: refuse if workspace is newer than target
            if current_version > target {
                anyhow::bail!(
                    "Workspace is at schema version {} but binary targets version {}. \
                     Refusing to downgrade. Use a newer binary or explicit rollback.",
                    current_version,
                    target
                );
            }

            if current_version == target {
                info!("Workspace already at schema version {}", target);
                return Ok(());
            }

            // Migrate forward
            if current_version == 1 && target == 2 {
                return self.migrate_v1_to_v2();
            }

            anyhow::bail!(
                "No migration path from version {} to {}",
                current_version,
                target
            );
        }

        // No manifest — detect version from layout
        let has_l4 = self.workspace.join("L4").exists();
        let has_core_a = self.workspace.join("core-a").exists();

        if has_core_a {
            // Already v2 layout but no manifest — write one
            info!("Detected v2 layout without manifest, writing manifest");
            self.write_manifest(&self.v2_manifest(Some(1)))?;
            return Ok(());
        }

        if has_l4 && target == 2 {
            return self.migrate_v1_to_v2();
        }

        if target == 2 {
            // Fresh workspace — just create v2 layout
            info!("Fresh workspace, creating v2 layout");
            self.create_v2_layout()?;
            return Ok(());
        }

        Ok(())
    }

    fn migrate_v1_to_v2(&self) -> anyhow::Result<()> {
        info!("Migrating workspace v1 → v2");

        // Step 1: Write status=migrating FIRST
        self.write_manifest(&VersionManifest {
            schema_version: 1,
            status: Some("migrating".to_string()),
            upgraded_from: None,
            upgraded_at: None,
            layout: HashMap::new(),
        })?;

        // Step 2: Rename L4 → core-a (if L4 exists and core-a doesn't)
        let l4_dir = self.workspace.join("L4");
        let core_a_dir = self.workspace.join("core-a");

        if l4_dir.exists() && !core_a_dir.exists() {
            // Resolve symlinks
            let l4_real = std::fs::canonicalize(&l4_dir)?;
            if l4_real != l4_dir {
                // L4 is a symlink — copy the target path logic
                info!("L4 is a symlink to {}, renaming symlink", l4_real.display());
            }
            std::fs::rename(&l4_dir, &core_a_dir)?;
            info!("Renamed L4/ → core-a/");
        } else if !core_a_dir.exists() {
            std::fs::create_dir_all(&core_a_dir)?;
            info!("Created core-a/ (no L4 to migrate)");
        }

        // Step 3: Create core-b
        let core_b_dir = self.workspace.join("core-b");
        if !core_b_dir.exists() {
            std::fs::create_dir_all(&core_b_dir)?;
            info!("Created core-b/");
        }

        // Step 4: Write core-state.json
        let core_state_path = self.workspace.join("core-state.json");
        if !core_state_path.exists() {
            let state = crate::cores::CoreState::new(200_000);
            let json = serde_json::to_string_pretty(&state)?;
            let tmp = core_state_path.with_extension("json.tmp");
            std::fs::write(&tmp, &json)?;
            std::fs::rename(&tmp, &core_state_path)?;
            info!("Created core-state.json");
        }

        // Step 5: Write final .version.json
        self.write_manifest(&self.v2_manifest(Some(1)))?;
        info!("Migration v1 → v2 complete");

        Ok(())
    }

    fn resume_migration(&self, target: u32) -> anyhow::Result<()> {
        if target == 2 {
            // Re-run v1→v2 — each step is idempotent (checks existence before acting)
            return self.migrate_v1_to_v2();
        }
        anyhow::bail!("Cannot resume migration to version {}", target)
    }

    fn create_v2_layout(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.workspace.join("core-a"))?;
        std::fs::create_dir_all(self.workspace.join("core-b"))?;

        let core_state_path = self.workspace.join("core-state.json");
        if !core_state_path.exists() {
            let state = crate::cores::CoreState::new(200_000);
            let json = serde_json::to_string_pretty(&state)?;
            let tmp = core_state_path.with_extension("json.tmp");
            std::fs::write(&tmp, &json)?;
            std::fs::rename(&tmp, &core_state_path)?;
        }

        self.write_manifest(&self.v2_manifest(None))?;
        Ok(())
    }

    fn v2_manifest(&self, upgraded_from: Option<u32>) -> VersionManifest {
        let mut layout = HashMap::new();
        layout.insert("L0".into(), "gateway".into());
        layout.insert("L1".into(), "attention".into());
        layout.insert("L2".into(), "pattern".into());
        layout.insert("L3".into(), "integration".into());
        layout.insert("core-a".into(), "core".into());
        layout.insert("core-b".into(), "core".into());

        VersionManifest {
            schema_version: 2,
            status: None,
            upgraded_from,
            upgraded_at: Some(chrono::Utc::now().to_rfc3339()),
            layout,
        }
    }

    /// Rollback v2 → v1
    pub fn rollback_v2_to_v1(&self) -> anyhow::Result<()> {
        info!("Rolling back workspace v2 → v1");

        let core_a_dir = self.workspace.join("core-a");
        let l4_dir = self.workspace.join("L4");

        if core_a_dir.exists() && !l4_dir.exists() {
            std::fs::rename(&core_a_dir, &l4_dir)?;
            info!("Renamed core-a/ → L4/");
        }

        // Only remove core-b if it's empty or has no .ctx files
        let core_b_dir = self.workspace.join("core-b");
        if core_b_dir.exists() {
            let has_ctx = std::fs::read_dir(&core_b_dir)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .any(|e| e.path().extension().is_some_and(|ext| ext == "ctx"))
                })
                .unwrap_or(false);

            if !has_ctx {
                let _ = std::fs::remove_dir_all(&core_b_dir);
                info!("Removed empty core-b/");
            } else {
                warn!("core-b/ contains .ctx files, preserving");
            }
        }

        // Remove core-state.json
        let _ = std::fs::remove_file(self.workspace.join("core-state.json"));

        // Write v1 manifest
        self.write_manifest(&VersionManifest {
            schema_version: 1,
            status: None,
            upgraded_from: Some(2),
            upgraded_at: Some(chrono::Utc::now().to_rfc3339()),
            layout: HashMap::new(),
        })?;

        info!("Rollback v2 → v1 complete");
        Ok(())
    }

    /// Get current schema version (0 if no manifest)
    pub fn current_version(&self) -> u32 {
        self.read_manifest().map(|m| m.schema_version).unwrap_or(0)
    }
}
