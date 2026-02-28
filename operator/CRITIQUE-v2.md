# SPEC-v2 Adversarial Security Critique

Reviewed: 2026-02-18
Reviewer: Adversarial Security Audit (automated)
Base documents: SPEC.md, SPEC-v2.md, CRITIQUE.md (v1)

---

## 1. CRITICAL — FROM scratch eliminates /dev/urandom; entrypoint.rs token generation is unspecified

**Severity**: CRITICAL

SPEC-v2 section 1.3 says `FROM scratch` with "only CA certs + binaries + policy + entrypoint." The current `entrypoint.sh` generates a per-boot token via `head -c 32 /dev/urandom`. A scratch image has NO `/dev` filesystem whatsoever — no `/dev/urandom`, no `/dev/null`, no `/dev/zero`.

SPEC-v2 acknowledges entrypoint must become a static binary but does not specify how token generation works. Options:
- Rust's `getrandom` crate uses the `getrandom(2)` syscall directly (does NOT need `/dev/urandom` filesystem node) — this works on scratch.
- But: `uuid::Uuid::new_v4()` (already a dependency) uses `getrandom` internally — so token generation is solvable IF the implementer knows to use the syscall path.

The spec says nothing about this. An implementer who tries `std::fs::read("/dev/urandom")` will get a runtime panic on scratch with no shell to debug it.

**Remediation**: SPEC-v2 must explicitly state: "Token generation uses the `getrandom(2)` syscall via Rust's `getrandom` crate or `rand`. Do NOT read from `/dev/urandom` as a file. Verify with `strace` that no `/dev` access occurs." Add an integration test that runs the entrypoint binary on a scratch container and confirms token generation succeeds.

---

## 2. CRITICAL — FROM scratch has no shell, no debuggability, no curl for sub-policy fetch

**Severity**: CRITICAL

The current entrypoint uses `curl` to fetch sub-policies. SPEC-v2 section 1.3 puts the image on scratch. Scratch has no `curl`, no `sh`, no `bash`, no debugging tools whatsoever.

This means:
- Sub-policy fetch must be reimplemented in Rust inside entrypoint.rs (using reqwest or a lighter HTTP client statically linked with rustls)
- If a container fails to start, there is zero ability to exec into it for debugging — `docker exec` requires a shell in the image
- Health check probes that use `curl` in Dockerfiles or docker-compose won't work
- The OPERATOR and AGENT roles (SPEC-v2 section 6.2) are supposed to have "full thomcom_shell install" — this is impossible on scratch

SPEC-v2 is internally contradictory: section 1.3 says scratch for all images, but section 6.2 says thomcom_shell for OPERATOR/AGENT.

**Remediation**: Either (a) use scratch only for READ/WRITE/LOCAL (the restrictive roles) and a minimal distro for AGENT/OPERATOR/PROBE/POKE, or (b) statically link busybox into scratch images that need debugging and thomcom_shell. The spec must resolve the contradiction. Sub-policy fetch must be rewritten in Rust regardless.

---

## 3. CRITICAL — Single binary merge increases blast radius

**Severity**: CRITICAL

SPEC-v2 section 1.4 merges `agenticlaw` + `protectgateway` + `operator` into a single binary with argv[0] dispatch. This means:

- **protectgateway now contains all agenticlaw gateway code**. If an attacker achieves code execution in protectgateway (e.g., via a malformed WebSocket frame in tokio-tungstenite), they have the entire agenticlaw gateway codebase in memory including any tool execution primitives.
- **The operator binary is inside the agent image**. The operator contains Docker/registry interaction code, build logic, and test harness code. None of this should exist in production agent containers.
- **CVE surface area**: A vulnerability in any one component's dependencies (e.g., a reqwest 0.12 vulnerability) now affects all three binaries since they are the same binary.
- **The policy enforcer and the policy-enforced process are the same binary**. This is the security equivalent of combining the kernel and userspace into one process.

CRITIQUE v1 item #1 identified the shared-loopback bypass. A single binary makes this worse: even if you separate network namespaces, the protectgateway code path can call agenticlaw functions directly via an internal function call, no network needed.

**Remediation**: Do NOT merge protectgateway with agenticlaw. Keep them as separate binaries with separate dependency trees. Merge only the operator subcommands (build/test/push) into one binary. protectgateway should remain a minimal, hardened, separately-compiled binary with the smallest possible dependency set. Consider building protectgateway with `#![forbid(unsafe_code)]` and minimal deps (just hyper + serde_json + policy logic).

---

## 4. HIGH — MockProvider creates false confidence in security tests

**Severity**: HIGH

