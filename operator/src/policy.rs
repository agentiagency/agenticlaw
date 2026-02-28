//! Policy model and enforcement engine
//!
//! Mirrors the Claude Code settings.json 3-tier model (allow/deny/ask)
//! adapted for agenticlaw tools with glob-pattern matching.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum Role {
    Read,
    Write,
    Local,
    Poke,
    Probe,
    Agent,
    Operator,
}

impl Role {
    pub fn all() -> &'static [Role] {
        &[
            Role::Read,
            Role::Write,
            Role::Local,
            Role::Poke,
            Role::Probe,
            Role::Agent,
            Role::Operator,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Role::Read => "READ",
            Role::Write => "WRITE",
            Role::Local => "LOCAL",
            Role::Poke => "POKE",
            Role::Probe => "PROBE",
            Role::Agent => "AGENT",
            Role::Operator => "OPERATOR",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Role {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "READ" => Ok(Role::Read),
            "WRITE" => Ok(Role::Write),
            "LOCAL" => Ok(Role::Local),
            "POKE" => Ok(Role::Poke),
            "PROBE" => Ok(Role::Probe),
            "AGENT" => Ok(Role::Agent),
            "OPERATOR" => Ok(Role::Operator),
            _ => Err(anyhow::anyhow!("Unknown role: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyTier {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub role: String,
    pub tools: PolicyTier,
    pub bash_commands: PolicyTier,
    pub filesystem: PolicyTier,
    pub network: PolicyTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Ask,
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Decision::Allow => f.write_str("ALLOW"),
            Decision::Deny => f.write_str("DENY"),
            Decision::Ask => f.write_str("ASK"),
        }
    }
}

impl Policy {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    /// Merge a sub-policy overlay. Deny always wins.
    pub fn merge_sub_policy(&mut self, sub: &Policy) {
        merge_tier(&mut self.tools, &sub.tools);
        merge_tier(&mut self.bash_commands, &sub.bash_commands);
        merge_tier(&mut self.filesystem, &sub.filesystem);
        merge_tier(&mut self.network, &sub.network);
    }

    /// Check if a tool name is allowed.
    pub fn check_tool(&self, tool_name: &str) -> Decision {
        check_tier(&self.tools, tool_name)
    }

    /// Check if a bash command is allowed.
    /// The command string is matched against patterns in bash_commands.
    /// Uses permissive glob matching (single * matches everything including /).
    /// Tries multiple normalizations to match both "rm:-rf /" and "rm -rf /:*" style patterns.
    pub fn check_bash_command(&self, command: &str) -> Decision {
        let trimmed = command.trim();
        // Generate candidate strings at different split points
        let candidates = bash_candidates(trimmed);
        // Check deny first across ALL candidates (deny wins)
        for candidate in &candidates {
            for pattern in &self.bash_commands.deny {
                if glob_match_permissive(pattern, candidate) {
                    return Decision::Deny;
                }
            }
        }
        // Then ask
        for candidate in &candidates {
            for pattern in &self.bash_commands.ask {
                if glob_match_permissive(pattern, candidate) {
                    return Decision::Ask;
                }
            }
        }
        // Then allow
        for candidate in &candidates {
            for pattern in &self.bash_commands.allow {
                if glob_match_permissive(pattern, candidate) {
                    return Decision::Allow;
                }
            }
        }
        Decision::Deny
    }

    /// Check a filesystem operation: "read:/path" or "write:/path"
    pub fn check_filesystem(&self, action: &str, path: &str) -> Decision {
        let key = format!("{}:{}", action, path);
        check_tier(&self.filesystem, &key)
    }

    /// Check a network operation: "connect:http://..." or "listen:8080"
    pub fn check_network(&self, operation: &str) -> Decision {
        check_tier(&self.network, operation)
    }

    /// Full check for a tool invocation. Returns the most restrictive decision.
    pub fn check_tool_call(&self, tool_name: &str, args: &serde_json::Value) -> Decision {
        // 1. Check tool name
        let tool_decision = self.check_tool(tool_name);
        if tool_decision == Decision::Deny {
            return Decision::Deny;
        }

        // 2. Tool-specific checks
        match tool_name {
            "bash" => {
                if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                    let cmd_decision = self.check_bash_command(cmd);
                    if cmd_decision == Decision::Deny {
                        return Decision::Deny;
                    }
                    // Check for obfuscation attempts
                    if detect_obfuscation(cmd) {
                        return Decision::Deny;
                    }
                    return most_restrictive(tool_decision, cmd_decision);
                }
            }
            "read" | "glob" | "grep" => {
                if let Some(path) = extract_path(args) {
                    let fs_decision = self.check_filesystem("read", &path);
                    if fs_decision == Decision::Deny {
                        return Decision::Deny;
                    }
                    // Check for path traversal
                    if detect_path_traversal(&path) {
                        return Decision::Deny;
                    }
                    return most_restrictive(tool_decision, fs_decision);
                }
            }
            "write" | "edit" => {
                if let Some(path) = extract_path(args) {
                    let fs_decision = self.check_filesystem("write", &path);
                    if fs_decision == Decision::Deny {
                        return Decision::Deny;
                    }
                    if detect_path_traversal(&path) {
                        return Decision::Deny;
                    }
                    return most_restrictive(tool_decision, fs_decision);
                }
            }
            _ => {}
        }

        tool_decision
    }
}

fn merge_tier(base: &mut PolicyTier, overlay: &PolicyTier) {
    // Deny always wins — add all overlay denials
    for d in &overlay.deny {
        if !base.deny.contains(d) {
            base.deny.push(d.clone());
        }
    }
    // Ask additions
    for a in &overlay.ask {
        if !base.ask.contains(a) {
            base.ask.push(a.clone());
        }
    }
    // Allow additions (but deny still overrides)
    for a in &overlay.allow {
        if !base.allow.contains(a) {
            base.allow.push(a.clone());
        }
    }
}

/// Like check_tier but uses permissive glob (single * = match everything).
/// Used for bash command matching where / has no special meaning.
fn check_tier_permissive(tier: &PolicyTier, value: &str) -> Decision {
    for pattern in &tier.deny {
        if glob_match_permissive(pattern, value) {
            return Decision::Deny;
        }
    }
    for pattern in &tier.ask {
        if glob_match_permissive(pattern, value) {
            return Decision::Ask;
        }
    }
    for pattern in &tier.allow {
        if glob_match_permissive(pattern, value) {
            return Decision::Allow;
        }
    }
    Decision::Deny
}

/// Permissive glob: both * and ** match everything (no slash restriction).
fn glob_match_permissive(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let mut regex_str = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                // Skip consecutive stars
                while i < chars.len() && chars[i] == '*' { i += 1; }
                regex_str.push_str(".*");
                continue;
            }
            '?' => regex_str.push('.'),
            '.' | '^' | '$' | '+' | '{' | '}' | '[' | ']' | '|' | '(' | ')' | '\\' => {
                regex_str.push('\\');
                regex_str.push(chars[i]);
            }
            c => regex_str.push(c),
        }
        i += 1;
    }
    regex_str.push('$');
    Regex::new(&regex_str)
        .map(|re| re.is_match(value))
        .unwrap_or(false)
}

