use chrono::Utc;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct LogEvent {
    pub ts: String,
    pub level: &'static str,
    pub event: &'static str,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

pub fn emit(level: &'static str, event: &'static str, data: serde_json::Value) {
    let entry = LogEvent {
        ts: Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        level,
        event,
        data,
    };
    if let Ok(json) = serde_json::to_string(&entry) {
        eprintln!("{json}");
    }
}

pub fn info(event: &'static str, data: serde_json::Value) {
    emit("info", event, data);
}

pub fn warn(event: &'static str, data: serde_json::Value) {
    emit("warn", event, data);
}

pub fn error(event: &'static str, data: serde_json::Value) {
    emit("error", event, data);
}