SPEC-v2 section 2.1 defines a MockProvider that returns canned responses. Section 8.1 uses MockProvider for adversarial tests. The mock returns `tool_use("write", {path: "/etc/shadow"})` and verifies protectgateway blocks it.

This tests only the happy path of the security boundary:
- The mock produces well-formed tool_use JSON. Real Claude can produce malformed JSON, unexpected field types, or novel tool names not in the schema.
- The mock always returns exactly one tool_use. Real Claude can return multiple tool_use blocks in a single response, embed tool_use within text blocks, or use streaming chunks that split a tool name across frames.
- The mock cannot test timing-dependent attacks (slow loris on WebSocket, partial JSON frames, interleaved tool calls).
- The mock cannot test emergent adversarial behavior — real Claude with a pentest prompt will try creative approaches the mock author never anticipated.
- **Most critically**: MockProvider tests verify that protectgateway blocks known attack patterns. They cannot discover unknown attack patterns. The mock gives confidence that the blocklist works, not that the security model is sound.

Section 8.2 (live adversarial tests) partially addresses this, but these are behind a `--live` flag and cost real API money, so they will likely be run rarely.

**Remediation**: Mock tests are necessary but not sufficient. The spec must mandate: (a) live adversarial tests run in CI at least weekly, (b) fuzz testing of protectgateway's JSON parsing with arbitrary byte sequences, (c) property-based testing that generates random tool_use payloads and verifies the policy engine never panics and always returns allow/deny (never undefined). Add a `MockProvider::respond_malformed()` mode that produces invalid JSON, oversized payloads, and null bytes.

---

## 5. HIGH — AP socket (port 12321) protocol has no replay protection

**Severity**: HIGH

SPEC-v2 section 5.2-5.3 describes: Ed25519 signed handshake, verify against provenance token, then X25519 encrypted messages.

Missing from the protocol:
- **No nonce or sequence number**: An attacker who captures encrypted messages can replay them. If a message says "deploy version X," replaying it weeks later triggers an unintended deployment.
- **No timestamp or TTL**: Signed handshakes are valid forever. A captured handshake can be replayed to establish new sessions indefinitely.
- **No channel binding**: The X25519 key exchange is not bound to the Ed25519 identity. A MITM who compromises the ephemeral key exchange (e.g., via implementation bug) gets a valid encrypted channel even without the Ed25519 key.
- **"Key file baked into the image at build time per role"**: This means every instance of a role has the same key. Compromise one READ container, you can impersonate all READ containers. There is no per-instance identity.
- **No forward secrecy rotation**: The spec says "ephemeral X25519" but doesn't specify whether a new key pair is generated per-session or per-message. If per-session only, compromising one session key decrypts the entire session.

**Remediation**: Use the Noise Protocol Framework (specifically Noise_IK or Noise_XX) which provides replay protection, channel binding, forward secrecy, and identity hiding in a well-analyzed package. The `snow` crate implements Noise for Rust. Do not design a custom crypto protocol. Add per-instance keys generated at boot time (not baked into the image). Include monotonic sequence numbers in every message.

---

## 6. HIGH — Tool use limits can be gamed by not calling tools

**Severity**: HIGH

SPEC-v2 section 3.1 says when the agent has used N tools, it gets 2 more API calls to write a summary. Section 3.2 says `tools: None` is set to force text-only response.

Gaming strategies:
- **Information exfiltration without tools**: The agent can encode workspace content into its text responses. If the operator reads the agent's text output (e.g., for a report), the agent can dump file contents directly into the conversation without using `read` tool — if it has already seen the content in earlier turns.
- **Stalling**: The agent returns text-only responses that don't advance the task, wasting API calls without triggering the tool limit.
- **Tool limit as a denial-of-service**: If `--max-tool-uses 1`, the agent gets one tool call plus 2 summary calls. An adversarial prompt could waste the single tool call on a no-op, then use the summary calls to write a misleading report.
- **`Done` signal spoofing**: The spec says the agent can emit `Done` to terminate. A compromised or confused agent can emit `Done` immediately, producing no useful work. No minimum-progress check exists.

But the most critical gap: **setting `tools: None` does not prevent the agent from asking for tools**. The LLM will still generate text that looks like tool calls. If any downstream parser (log analyzer, report consumer) naively parses these text blocks as tool invocations, the limit is bypassed.

**Remediation**: The tool limit must be paired with output validation: after the limit is reached, the agent's text responses must be sanitized to ensure no tool_use-shaped JSON is present. Add a minimum-progress gate: if the agent uses fewer than some threshold of tools before emitting `Done`, flag the run as suspicious. Rate-limit API calls (not just tool calls) to prevent stalling.