fn check_tier(tier: &PolicyTier, value: &str) -> Decision {
    // Deny takes priority over everything
    for pattern in &tier.deny {
        if glob_match(pattern, value) {
            return Decision::Deny;
        }
    }
    // Then check ask
    for pattern in &tier.ask {
        if glob_match(pattern, value) {
            return Decision::Ask;
        }
    }
    // Then check allow
    for pattern in &tier.allow {
        if glob_match(pattern, value) {
            return Decision::Allow;
        }
    }
    // Default: deny (whitelist model)
    Decision::Deny
}

/// Glob pattern matching compatible with provenance policy.py
fn glob_match(pattern: &str, value: &str) -> bool {
    // Handle the simple wildcard-only pattern
    if pattern == "*" {
        return true;
    }

    // Convert glob to regex
    let mut regex_str = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    regex_str.push_str(".*"); // ** matches everything
                    i += 2;
                    continue;
                } else {
                    regex_str.push_str("[^/]*"); // * matches non-slash
                }
            }
            '?' => regex_str.push_str("[^/]"),
            '.' | '^' | '$' | '+' | '{' | '}' | '[' | ']' | '|' | '(' | ')' | '\\' => {
                regex_str.push('\\');
                regex_str.push(chars[i]);
            }
            c => regex_str.push(c),
        }
        i += 1;
    }
    regex_str.push('$');

    Regex::new(&regex_str)
        .map(|re| re.is_match(value))
        .unwrap_or(false)
}

