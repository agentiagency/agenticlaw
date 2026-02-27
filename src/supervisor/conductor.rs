use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use super::log;
use super::types::ConductorCommand;

pub async fn listen_stdin(tx: mpsc::Sender<ConductorCommand>) {
    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<ConductorCommand>(&line) {
            Ok(cmd) => {
                if tx.send(cmd).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                log::warn("invalid_command", serde_json::json!({"error": e.to_string(), "input": line}));
            }
        }
    }
}
