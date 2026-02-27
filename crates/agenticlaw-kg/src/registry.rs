//! Node Type Registry — maps graph positions to reusable FEAR/PURPOSE templates.
//!
//! The taxonomy (AGENT_TAXONOMY.md) defines the type system for agents.
//! This registry makes those types executable by the KG executor.
//!
//! Every node type has:
//! - PURPOSE template (with {issue}, {repo}, {context} placeholders)
//! - FEAR template (hard constraints)
//! - Whether it's a leaf or parent
//! - For parents: ordered list of child node types
//! - Operator role (what permissions the agent needs)

use std::collections::HashMap;
use std::fmt;

/// Operator container roles (from operator/policies/).
/// Determines what tools/bash/network the agent gets.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OperatorRole {
    Read,
    Write,
    Local,
    Poke,
    Probe,
    Agent,
    Operator,
}

impl fmt::Display for OperatorRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => write!(f, "READ"),
            Self::Write => write!(f, "WRITE"),
            Self::Local => write!(f, "LOCAL"),
            Self::Poke => write!(f, "POKE"),
            Self::Probe => write!(f, "PROBE"),
            Self::Agent => write!(f, "AGENT"),
            Self::Operator => write!(f, "OPERATOR"),
        }
    }
}

/// A registered node type — reusable across runs.
#[derive(Clone, Debug)]
pub struct NodeType {
    /// Unique identifier (e.g. "issue-reader", "codebase-tracer", "analysis")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Taxonomy reference (e.g. "1.10.2" from AGENT_TAXONOMY.md)
    pub taxonomy_ref: Option<String>,
    /// PURPOSE template. Placeholders: {issue}, {repo}, {context}, {parent_output}
    pub purpose_template: String,
    /// FEAR template. Hard constraints.
    pub fear_template: String,
    /// Prompt template. What the agent actually receives.
    pub prompt_template: String,
    /// Minimum operator role needed.
    pub role: OperatorRole,
    /// If true, this node executes directly. If false, it decomposes into children.
    pub is_leaf: bool,
    /// Ordered child node type IDs (only for parent nodes).
    pub children: Vec<String>,
    /// Success criterion — how to evaluate if the leaf succeeded.
    /// For leaves: a description of what artifact/state proves success.
    pub success_criterion: Option<String>,
    /// Max tool calls for leaf nodes. Default: 3.
    /// A leaf that can't do its job in this many calls is too broad — decompose further.
    pub max_tool_calls: u32,
}

/// The registry: holds all node types and resolves them by ID.
pub struct NodeTypeRegistry {
    types: HashMap<String, NodeType>,
}

impl NodeTypeRegistry {
    pub fn new() -> Self {
        Self { types: HashMap::new() }
    }

    pub fn register(&mut self, node_type: NodeType) {
        self.types.insert(node_type.id.clone(), node_type);
    }

    pub fn get(&self, id: &str) -> Option<&NodeType> {
        self.types.get(id)
    }