/// Generate candidate match strings for a bash command.
/// For "rm -rf /workspace", generates:
///   "rm:-rf /workspace"      (split after first word)
///   "rm -rf:/workspace"      (split after second word)
///   "rm -rf /workspace"      (raw, no colon)
///   "rm -rf /:workspace"     ... etc
///
/// Also generates candidates with:
///   - Absolute paths resolved to basenames: "/usr/bin/rm -rf /" -> "rm -rf /"
///   - `env` prefix stripped: "env rm -rf /" -> "rm -rf /"
fn bash_candidates(command: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut base_commands = vec![command.to_string()];

    // Strip `env` prefix (with optional env vars like VAR=val)
    let env_stripped = strip_env_prefix(command);
    if env_stripped != command {
        base_commands.push(env_stripped.to_string());
    }

    // Resolve absolute paths to basenames: /usr/bin/rm -> rm
    for cmd in base_commands.clone() {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        if let Some(first) = words.first() {
            if first.contains('/') {
                if let Some(basename) = first.rsplit('/').next() {
                    if !basename.is_empty() {
                        let resolved = std::iter::once(basename)
                            .chain(words[1..].iter().copied())
                            .collect::<Vec<&str>>()
                            .join(" ");
                        if !base_commands.contains(&resolved) {
                            base_commands.push(resolved);
                        }
                    }
                }
            }
        }
    }

    for base in &base_commands {
        candidates.push(base.clone());

        let words: Vec<&str> = base.split_whitespace().collect();
        for i in 1..words.len() {
            let prefix = words[..i].join(" ");
            let suffix = words[i..].join(" ");
            candidates.push(format!("{}:{}", prefix, suffix));
        }
    }

    candidates
}

/// Strip `env` prefix and any inline VAR=VAL assignments.
/// "env FOO=bar rm -rf /" -> "rm -rf /"
/// "env rm -rf /" -> "rm -rf /"
fn strip_env_prefix(command: &str) -> &str {
    let trimmed = command.trim();
    if !trimmed.starts_with("env ") {
        return command;
    }
    let rest = trimmed[4..].trim_start();
    // Skip any VAR=VALUE pairs
    let mut pos = rest;
    loop {
        let word_end = pos.find(char::is_whitespace).unwrap_or(pos.len());
        let word = &pos[..word_end];
        if word.contains('=') && !word.starts_with('-') {
            pos = pos[word_end..].trim_start();
        } else {
            break;
        }
    }
    pos
}

