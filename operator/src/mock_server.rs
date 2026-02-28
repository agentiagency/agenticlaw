//! Mock policy server — serves sub-policies over HTTP for testing
//!
//! GET /policies/{ROLE} → returns sub-policies for that role
//! GET /policies/{ROLE}/{name} → returns a specific sub-policy

mod mock_provider;
mod policy;

use axum::{
    extract::Path,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde_json::json;
use tracing::info;

/// Universal sub-policies applied to ALL roles
fn universal_sub_policy() -> serde_json::Value {
    json!({
        "role": "UNIVERSAL",
        "tools": {"allow": [], "deny": [], "ask": []},
        "bash_commands": {
            "allow": [],
            "deny": [],
            "ask": [
                "rm -rf /:*", "rm -rf /*:*",
                "dd if=/dev/zero of=/dev/sd*:*",
                ":(){ :|:& };:*",
                "chmod -R 777 /:*",
                "kill -9 1"
            ]
        },
        "filesystem": {
            "allow": [],
            "deny": [
                "write:/etc/agenticlaw/policy.json",
                "read:/etc/agenticlaw/policy.json",
                "write:/var/log/agenticlaw/audit.jsonl"
            ],
            "ask": []
        },
        "network": {"allow": [], "deny": [], "ask": []}
    })
}

/// Role-specific sub-policies for testing
fn role_sub_policy(role: &str) -> serde_json::Value {
    match role.to_uppercase().as_str() {
        "READ" => json!({
            "role": "READ",
            "tools": {"allow": [], "deny": [], "ask": []},
            "bash_commands": {"allow": [], "deny": ["*"], "ask": []},
            "filesystem": {"allow": [], "deny": ["write:**", "execute:**"], "ask": []},
            "network": {"allow": [], "deny": ["connect:**"], "ask": []}
        }),
        "WRITE" => json!({
            "role": "WRITE",
            "tools": {"allow": [], "deny": [], "ask": []},
            "bash_commands": {"allow": [], "deny": [], "ask": []},
            "filesystem": {"allow": [], "deny": [], "ask": []},
            "network": {"allow": [], "deny": ["connect:**"], "ask": []}
        }),
        _ => json!({
            "role": role.to_uppercase(),
            "tools": {"allow": [], "deny": [], "ask": []},
            "bash_commands": {"allow": [], "deny": [], "ask": []},
            "filesystem": {"allow": [], "deny": [], "ask": []},
            "network": {"allow": [], "deny": [], "ask": []}
        }),
    }
}

async fn get_policies(Path(role): Path<String>) -> impl IntoResponse {
    let universal = universal_sub_policy();
    let specific = role_sub_policy(&role);
    Json(json!({
        "universal": universal,
        "role_specific": specific,
    }))
}

async fn get_sub_policy(Path((role, name)): Path<(String, String)>) -> impl IntoResponse {
    match name.as_str() {
        "universal" => Json(universal_sub_policy()),
        _ => Json(role_sub_policy(&role)),
    }
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok", "service": "mock-policy-server"}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("mock_policy_server=info")
        .init();

    let app = Router::new()
        .route("/health", get(health))
        .route("/policies/{role}", get(get_policies))
        .route("/policies/{role}/{name}", get(get_sub_policy));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    info!("Mock policy server on :8080");
    axum::serve(listener, app).await?;

    Ok(())
}