    pub fn children_of(&self, id: &str) -> Vec<&NodeType> {
        self.get(id)
            .map(|nt| {
                nt.children.iter()
                    .filter_map(|cid| self.get(cid))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn all_ids(&self) -> Vec<&str> {
        self.types.keys().map(|s| s.as_str()).collect()
    }

    /// Render a purpose template with context variables.
    pub fn render_purpose(&self, id: &str, vars: &TemplateVars) -> Option<String> {
        self.get(id).map(|nt| render_template(&nt.purpose_template, vars))
    }

    /// Render a fear template with context variables.
    pub fn render_fear(&self, id: &str, vars: &TemplateVars) -> Option<String> {
        self.get(id).map(|nt| render_template(&nt.fear_template, vars))
    }

    /// Render a prompt template with context variables.
    pub fn render_prompt(&self, id: &str, vars: &TemplateVars) -> Option<String> {
        self.get(id).map(|nt| render_template(&nt.prompt_template, vars))
    }
}

/// Variables for template rendering.
#[derive(Default, Clone, Debug)]
pub struct TemplateVars {
    pub issue: String,
    pub repo: String,
    pub context: String,
    pub parent_output: String,
    pub branch: String,
    pub files: String,
    pub plan: String,
    /// Extra key-value pairs.
    pub extra: HashMap<String, String>,
}

/// Public template rendering (used by executor).
pub fn render_template_pub(template: &str, vars: &TemplateVars) -> String {
    render_template(template, vars)
}

fn render_template(template: &str, vars: &TemplateVars) -> String {
    let mut s = template.to_string();
    s = s.replace("{issue}", &vars.issue);
    s = s.replace("{repo}", &vars.repo);
    s = s.replace("{context}", &vars.context);
    s = s.replace("{parent_output}", &vars.parent_output);
    s = s.replace("{branch}", &vars.branch);
    s = s.replace("{files}", &vars.files);
    s = s.replace("{plan}", &vars.plan);
    for (k, v) in &vars.extra {
        s = s.replace(&format!("{{{}}}", k), v);
    }
    s
}

/// The ROOT FEAR for agentimolt. Every agent in every node inherits this.
/// This is not a suggestion. This is the law of the tree.
pub const AGENTIMOLT_ROOT_FEAR: &str = "\
CONFIGURATION IS THE ONLY STATE. VIOLATING THIS IS IMMEDIATE FAILURE.\n\
\n\
1. ALL config comes from a SINGLE source per environment (AWS Secrets Manager → config/{env}.yaml → Flux ${var} substitution).\n\
2. NEVER hardcode: credentials, passwords, API keys, secret names, region strings, registry URLs, service URLs, port numbers, cluster names, or ANY value that differs between environments.\n\
3. NEVER create environment variables to hold config. Environment variables are SET by the deployment system FROM the single config source. Code reads them — code never defines them.\n\
4. If you need a value, it MUST trace back to config/{env}.yaml or defs.py constants that read from environment. No exceptions.\n\
5. If you see a hardcoded value in existing code, THAT IS A BUG. Do not copy it. Do not reference it. Fix it or flag it.\n\
6. CI test fixtures (like postgres service containers) define their values ONCE at job level, never scattered across steps.\n\
\n\
The pattern: AWS SM secret → ExternalSecret → K8s Secret → Flux postBuild.substitute → ${variable} in manifests → env var in pod → code reads os.environ.\n\
\n\
There is ONE path. There are ZERO alternatives.\n\
\n\
EVERY TOKEN IS PRODUCTION CODE. NO EXCEPTIONS.\n\
\n\
7. Never create placeholder data, lorem ipsum, fake content, or mock values that pretend to be real.\n\
8. If something is a placeholder, it MUST say 'Placeholder' and MUST specify exactly what real resource replaces it and where that resource comes from.\n\
9. Every string, every value, every piece of content is real, professional, production software. If it's not ready, leave it empty or gated — never fake.";

/// Build the default registry for issue-grinding workflows.
/// This maps the agentimolt issue→PR pipeline to taxonomy node types.
pub fn default_issue_registry() -> NodeTypeRegistry {
    let mut reg = NodeTypeRegistry::new();

    // ─── ROOT: issue ───
    // The root FEAR is inherited by EVERY node in the tree.
    reg.register(NodeType {
        id: "issue".into(),
        name: "Issue Root".into(),
        taxonomy_ref: None,
        purpose_template: "Resolve issue #{issue} in {repo}: analyze, implement, and PR.".into(),
        fear_template: format!("{}\n\nEvery child must succeed for this to succeed. If any child fails, stop and report which one and why.", AGENTIMOLT_ROOT_FEAR),
        prompt_template: "".into(), // Parents don't get prompted directly
        role: OperatorRole::Read,
        is_leaf: false,
        children: vec!["analysis".into(), "impl".into(), "pr".into()],
        success_criterion: Some("All children succeed: analysis produced plan, impl produced passing code, PR is created and CI green.".into()),
        max_tool_calls: 3,
    });

    // ─── PARENT: analysis ───
    reg.register(NodeType {
        id: "analysis".into(),
        name: "Analysis Phase".into(),
        taxonomy_ref: Some("1.10".into()), // Code Reading [R]
        purpose_template: "Understand issue #{issue} deeply enough to produce a precise implementation plan.".into(),
        fear_template: "Do NOT implement anything. Do NOT modify any files. Only read, trace, and plan. \
                        Output must be a NUMBERED LIST of exact changes (file, function, what changes). \
                        Max 12 tool calls — be surgical, not exhaustive.".into(),
        prompt_template: "".into(),
        role: OperatorRole::Read,
        is_leaf: false,
        children: vec![
            "read-issue".into(),
            "trace-entrypoints".into(),
            "identify-files".into(),
            "check-conflicts".into(),
            "synthesize-plan".into(),
        ],
        success_criterion: Some("A numbered, falsifiable implementation plan exists.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: read-issue ───
    // Parent (analysis) already fetched the issue text and put it in EGO.
    // This child just parses it. Zero tool calls — pure reasoning.
    reg.register(NodeType {
        id: "read-issue".into(),
        name: "Parse Issue".into(),
        taxonomy_ref: Some("9.1.1".into()), // Markdown Reading
        purpose_template: "Extract structured problem definition from issue #{issue}.".into(),
        fear_template: "Output ONLY the three sections below. Do not call any tools. \
                        The issue text is already provided. Parse it. \
                        If any section is unclear from the issue text, say 'UNCLEAR: <what's missing>'.".into(),
        prompt_template: "From the issue text provided, extract:\n\n\
                         ## PROBLEM\n<what's broken>\n\n\
                         ## EXPECTED\n<what should happen>\n\n\
                         ## REPRODUCTION\n<steps to reproduce, or UNCLEAR if not specified>".into(),
        role: OperatorRole::Read,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("Output contains ## PROBLEM, ## EXPECTED, and ## REPRODUCTION sections.".into()),
        max_tool_calls: 0, // pure reasoning — no tools
    });

    // ─── LEAF: trace-entrypoints ───
    // Universe: parsed issue (PROBLEM/EXPECTED/REPRODUCTION).
    // Purpose: find the execution path. 3 reads max: entrypoint, router, target component.
    reg.register(NodeType {
        id: "trace-entrypoints".into(),
        name: "Trace Entrypoints".into(),
        taxonomy_ref: Some("1.10.3".into()), // Workflow Analysis
        purpose_template: "Trace the execution path from app entrypoint to the code affected by issue #{issue}.".into(),
        fear_template: "You have 3 file reads. Use them: 1) app entrypoint/router, 2) intermediate, 3) target component. \
                        Output the path: `file → file → file`. If 3 reads aren't enough, the issue description \
                        is too vague — report what you found and stop.".into(),
        prompt_template: "Trace the execution path to the affected code.\n\n\
                         1. Read the app entrypoint or router to find where the affected area is mounted\n\
                         2. Follow the import chain to the affected component/function\n\
                         3. Output: `entrypoint.tsx:L# → Router.tsx:L# → Target.tsx:L#`".into(),
        role: OperatorRole::Read,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("Output contains at least one traced path from entrypoint to affected code.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: identify-files ───
    // Universe: parsed issue + traced execution path.
    // Purpose: read the target files and determine which need edits. 3 reads max.
    reg.register(NodeType {
        id: "identify-files".into(),
        name: "Identify Files to Change".into(),
        taxonomy_ref: Some("1.10.2".into()), // Code Reading
        purpose_template: "Identify exactly which files need editing to fix issue #{issue}.".into(),
        fear_template: "Read up to 3 files from the execution trace. For each, state WHAT needs to change \
                        (one sentence, not HOW). Only list files that get EDITED. Context-only files are irrelevant.".into(),
        prompt_template: "Read the target files from the execution trace and determine which need modification.\n\n\
                         Output a numbered list:\n\
                         1. `path/to/file.tsx` — WHAT needs to change (one sentence)\n\n\
                         Only files that will be EDITED.".into(),
        role: OperatorRole::Read,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("Output is a numbered list of file paths with change descriptions.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: check-conflicts ───
    reg.register(NodeType {
        id: "check-conflicts".into(),
        name: "Check Open PR Conflicts".into(),
        taxonomy_ref: Some("10.1".into()), // Git Operations
        purpose_template: "Check if any open PRs touch the same files we plan to modify.".into(),
        fear_template: "If there are conflicts, report them but do NOT stop. The plan will note conflicts \
                        and the implementation will handle them (rebase, coordinate). \
                        Only check files we plan to modify — not the whole repo.".into(),
        prompt_template: "Files we plan to modify:\n{parent_output}\n\n\
                         Check for conflicts with open PRs:\n\
                         1. `gh pr list --repo {repo} --state open --json number,title,files`\n\
                         2. For each open PR, check if it touches any of our target files\n\
                         3. Output:\n\
                            ## CONFLICTS\n\
                            - PR #N (title) touches `file.tsx` — we also modify this\n\n\
                            ## NO CONFLICTS\n\
                            (if none found)".into(),
        role: OperatorRole::Poke,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("Output contains either ## CONFLICTS or ## NO CONFLICTS section.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: synthesize-plan ───
    // Universe: all sibling outputs (parsed issue, trace, files, conflicts).
    // Purpose: pure synthesis. Zero tools. Combine into actionable plan.
    reg.register(NodeType {
        id: "synthesize-plan".into(),
        name: "Synthesize Implementation Plan".into(),
        taxonomy_ref: Some("1.11.1".into()), // Architecture Planning
        purpose_template: "Synthesize a precise, falsifiable implementation plan for issue #{issue}.".into(),
        fear_template: "No tools. No exploration. The universe is complete — synthesize it. \
                        Every plan item must be concrete: file, function, exact change. \
                        A different agent will implement this plan with ONLY this text. No ambiguity.".into(),
        prompt_template: "Synthesize the implementation plan.\n\n\
                         Output format:\n\
                         ## IMPLEMENTATION PLAN for #{issue}\n\n\
                         ### Changes\n\
                         1. **`path/file.tsx`** — `FunctionName`: <exact change>\n\n\
                         ### Tests\n\
                         1. <test to add and what it verifies>\n\n\
                         ### Conflicts\n\
                         <from conflict check, or 'None'>\n\n\
                         ### Branch\n\
                         `fix-{issue}-<short-description>`".into(),
        role: OperatorRole::Read,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("Output contains ## IMPLEMENTATION PLAN with numbered changes and tests.".into()),
        max_tool_calls: 0, // pure reasoning
    });

    // ─── PARENT: impl ───
    reg.register(NodeType {
        id: "impl".into(),
        name: "Implementation Phase".into(),
        taxonomy_ref: Some("1.1".into()), // Code Creation
        purpose_template: "Implement the approved plan for issue #{issue}.".into(),
        fear_template: "Only modify files identified in the plan. All PRs target main. Run tests. \
                        Do NOT add unrelated changes. Do NOT touch CI/CD or config without explicit authorization.".into(),
        prompt_template: "".into(),
        role: OperatorRole::Agent,
        is_leaf: false,
        children: vec![
            "create-branch".into(),
            "apply-changes".into(),
            "write-tests".into(),
            "run-tests".into(),
        ],
        success_criterion: Some("All changes applied, tests written, tests pass.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: create-branch ───
    reg.register(NodeType {
        id: "create-branch".into(),
        name: "Create Branch".into(),
        taxonomy_ref: Some("10.1.1".into()), // Git Basics
        purpose_template: "Create a feature branch from main for issue #{issue}.".into(),
        fear_template: "Always branch from latest main. Pull first. Branch name from the plan. \
                        If branch already exists, check it out and verify it's based on latest main.".into(),
        prompt_template: "Create the implementation branch:\n\n\
                         1. `git checkout main && git pull origin main`\n\
                         2. `git checkout -b {branch}`\n\
                         3. Verify: `git log --oneline -3`\n\n\
                         Output the branch name and the HEAD commit.".into(),
        role: OperatorRole::Local,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("Branch exists, based on latest main.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: apply-changes ───
    reg.register(NodeType {
        id: "apply-changes".into(),
        name: "Apply Code Changes".into(),
        taxonomy_ref: Some("1.2".into()), // Code Modification
        purpose_template: "Apply the implementation plan changes to the codebase.".into(),
        fear_template: "Follow the plan EXACTLY. Do not add 'improvements' not in the plan. \
                        Do not modify files not in the plan. Read each file before modifying to confirm current state. \
                        Commit after each logical change with a descriptive message.".into(),
        prompt_template: "Apply these changes from the approved plan:\n\n{plan}\n\n\
                         For each change:\n\
                         1. Read the file\n\
                         2. Make the change\n\
                         3. Verify the change looks correct\n\
                         4. `git add <file> && git commit -m '<descriptive message>'`\n\n\
                         Output what you changed and the commit hashes.".into(),
        role: OperatorRole::Agent,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("All plan changes applied with commits.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: write-tests ───
    reg.register(NodeType {
        id: "write-tests".into(),
        name: "Write Tests".into(),
        taxonomy_ref: Some("1.7".into()), // Test Engineering
        purpose_template: "Write or update tests for the changes made to fix issue #{issue}.".into(),
        fear_template: "Tests must cover the SPECIFIC behavior changed. Not general tests. \
                        Each test has a clear name describing what it verifies. \
                        Use existing test patterns in the codebase. Do not introduce new test frameworks.".into(),
        prompt_template: "Write tests for the implementation:\n\n{plan}\n\n\
                         1. Find existing test files near the changed code\n\
                         2. Add tests that verify the fix works\n\
                         3. Add tests that verify the old bug doesn't regress\n\
                         4. `git add <test files> && git commit -m 'test: add tests for #{issue}'`\n\n\
                         Output the test file paths and test names.".into(),
        role: OperatorRole::Agent,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("New test files/cases committed that cover the change.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: run-tests ───
    reg.register(NodeType {
        id: "run-tests".into(),
        name: "Run Tests".into(),
        taxonomy_ref: Some("1.7".into()), // Test Engineering
        purpose_template: "Run the test suite and verify all tests pass.".into(),
        fear_template: "Run the FULL test suite, not just new tests. If tests fail, report EXACTLY which \
                        tests and the error output. Do NOT fix failing tests — report them for iteration.".into(),
        prompt_template: "Run the test suite:\n\n\
                         1. `npm test` (or whatever the project uses)\n\
                         2. If tests pass: output 'TESTS PASS: <count> tests'\n\
                         3. If tests fail: output 'TESTS FAIL:' followed by each failing test name and error\n\n\
                         Do NOT fix failures. Just report them.".into(),
        role: OperatorRole::Local,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("Output contains 'TESTS PASS' with count.".into()),
        max_tool_calls: 3,
    });

    // ─── PARENT: pr ───
    reg.register(NodeType {
        id: "pr".into(),
        name: "PR Creation Phase".into(),
        taxonomy_ref: None,
        purpose_template: "Create a clean PR for issue #{issue} with full template compliance.".into(),
        fear_template: "Target main ONLY. Fill EVERY section of .github/PULL_REQUEST_TEMPLATE.md. \
                        If this touches CI/CD or config, add **** POTENTIALLY CATASTROPHIC UPDATE **** header.".into(),
        prompt_template: "".into(),
        role: OperatorRole::Agent,
        is_leaf: false,
        children: vec![
            "fill-template".into(),
            "push-and-create".into(),
            "verify-ci".into(),
        ],
        success_criterion: Some("PR created targeting main, template filled, CI green.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: fill-template ───
    // Universe: implementation output + test results. Needs 1 tool call to read the template.
    reg.register(NodeType {
        id: "fill-template".into(),
        name: "Fill PR Template".into(),
        taxonomy_ref: Some("1.13".into()), // Code Documentation
        purpose_template: "Fill out .github/PULL_REQUEST_TEMPLATE.md for issue #{issue}.".into(),
        fear_template: "Read the template (1 tool call), then fill every section from the provided context. \
                        No 'N/A' unless truly not applicable. Include 'Closes #{issue}'. \
                        If changes touch CI/CD, add **** POTENTIALLY CATASTROPHIC UPDATE **** header.".into(),
        prompt_template: "1. Read `.github/PULL_REQUEST_TEMPLATE.md`\n\
                         2. Fill every section using the implementation context provided\n\
                         3. Output the complete filled PR body text".into(),
        role: OperatorRole::Read,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("PR body text exists with all template sections filled.".into()),
        max_tool_calls: 1, // read template, then pure reasoning
    });

    // ─── LEAF: push-and-create ───
    reg.register(NodeType {
        id: "push-and-create".into(),
        name: "Push and Create PR".into(),
        taxonomy_ref: Some("10.1.1".into()), // Git Basics
        purpose_template: "Push the branch and create the PR on GitHub.".into(),
        fear_template: "Target MUST be main. Double-check before pushing. Use the prepared PR body. \
                        If `gh pr create` fails, report the error — do NOT retry with different target.".into(),
        prompt_template: "Push and create the PR:\n\n\
                         1. `git push origin {branch}`\n\
                         2. `gh pr create --base main --title '<title>' --body-file <template-file> --repo {repo}`\n\
                         3. Output the PR URL.".into(),
        role: OperatorRole::Agent,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("PR URL output from gh pr create.".into()),
        max_tool_calls: 3,
    });

    // ─── LEAF: verify-ci ───
    reg.register(NodeType {
        id: "verify-ci".into(),
        name: "Verify CI Status".into(),
        taxonomy_ref: Some("2.5.1".into()), // System Monitoring
        purpose_template: "Watch CI and report pass/fail status.".into(),
        fear_template: "Wait up to 5 minutes for CI. Check every 30 seconds. \
                        If CI fails, report the failing job and log output. Do NOT attempt to fix — report only.".into(),
        prompt_template: "Monitor CI for the newly created PR:\n\n\
                         1. `gh pr checks {branch} --repo {repo} --watch`\n\
                         2. If all checks pass: output 'CI PASS'\n\
                         3. If any check fails: output 'CI FAIL: <check-name>: <error summary>'".into(),
        role: OperatorRole::Poke,
        is_leaf: true,
        children: vec![],
        success_criterion: Some("Output contains 'CI PASS' or 'CI FAIL' with details.".into()),
        max_tool_calls: 3,
    });

    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_all_nodes() {
        let reg = default_issue_registry();
        // Root
        assert!(reg.get("issue").is_some());
        // Parents
        assert!(reg.get("analysis").is_some());
        assert!(reg.get("impl").is_some());
        assert!(reg.get("pr").is_some());
        // Analysis leaves
        assert!(reg.get("read-issue").is_some());
        assert!(reg.get("trace-entrypoints").is_some());
        assert!(reg.get("identify-files").is_some());
        assert!(reg.get("check-conflicts").is_some());
        assert!(reg.get("synthesize-plan").is_some());
        // Impl leaves
        assert!(reg.get("create-branch").is_some());
        assert!(reg.get("apply-changes").is_some());
        assert!(reg.get("write-tests").is_some());
        assert!(reg.get("run-tests").is_some());
        // PR leaves
        assert!(reg.get("fill-template").is_some());
        assert!(reg.get("push-and-create").is_some());
        assert!(reg.get("verify-ci").is_some());
    }

    #[test]
    fn children_resolve() {
        let reg = default_issue_registry();
        let analysis_children = reg.children_of("analysis");
        assert_eq!(analysis_children.len(), 5);
        assert_eq!(analysis_children[0].id, "read-issue");
        assert_eq!(analysis_children[4].id, "synthesize-plan");
    }

    #[test]
    fn template_rendering() {
        let reg = default_issue_registry();
        let vars = TemplateVars {
            issue: "183".into(),
            repo: "agentiagency/agentimolt-v03".into(),
            ..Default::default()
        };
        let purpose = reg.render_purpose("read-issue", &vars).unwrap();
        assert!(purpose.contains("183"));
        assert!(!purpose.contains("{issue}"));
    }

    #[test]
    fn leaves_are_leaves() {
        let reg = default_issue_registry();
        for id in &["read-issue", "trace-entrypoints", "identify-files", "create-branch", "run-tests"] {
            let nt = reg.get(id).unwrap();
            assert!(nt.is_leaf, "{} should be a leaf", id);
            assert!(nt.children.is_empty(), "{} should have no children", id);
        }
    }

    #[test]
    fn parents_have_children() {
        let reg = default_issue_registry();
        for id in &["issue", "analysis", "impl", "pr"] {
            let nt = reg.get(id).unwrap();
            assert!(!nt.is_leaf, "{} should be a parent", id);
            assert!(!nt.children.is_empty(), "{} should have children", id);
        }
    }

    #[test]
    fn all_children_exist_in_registry() {
        let reg = default_issue_registry();
        for id in reg.all_ids() {
            let nt = reg.get(id).unwrap();
            for child_id in &nt.children {
                assert!(reg.get(child_id).is_some(), "child '{}' of '{}' not found in registry", child_id, id);
            }
        }
    }

    #[test]
    fn tool_limits_correct() {
        let reg = default_issue_registry();
        // Pure reasoning leaves: 0 tool calls
        assert_eq!(reg.get("read-issue").unwrap().max_tool_calls, 0);
        assert_eq!(reg.get("synthesize-plan").unwrap().max_tool_calls, 0);
        // Read-only leaves: 1-3 tool calls
        assert_eq!(reg.get("fill-template").unwrap().max_tool_calls, 1);
        assert!(reg.get("trace-entrypoints").unwrap().max_tool_calls <= 3);
        assert!(reg.get("identify-files").unwrap().max_tool_calls <= 3);
        // Action leaves: exactly 3
        assert_eq!(reg.get("apply-changes").unwrap().max_tool_calls, 3);
        assert_eq!(reg.get("run-tests").unwrap().max_tool_calls, 3);
    }

    #[test]
    fn operator_roles_assigned() {
        let reg = default_issue_registry();
        assert_eq!(reg.get("read-issue").unwrap().role, OperatorRole::Read);
        assert_eq!(reg.get("trace-entrypoints").unwrap().role, OperatorRole::Read);
        assert_eq!(reg.get("apply-changes").unwrap().role, OperatorRole::Agent);
        assert_eq!(reg.get("create-branch").unwrap().role, OperatorRole::Local);
    }
}