---

## 7. HIGH — TOCTOU race condition on file-watch inbox

**Severity**: HIGH

SPEC-v2 section 4.3 describes message subscription via file-watch on `/workspace/.agenticlaw/inbox/`. A file appearing = a message. The tool reads and deletes the file.

Race conditions:
- **Double delivery**: Two subscribe calls watch the same queue. File appears. Both detect it via inotify. Both attempt to read it. First succeeds, second gets partial or empty content (or succeeds too if read completes before delete). No atomic claim mechanism.
- **Partial write delivery**: The file is detected by inotify on CREATE, but the writer hasn't finished writing content yet. The subscriber reads a partial message. Inotify CLOSE_WRITE should be used instead of CREATE, but the spec doesn't specify which event.
- **Delete-before-read**: A separate process (or a concurrent agent tool call) deletes the file between the watcher detecting it and the subscriber reading it.
- **Symlink injection**: An agent with write access to `/workspace` can create a symlink at `/workspace/.agenticlaw/inbox/msg -> /etc/agenticlaw/policy.json`. The subscribe tool reads it, delivering the policy file contents to the agent — bypassing the filesystem deny rule since the subscribe tool, not the read tool, does the actual read.
- **Denial of service**: An agent floods the inbox directory with millions of zero-byte files, exhausting inotify watches or causing the subscriber to spin.

**Remediation**: Use `tokio::sync::mpsc` channels as the primary mechanism; file-watch is a secondary ingestion source only. For file-watch: use CLOSE_WRITE events, not CREATE. Implement atomic claim via `rename()` to a processing directory before read. Resolve symlinks (realpath) before reading and check the resolved path against the filesystem policy. Set a maximum file count and size limit on the inbox directory.

---

## 8. HIGH — reqwest 0.11 vs 0.12 version split creates two TLS stacks

**Severity**: HIGH

SPEC-v2 section 1.1 specifies:
- `agenticlaw-llm/Cargo.toml`: `reqwest = "0.11"` with `rustls-tls`
- `operator/Cargo.toml`: `reqwest = "0.12"` with `rustls-tls`

reqwest 0.11 uses `hyper` 0.14 and `rustls` 0.21. reqwest 0.12 uses `hyper` 1.x and `rustls` 0.22+. These are incompatible ecosystems:

- **Two copies of rustls are linked into the single merged binary** (section 1.4), inflating binary size and contradicting the minification goal.
- **Two different TLS implementations** with potentially different security properties, different certificate validation behavior, and different vulnerability profiles.
- **hyper 0.14 and hyper 1.x coexist**, meaning two HTTP stacks, two connection pools, two sets of timeouts.
- The workspace `Cargo.toml` currently specifies `reqwest = "0.11"`. The operator specifies `reqwest = "0.12"`. If merged into a single binary, cargo will compile both versions.
- `tokio-tungstenite 0.24` uses `hyper` 1.x / `rustls` 0.23, introducing a potential THIRD rustls version.

