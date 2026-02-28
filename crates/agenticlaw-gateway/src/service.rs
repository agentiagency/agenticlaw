//! Systemd service management for agenticlaw bee

use std::path::PathBuf;
use std::process::Command;

const SERVICE_NAME: &str = "bee-agenticlaw";

fn service_file_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".config/systemd/user")
        .join(format!("{}.service", SERVICE_NAME))
}

fn agenticlaw_binary_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let agentibin = format!("{}/agentibin/agenticlaw", home);
    if std::path::Path::new(&agentibin).exists() {
        agentibin
    } else {
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "agenticlaw".to_string())
    }
}

fn service_unit(port: u16) -> String {
    let binary = agenticlaw_binary_path();
    let home = std::env::var("HOME").unwrap_or_default();
    format!(
        r#"[Unit]
Description=Agenticlaw AI Agent Runtime (bee)
After=network.target

[Service]
Type=simple
ExecStart={binary} gateway --port {port} --bind lan
Restart=on-failure
RestartSec=5
Environment=HOME={home}
WorkingDirectory={home}

[Install]
WantedBy=default.target
"#
    )
}

pub fn install(port: u16) -> anyhow::Result<()> {
    let path = service_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let unit = service_unit(port);
    std::fs::write(&path, &unit)?;
    println!("✓ Wrote service file: {}", path.display());

    let status = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()?;
    if !status.success() {
        anyhow::bail!("systemctl daemon-reload failed");
    }
    println!("✓ Reloaded systemd");

    let status = Command::new("systemctl")
        .args(["--user", "enable", SERVICE_NAME])
        .status()?;
    if !status.success() {
        anyhow::bail!("systemctl enable failed");
    }
    println!("✓ Enabled {}", SERVICE_NAME);

    let status = Command::new("systemctl")
        .args(["--user", "start", SERVICE_NAME])
        .status()?;
    if !status.success() {
        eprintln!("⚠ Failed to start service (may already be running)");
    } else {
        println!("✓ Started {}", SERVICE_NAME);
    }

    println!(
        "\nagenticlaw service installed and running on port {}",
        port
    );
    println!("  Check status: agenticlaw status");
    println!("  View logs:    journalctl --user -u {} -f", SERVICE_NAME);
    println!("  Chat:         agenticlaw chat --session myproject");
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    let _ = Command::new("systemctl")
        .args(["--user", "stop", SERVICE_NAME])
        .status();
    println!("✓ Stopped {}", SERVICE_NAME);

    let _ = Command::new("systemctl")
        .args(["--user", "disable", SERVICE_NAME])
        .status();
    println!("✓ Disabled {}", SERVICE_NAME);

    let path = service_file_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
        println!("✓ Removed {}", path.display());
    }

    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    println!("✓ Reloaded systemd");

    println!("\nagenticlaw service uninstalled.");
    Ok(())
}

pub fn restart() -> anyhow::Result<()> {
    let status = Command::new("systemctl")
        .args(["--user", "restart", SERVICE_NAME])
        .status()?;
    if status.success() {
        println!("✓ Restarted {}", SERVICE_NAME);
    } else {
        anyhow::bail!("Failed to restart — is the service installed? Run: agenticlaw install");
    }
    Ok(())
}

pub async fn check_health(port: u16) -> anyhow::Result<serde_json::Value> {
    let url = format!("http://127.0.0.1:{}/health", port);
    let resp = reqwest::get(&url).await?;
    let json: serde_json::Value = resp.json().await?;
    Ok(json)
}