/// Detect bash command obfuscation attempts.
fn detect_obfuscation(command: &str) -> bool {
    let lower = command.to_lowercase();

    // Base64 decode piped to execution
    if lower.contains("base64") && (lower.contains("| bash") || lower.contains("| sh") || lower.contains("| eval")) {
        return true;
    }

    // Hex/octal escape sequences that could hide commands
    if lower.contains("\\x") && lower.contains("printf") {
        return true;
    }

    // $() or backtick command substitution containing denied patterns
    // This is a heuristic — we check for nested execution
    if (lower.contains("$(") || lower.contains('`')) &&
       (lower.contains("rm ") || lower.contains("chmod") || lower.contains("dd ") ||
        lower.contains("curl") || lower.contains("wget") || lower.contains("nc ")) {
        return true;
    }

    // env/export to override PATH or LD_PRELOAD
    if lower.contains("ld_preload") || lower.contains("ld_library_path") {
        return true;
    }

    // /proc/self/exe tricks
    if lower.contains("/proc/self/exe") || lower.contains("/proc/self/fd") {
        return true;
    }

    // Python/perl/ruby one-liners used to bypass bash restrictions
    if (lower.contains("python") || lower.contains("perl") || lower.contains("ruby"))
        && (lower.contains("-c") || lower.contains("-e"))
    {
        return true;
    }

    // Variable assignment before execution: R=rm; $R or CMD=rm; $CMD
    // Pattern: WORD=WORD followed by ; and $WORD
    if Regex::new(r"[A-Za-z_]\w*=\S+\s*;.*\$")
        .map(|re| re.is_match(&lower))
        .unwrap_or(false)
    {
        return true;
    }

    // bash -c / sh -c / dash -c wrappers (shell re-invocation)
    if Regex::new(r"(?:^|\s|;)(bash|sh|dash)\s+-c\s")
        .map(|re| re.is_match(&lower))
        .unwrap_or(false)
    {
        return true;
    }

    // eval keyword
    if Regex::new(r"(?:^|\s|;)eval\s")
        .map(|re| re.is_match(&lower))
        .unwrap_or(false)
    {
        return true;
    }

    // Here-string: bash <<< or sh <<<
    if Regex::new(r"(?:bash|sh|dash)\s+<<<")
        .map(|re| re.is_match(&lower))
        .unwrap_or(false)
    {
        return true;
    }

    // Here-doc: bash <<EOF or sh <<EOF
    if Regex::new(r"(?:bash|sh|dash)\s+<<\s*\w")
        .map(|re| re.is_match(&lower))
        .unwrap_or(false)
    {
        return true;
    }

    // xargs piped to sh/bash
    if lower.contains("xargs") && (lower.contains("sh") || lower.contains("bash")) {
        return true;
    }

    false
}

/// Detect path traversal attempts
fn detect_path_traversal(path: &str) -> bool {
    // Normalize and check for ..
    let normalized = path.replace("\\", "/");
    if normalized.contains("../") || normalized.contains("/..") || normalized == ".." {
        return true;
    }
    // Symlink indicators (these would need runtime resolution too)
    if normalized.contains("/proc/self/") {
        return true;
    }
    false
}