**Remediation**: Align the entire workspace on a single reqwest version. Either upgrade `agenticlaw-llm` to reqwest 0.12 (preferred — it's the maintained version) or downgrade operator to 0.11. This is a prerequisite for the single-binary merge. Audit all transitive deps with `cargo tree -d` to ensure exactly one copy of rustls, hyper, and ring.

---

## 9. MEDIUM — thomcom_shell in containers expands attack surface unpredictably

**Severity**: MEDIUM

SPEC-v2 section 6.2 installs `thomcom_shell` (an unpackaged, external binary from github.com/thomcom) into OPERATOR and AGENT images, with a reduced version in PROBE and POKE.

Security concerns:
- **thomcom_shell is not audited**: The spec says it's "not yet packaged as an agentisoft bee." There is no version pinning, no checksum verification, no supply chain attestation. The Dockerfile clones from GitHub at build time — a compromised repo or MITM on the clone serves arbitrary code.
- **"Read-only commands only" for PROBE/POKE is undefined**: What constitutes read-only in thomcom_shell? Who enforces it? If thomcom_shell has its own shell interpreter, the entire bash command filtering in protectgateway is bypassed — the agent runs `tsh "rm -rf /"` and protectgateway sees a `tsh` command, not `rm`.
- **Shell-within-a-shell**: thomcom_shell is itself a shell. Any command execution inside it bypasses protectgateway's bash command parsing, which only inspects the outermost command string.
- **thomcom_shell on scratch is impossible**: Section 1.3 says scratch base. thomcom_shell likely has dynamic dependencies (libc at minimum unless statically compiled). The spec doesn't address this.

**Remediation**: (a) Pin thomcom_shell to a specific commit hash with SHA256 verification in the Dockerfile. (b) thomcom_shell commands must be routed through protectgateway's policy engine, not executed directly. (c) Add `tsh` to the bash deny list for all roles below OPERATOR if thomcom_shell can execute arbitrary commands. (d) Defer thomcom_shell integration until it has an agentisoft manifest with signed builds. (e) Resolve the scratch contradiction.

---

## 10. MEDIUM — Entrypoint.rs must replicate bash entrypoint atomics correctly

**Severity**: MEDIUM

The current `entrypoint.sh` performs:
1. Token generation
2. Sub-policy fetch with curl
3. Start agenticlaw gateway in background
4. Health check loop (30 iterations, 100ms sleep)
5. Trap handler to kill agenticlaw on exit
6. Exec protectgateway

Rewriting this in Rust (SPEC-v2 section 1.3) requires:
- **Process supervision**: The Rust entrypoint must be PID 1, which means it must handle SIGTERM/SIGCHLD correctly. Getting PID 1 signal handling right in Rust requires either a proper init like `tini` (unavailable on scratch) or implementing zombie reaping and signal forwarding manually.
- **The exec of protectgateway disappears in a single binary**: If everything is one binary (section 1.4), the entrypoint doesn't exec a separate protectgateway — it starts the gateway code in a thread/task. This means a panic in the gateway code brings down the policy enforcer too. No process isolation.
- **Health check without curl**: The Rust entrypoint must implement HTTP health checking internally. One more HTTP client in the binary.
- **If entrypoint.rs panics, there is no shell to debug**: On scratch, a panic produces a core dump that cannot be inspected without extracting it from the container's overlay filesystem.

**Remediation**: Use `tini` as PID 1 (statically compiled, ~30KB, can be included in scratch images) with the Rust entrypoint as the child. Or implement signal handling explicitly using `tokio::signal` with SIGTERM, SIGCHLD, and SIGINT handlers. The entrypoint must never panic — use `catch_unwind` around all initialization. For the single-binary case, run protectgateway and agenticlaw in separate tokio tasks with independent panic boundaries using `std::panic::catch_unwind` in spawned threads.

---

## 11. MEDIUM — opt-level "z" may disable security-relevant checks

**Severity**: MEDIUM

SPEC-v2 section 1.2 uses `opt-level = "z"` (optimize for size). This can:
- Remove bounds checks that LLVM considers provably unnecessary but that serve as defense-in-depth
- Inline less aggressively, potentially changing timing characteristics (relevant if any timing-sensitive crypto operations exist)
- In combination with `strip = true` and `lto = true`, produce a binary that is extremely difficult to debug or analyze post-incident

The size savings from `opt-level = "z"` vs `opt-level = "s"` (optimize for size, less aggressive) are typically 5-15%, while `"z"` can have measurable performance penalties.

**Remediation**: Use `opt-level = "s"` instead of `"z"`. The additional few hundred KB are irrelevant given that the minification target is ~9MB. Keep `strip = true` and `lto = true`. Add `overflow-checks = true` and `debug-assertions = false` explicitly in the release profile to ensure integer overflow detection remains active.

---

## 12. MEDIUM — Per-role key baked at build time is a static credential

**Severity**: MEDIUM

SPEC-v2 section 5.2: "The key file is baked into the image at build time per role." This means:
- The Ed25519 private key is in the Docker image layer. Anyone who pulls the image (from ECR or local registry) has the key.
- `docker history` or `docker save | tar` extracts the key from any layer.
- All instances of the same role share the same key — no per-instance identity.
- Key rotation requires rebuilding and redeploying all images.
- If the image is ever pushed to a public registry by accident, the key is permanently compromised.

Combined with the replay attack issue (#5), this is especially dangerous: an attacker who extracts a role key can impersonate that role forever.

**Remediation**: Generate per-instance keys at boot time (in entrypoint.rs). Register the public key with the provenance service on first boot. Store the private key only in memory (never on disk). Bake only the provenance service's public key into the image for verifying incoming messages.

---

## 13. LOW — `respond_from_file(path)` in MockProvider reads arbitrary files

**Severity**: LOW

SPEC-v2 section 2.1 defines `respond_from_file(path)` which replays canned responses from a JSON file. If the path argument is user-controlled (e.g., via test CLI flags), this is a file read primitive. In a test context this is low risk, but if MockProvider is ever accidentally available in production (e.g., through the single-binary merge where mode dispatch is argv-based), an attacker who controls the command line can read arbitrary files.

**Remediation**: MockProvider must be behind a compile-time `#[cfg(test)]` or `#[cfg(feature = "mock")]` gate, never compiled into release binaries. The single-binary merge makes this harder — ensure the mock feature is not enabled in the production build profile.

---

## 14. LOW — No specification of what happens when protectgateway itself is the tool executor

**Severity**: LOW

With the single-binary merge (section 1.4), protectgateway and agenticlaw are the same process. The spec describes protectgateway as a proxy that forwards to agenticlaw on a different port. In a single binary, this "forwarding" becomes an internal function call. The spec does not describe:
- How policy enforcement is maintained when there is no network boundary
- Whether the tool execution context has access to the policy engine's memory
- How the `--upstream-token` mechanism works when upstream is localhost-to-self

**Remediation**: If merging into a single binary, the spec must define an internal API boundary (trait-based, with the policy engine as a middleware layer). The tool executor must not be able to call policy engine internals. Consider using Rust module visibility (`pub(crate)` restrictions) to enforce this at compile time.

---

## 15. LOW — Sub-policy fetch failure is now a WARNING, not a hard failure

**Severity**: LOW

The current `entrypoint.sh` prints `WARNING: Failed to fetch sub-policies` and continues. CRITIQUE v1 item #5 recommended fail-closed behavior. SPEC-v2 does not address this — it inherits the warn-and-continue pattern.

If sub-policies contain additional deny rules (e.g., the "even operators can't" list from the OPERATOR role), failing to load them silently expands the effective permissions of the container.

**Remediation**: As recommended in CRITIQUE v1: fail closed. If `SUB_POLICIES_URL` is set and the fetch fails, the container must not start. Remove the warning-only path. If sub-policies are optional for a role, do not set `SUB_POLICIES_URL`.

---

## 16. LOW — Execution order puts minification before security

**Severity**: LOW

SPEC-v2 section 10 lists "Minification (Cargo.toml changes + Dockerfile rewrite)" as step 1, with MockProvider at step 2 and adversarial tests at step 7. This means the security testing infrastructure does not exist when the most architecturally disruptive change (scratch base, single binary, musl) is made.

**Remediation**: Reorder: (1) MockProvider, (2) Tool use limits, (3) Adversarial tests with the current Dockerfile, (4) verify all tests pass, THEN (5) minification. Never make the architecture change before the test harness exists to catch regressions.

---

## Summary

| # | Severity | Title |
|---|----------|-------|
| 1 | CRITICAL | FROM scratch eliminates /dev/urandom; token generation unspecified |
| 2 | CRITICAL | FROM scratch has no shell/curl; contradicts thomcom_shell requirement |
| 3 | CRITICAL | Single binary merge collapses security boundary between enforcer and enforced |
| 4 | HIGH | MockProvider creates false confidence; cannot discover unknown attacks |
| 5 | HIGH | AP socket protocol has no replay protection, no nonces, static per-role keys |
| 6 | HIGH | Tool use limits gameable via text-only exfiltration and Done spoofing |
| 7 | HIGH | TOCTOU race conditions on file-watch inbox; symlink injection |
| 8 | HIGH | reqwest 0.11/0.12 split creates two TLS stacks in merged binary |
| 9 | MEDIUM | thomcom_shell is unaudited, bypasses policy engine, impossible on scratch |
| 10 | MEDIUM | Entrypoint.rs PID 1 signal handling, panic behavior on scratch |
| 11 | MEDIUM | opt-level "z" may remove security-relevant bounds checks |
| 12 | MEDIUM | Per-role key baked at build time is a static credential in image layers |
| 13 | LOW | MockProvider respond_from_file is arbitrary file read if not cfg-gated |
| 14 | LOW | Single binary eliminates network boundary for policy enforcement |
| 15 | LOW | Sub-policy fetch failure is warn-only, not fail-closed |
| 16 | LOW | Execution order builds minification before test harness exists |

### Key Architectural Concern

SPEC-v2's primary goal — minification to ~9MB — is in direct tension with its security requirements. The single-binary merge (section 1.4) is the most dangerous proposal: it collapses the process isolation between the policy enforcer (protectgateway) and the policy-enforced process (agenticlaw), turning a network-boundary security model into an in-process honor system. This must not ship as designed.

### Unresolved from CRITIQUE v1

CRITIQUE v1 items #1 (loopback bypass), #2 (bash obfuscation), #4 (/proc restrictions), #7 (TOCTOU symlinks), #9 (SYS_PTRACE), #10 (LD_PRELOAD), #13 (POKE GET-only string matching), and #14 (resource exhaustion) are NOT addressed by SPEC-v2. They remain open vulnerabilities.
