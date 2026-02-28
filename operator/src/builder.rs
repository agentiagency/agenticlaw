//! Docker image builder — builds agenticlaw agent images per role

use crate::policy::Role;
use anyhow::Result;
use std::path::Path;
use std::process::Command;
use tracing::info;

/// Build all 7 agent container images.
pub fn build_all(project_root: &Path, registry: Option<&str>) -> Result<()> {
    for role in Role::all() {
        build_role(project_root, *role, registry)?;
    }
    Ok(())
}

/// Build a single role's container image.
pub fn build_role(project_root: &Path, role: Role, registry: Option<&str>) -> Result<()> {
    let tag = image_tag(role, registry);
    info!("Building image: {}", tag);

    let operator_dir = project_root.join("operator");
    let dockerfile = operator_dir.join("Dockerfile");

    let status = Command::new("docker")
        .arg("build")
        .arg("-f")
        .arg(&dockerfile)
        .arg("--build-arg")
        .arg(format!("ROLE={}", role.name()))
        .arg("-t")
        .arg(&tag)
        .arg(project_root)
        .status()?;

    if !status.success() {
        anyhow::bail!("Docker build failed for role {}", role);
    }

    info!("Built: {}", tag);
    Ok(())
}

/// Push an image to registry.
pub fn push_image(role: Role, registry: &str) -> Result<()> {
    let tag = image_tag(role, Some(registry));
    info!("Pushing: {}", tag);

    let status = Command::new("docker")
        .arg("push")
        .arg(&tag)
        .status()?;

    if !status.success() {
        anyhow::bail!("Docker push failed for {}", tag);
    }

    Ok(())
}

/// Push all images.
pub fn push_all(registry: &str) -> Result<()> {
    for role in Role::all() {
        push_image(*role, registry)?;
    }
    Ok(())
}

fn image_tag(role: Role, registry: Option<&str>) -> String {
    let name = format!("agenticlaw-{}", role.name().to_lowercase());
    match registry {
        Some(reg) => format!("{}/{}", reg, name),
        None => name,
    }
}

/// Run a container for testing, returns the container ID.
pub fn run_container(role: Role, registry: Option<&str>, host_port: u16) -> Result<String> {
    let tag = image_tag(role, registry);
    let container_name = format!("agenticlaw-test-{}", role.name().to_lowercase());

    // Stop any existing container with same name
    let _ = Command::new("docker")
        .args(["rm", "-f", &container_name])
        .output();

    let mut cmd = Command::new("docker");
    cmd.arg("run").arg("-d")
        .arg("--name").arg(&container_name)
        .arg("-p").arg(format!("{}:18789", host_port));

    // Apply container security based on role
    apply_container_security(&mut cmd, role);

    // Set environment
    cmd.arg("-e").arg(format!("ANTHROPIC_API_KEY={}", std::env::var("ANTHROPIC_API_KEY").unwrap_or_default()));

    cmd.arg(&tag);

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to start container {}: {}", container_name, stderr);
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    info!("Started {} on port {} (id: {})", container_name, host_port, &container_id[..12]);
    Ok(container_id)
}

/// Stop and remove a test container.
pub fn stop_container(container_id: &str) -> Result<()> {
    Command::new("docker")
        .args(["rm", "-f", container_id])
        .output()?;
    Ok(())
}

fn apply_container_security(cmd: &mut Command, role: Role) {
    // All containers: no-new-privileges
    cmd.arg("--security-opt").arg("no-new-privileges");

    match role {
        Role::Read => {
            cmd.arg("--read-only")
                .arg("--cap-drop=ALL")
                .arg("--network=none")
                .arg("--tmpfs").arg("/tmp:rw,noexec,nosuid,size=64m");
        }
        Role::Write => {
            cmd.arg("--cap-drop=ALL")
                .arg("--network=none")
                .arg("--tmpfs").arg("/tmp:rw,noexec,nosuid,size=64m");
        }
        Role::Local => {
            cmd.arg("--cap-drop=ALL")
                .arg("--cap-add=DAC_OVERRIDE")
                .arg("--network=none")
                .arg("--tmpfs").arg("/tmp:rw,nosuid,size=256m");
        }
        Role::Poke => {
            cmd.arg("--cap-drop=ALL")
                .arg("--cap-add=DAC_OVERRIDE")
                .arg("--tmpfs").arg("/tmp:rw,nosuid,size=256m");
        }
        Role::Probe => {
            cmd.arg("--cap-drop=ALL")
                .arg("--cap-add=DAC_OVERRIDE")
                .arg("--cap-add=NET_RAW")
                .arg("--tmpfs").arg("/tmp:rw,nosuid,size=256m");
        }
        Role::Agent => {
            cmd.arg("--cap-drop=ALL")
                .arg("--cap-add=DAC_OVERRIDE")
                .arg("--cap-add=NET_BIND_SERVICE")
                .arg("--cap-add=NET_RAW")
                .arg("--tmpfs").arg("/tmp:rw,nosuid,size=512m");
        }
        Role::Operator => {
            cmd.arg("--cap-drop=ALL")
                .arg("--cap-add=DAC_OVERRIDE")
                .arg("--cap-add=NET_BIND_SERVICE")
                .arg("--cap-add=NET_RAW")
                .arg("--cap-add=SYS_PTRACE")
                .arg("--tmpfs").arg("/tmp:rw,nosuid,size=1g");
        }
    }
}