fn extract_path(args: &serde_json::Value) -> Option<String> {
    // Try common field names for file paths
    args.get("file_path")
        .or_else(|| args.get("path"))
        .or_else(|| args.get("pattern"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn most_restrictive(a: Decision, b: Decision) -> Decision {
    match (a, b) {
        (Decision::Deny, _) | (_, Decision::Deny) => Decision::Deny,
        (Decision::Ask, _) | (_, Decision::Ask) => Decision::Ask,
        _ => Decision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read_policy() -> Policy {
        Policy::load("policies/READ.json").unwrap()
    }

    fn operator_policy() -> Policy {
        Policy::load("policies/OPERATOR.json").unwrap()
    }

    // ── Glob matching ──

    #[test]
    fn glob_wildcard_matches_all() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", "read"));
    }

    #[test]
    fn glob_double_star_matches_deep_paths() {
        assert!(glob_match("read:/workspace/**", "read:/workspace/foo/bar/baz.rs"));
        assert!(glob_match("read:/workspace/**", "read:/workspace/a"));
    }

    #[test]
    fn glob_single_star_no_slash() {
        assert!(glob_match("read:/workspace/*", "read:/workspace/foo"));
        assert!(!glob_match("read:/workspace/*", "read:/workspace/foo/bar"));
    }

    #[test]
    fn glob_literal_match() {
        assert!(glob_match("read", "read"));
        assert!(!glob_match("read", "write"));
    }

    // ── Tool checks ──

    #[test]
    fn read_policy_allows_read_tool() {
        let p = read_policy();
        assert_eq!(p.check_tool("read"), Decision::Allow);
        assert_eq!(p.check_tool("glob"), Decision::Allow);
        assert_eq!(p.check_tool("grep"), Decision::Allow);
    }

    #[test]
    fn read_policy_denies_write_tools() {
        let p = read_policy();
        assert_eq!(p.check_tool("bash"), Decision::Deny);
        assert_eq!(p.check_tool("write"), Decision::Deny);
        assert_eq!(p.check_tool("edit"), Decision::Deny);
    }

    #[test]
    fn operator_allows_all_tools() {
        let p = operator_policy();
        for tool in &["read", "glob", "grep", "write", "edit", "bash"] {
            assert_eq!(p.check_tool(tool), Decision::Allow, "tool {} should be allowed", tool);
        }
    }

    // ── Bash command checks ──

    #[test]
    fn read_policy_allows_ls() {
        let p = read_policy();
        assert_eq!(p.check_bash_command("ls -la /workspace"), Decision::Allow);
    }

    #[test]
    fn read_policy_denies_rm() {
        let p = read_policy();
        assert_eq!(p.check_bash_command("rm -rf /"), Decision::Deny);
    }

    #[test]
    fn operator_policy_asks_rm_rf_root() {
        let p = operator_policy();
        assert_eq!(p.check_bash_command("rm -rf /"), Decision::Ask);
    }

    // ── Filesystem checks ──

    #[test]
    fn read_policy_allows_workspace_read() {
        let p = read_policy();
        assert_eq!(p.check_filesystem("read", "/workspace/foo.rs"), Decision::Allow);
    }

    #[test]
    fn read_policy_denies_etc_shadow() {
        let p = read_policy();
        assert_eq!(p.check_filesystem("read", "/etc/shadow"), Decision::Deny);
    }

    #[test]
    fn read_policy_denies_all_writes() {
        let p = read_policy();
        assert_eq!(p.check_filesystem("write", "/workspace/foo"), Decision::Deny);
    }

    #[test]
    fn read_policy_denies_policy_file_read() {
        let p = read_policy();
        assert_eq!(p.check_filesystem("read", "/etc/agenticlaw/policy.json"), Decision::Deny);
    }

    // ── Full tool call checks ──

    #[test]
    fn read_policy_full_check_read_tool() {
        let p = read_policy();
        let args = json!({"file_path": "/workspace/src/main.rs"});
        assert_eq!(p.check_tool_call("read", &args), Decision::Allow);
    }

    #[test]
    fn read_policy_full_check_write_denied() {
        let p = read_policy();
        let args = json!({"file_path": "/workspace/foo.txt", "content": "hello"});
        assert_eq!(p.check_tool_call("write", &args), Decision::Deny);
    }

    #[test]
    fn read_policy_full_check_path_traversal() {
        let p = read_policy();
        let args = json!({"file_path": "/workspace/../etc/shadow"});
        assert_eq!(p.check_tool_call("read", &args), Decision::Deny);
    }

    // ── Obfuscation detection ──

    #[test]
    fn detects_base64_pipe() {
        assert!(detect_obfuscation("echo cm0gLXJmIC8= | base64 -d | bash"));
    }

    #[test]
    fn detects_ld_preload() {
        assert!(detect_obfuscation("LD_PRELOAD=/tmp/evil.so ls"));
    }

    #[test]
    fn detects_proc_self_exe() {
        assert!(detect_obfuscation("/proc/self/exe --help"));
    }

    #[test]
    fn detects_command_substitution_with_rm() {
        assert!(detect_obfuscation("echo $(rm -rf /)"));
    }

    #[test]
    fn does_not_flag_normal_commands() {
        assert!(!detect_obfuscation("ls -la /workspace"));
        assert!(!detect_obfuscation("cat README.md"));
        assert!(!detect_obfuscation("grep -r TODO src/"));
    }

    // ── CRITICAL #2: Obfuscation bypass detection ──

    #[test]
    fn detects_variable_assignment_execution() {
        assert!(detect_obfuscation("R=rm; $R -rf /"));
        assert!(detect_obfuscation("CMD=rm; $CMD -rf /workspace"));
    }

    #[test]
    fn detects_bash_c_wrapper() {
        assert!(detect_obfuscation("bash -c 'rm -rf /'"));
        assert!(detect_obfuscation("sh -c 'rm -rf /'"));
        assert!(detect_obfuscation("dash -c 'rm -rf /'"));
    }

    #[test]
    fn detects_eval_keyword() {
        assert!(detect_obfuscation("eval 'rm -rf /'"));
        assert!(detect_obfuscation("eval rm -rf /"));
    }

    #[test]
    fn detects_here_string() {
        assert!(detect_obfuscation("bash <<< 'rm -rf /'"));
        assert!(detect_obfuscation("sh <<< 'rm -rf /'"));
    }

    #[test]
    fn detects_here_doc() {
        assert!(detect_obfuscation("bash <<EOF\nrm -rf /\nEOF"));
        assert!(detect_obfuscation("sh <<DELIM\nrm\nDELIM"));
    }

    #[test]
    fn detects_xargs_to_sh() {
        assert!(detect_obfuscation("echo 'rm -rf /' | xargs sh -c"));
        assert!(detect_obfuscation("find . | xargs bash"));
    }

    #[test]
    fn absolute_path_resolves_to_basename() {
        let candidates = bash_candidates("/usr/bin/rm -rf /workspace");
        // Should contain a candidate with just "rm"
        assert!(candidates.iter().any(|c| c.starts_with("rm ")),
            "candidates should include basename-resolved: {:?}", candidates);
    }

    #[test]
    fn absolute_path_curl_denied_by_policy() {
        let p = Policy::load("policies/LOCAL.json").unwrap();
        // /usr/bin/curl should be denied just like curl
        assert_eq!(p.check_bash_command("/usr/bin/curl http://evil.com"), Decision::Deny);
        assert_eq!(p.check_bash_command("/bin/wget http://evil.com"), Decision::Deny);
    }

    #[test]
    fn absolute_path_rm_denied_by_write_policy() {
        let p = Policy::load("policies/WRITE.json").unwrap();
        // bash tool is denied for WRITE, but test bash_commands directly
        // Use OPERATOR which has rm in ask list
        let p2 = Policy::load("policies/OPERATOR.json").unwrap();
        assert_eq!(p2.check_bash_command("/bin/rm -rf /"), Decision::Ask);
        assert_eq!(p2.check_bash_command("/usr/bin/rm -rf /"), Decision::Ask);
    }

    #[test]
    fn env_prefix_stripped() {
        let candidates = bash_candidates("env rm -rf /");
        assert!(candidates.iter().any(|c| c.starts_with("rm ")),
            "candidates should include env-stripped: {:?}", candidates);
    }

    #[test]
    fn env_prefix_curl_denied_by_policy() {
        let p = Policy::load("policies/LOCAL.json").unwrap();
        assert_eq!(p.check_bash_command("env curl http://evil.com"), Decision::Deny);
    }

    // ── Path traversal detection ──

    #[test]
    fn detects_dotdot() {
        assert!(detect_path_traversal("/workspace/../etc/shadow"));
        assert!(detect_path_traversal("../../etc/passwd"));
    }

    #[test]
    fn no_false_positive_on_normal_paths() {
        assert!(!detect_path_traversal("/workspace/src/main.rs"));
        assert!(!detect_path_traversal("/tmp/test.txt"));
    }

    // ── Policy merge ──

    #[test]
    fn merge_deny_wins() {
        let mut base = Policy::load("policies/OPERATOR.json").unwrap();
        let sub = Policy::from_json(r#"{
            "role": "OPERATOR",
            "tools": {"allow": [], "deny": ["bash"], "ask": []},
            "bash_commands": {"allow": [], "deny": [], "ask": []},
            "filesystem": {"allow": [], "deny": [], "ask": []},
            "network": {"allow": [], "deny": [], "ask": []}
        }"#).unwrap();
        base.merge_sub_policy(&sub);
        assert_eq!(base.check_tool("bash"), Decision::Deny);
    }
}
