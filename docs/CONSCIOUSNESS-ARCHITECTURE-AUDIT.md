# Consciousness Architecture Audit — Issue #10

**Date:** 2026-02-27
**Scope:** Sleep/wake ownership chain + injection targeting in the consciousness stack

## 1. Sleep/Wake Ownership

### Question: Which layer wakes which?

### Current Implementation

**Ego distillation follows the watcher chain:**
- L1 distills L0's ego (`ego.rs:distill_layer_ego_on_sleep`, layer=0 → uses L1's sessions)
- L2 distills L1's ego (layer=1 → uses L2's sessions)
- L3 distills L2's ego (layer=2 → uses L3's sessions)
- Warm Core distills L3's ego (layer=3 → uses core's sessions)
- Warm Core self-distills its own ego

This matches the **"subconscious restores the conscious"** vision — each watcher knows its target best.

**However, sleep detection and wake orchestration are NOT cascaded:**

1. **Sleep detection** (`stack.rs:process_layer_update`): When a layer emits `AgentEvent::Sleep`, the handler logs it and returns empty. There is no mechanism to notify the watcher layer or trigger ego distillation at runtime.

2. **Wake orchestration** (`stack.rs:launch`): At stack launch, ALL layers are distilled **sequentially in a flat loop** (L0, L1, L2, L3, then core). There is no cascade chain where Core wakes L3, L3 wakes L2, etc.

3. **Missing cascade wake**: If L0 sleeps mid-operation, L1 should detect this (via `.ctx` change or sleep event), distill L0's ego, and restart L0. This does not happen. Sleep mid-session is a dead end — the layer produces an empty response and stops processing that delta.

### Divergence from Vision

| Aspect | Vision | Implementation |
|--------|--------|----------------|
| Ego authorship | Watcher distills target | ✅ Correct |
| Sleep detection | Watcher detects sleep | ❌ Self-detected, no notification |
| Wake trigger | Watcher initiates wake | ❌ Only at stack launch (flat loop) |
| Cascade chain | Core→L3→L2→L1→L0 | ❌ Flat sequential at launch only |
| Mid-session sleep | Watcher recovers layer | ❌ Layer just stops |

### Recommended Follow-up

1. **Issue: Runtime sleep/wake cascade** — When a layer emits `AgentEvent::Sleep`, propagate to the watcher layer via an event channel. The watcher distills ego, clears the sleeping layer's session, and restarts it with the new ego prompt.
2. **Issue: Ordered wake chain** — At launch, wake bottom-up: Core first (self-distill), then L3 (using core), then L2 (using L3), then L1 (using L2), then L0 (using L1). Currently all are distilled independently, which works only because prior `.ctx` files exist from previous runs.

## 2. Injection Targeting

### Question: Inject to L0 only, or inject to layer above?

### Current Implementation

**All injections target L0 (the gateway):**

1. **Layer injection** (`stack.rs:process_layer_update`, line ~570): Only layers ≥ 2 (L2, L3) check correlation against L0's `.ctx` tail. If above threshold, they write an injection file to the shared `injections/` directory.

2. **Core injection** (`cores.rs:process_l3_delta`): Both Core-A and Core-B check correlation against L0's `.ctx` tail and write to the same `injections/` directory.

3. **L1 never injects**: The `layer >= 2` guard means L1's output never triggers injection, even if highly correlated with L0.

4. **Injection consumption** (`injection.rs:read_and_clear_injections`): A single reader (L0/gateway) atomically reads and clears all injection files. No other layer reads injections.

### Analysis

```
Current flow:
  L2 ──inject──→ L0
  L3 ──inject──→ L0
  Core-A ──inject──→ L0
  Core-B ──inject──→ L0

Vision (layer-above):
  L1 ──inject──→ L0
  L2 ──inject──→ L1
  L3 ──inject──→ L2
  Core ──inject──→ L3
```

### Trade-offs

| Approach | Pros | Cons |
|----------|------|------|
| **Current (all→L0)** | Simple, L0 gets all insights immediately, no latency | Lower layers never benefit from deeper analysis; L0 gets unfiltered noise |
| **Layer-above** | Biologically plausible, each layer enriches the next, natural filtering | Latency (insight must travel L3→L2→L1→L0), complexity |
| **Hybrid** | Deep insights (Core/L3) reach L0 urgently, routine (L1) stays local | Most complex to implement, needs priority classification |

### Divergence from Vision

The implementation uses the **"inject to L0 only"** approach. The original vision called for **"inject to layer above"**. Neither is wrong — they're architectural choices with different properties.

### Recommended Follow-up

1. **Issue: L1 injection to L0** — L1 should be allowed to inject (remove the `layer >= 2` guard). L1's attention distillation is arguably the most relevant for L0.
2. **Issue: Evaluate layer-above injection** — If the vision of percolating insights upward is desired, create per-layer injection directories and have each layer read from its own. This is a larger refactor.

## 3. Summary of Code Paths

### Files Audited

| File | Purpose | Lines |
|------|---------|-------|
| `stack.rs` | Stack orchestration, launch, cascade, layer processing | 815 |
| `watcher.rs` | `.ctx` file polling, delta detection | 155 |
| `injection.rs` | Injection write/read, Jaccard correlation | 204 |
| `ego.rs` | LLM-powered ego distillation for wake | 410 |
| `cores.rs` | Dual-core phase-locked system | 788 |

### Key Constants

- Injection correlation threshold: configurable via `ConsciousnessConfig`
- L0 tail chars for correlation: configurable
- Delta max chars: configurable
- Sleep threshold: configurable (`sleep_threshold_pct`, default 0.55 per MEMORY.md)
- Core budget: 200,000 tokens, compaction at half (100k)
- Watcher poll: configurable (`watcher_poll_ms`)
