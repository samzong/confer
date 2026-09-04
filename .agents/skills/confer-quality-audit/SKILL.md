---
name: confer-quality-audit
description: Audit Confer's saved room metadata against the current repository contract and implementation. Use when the Confer author asks for a periodic quality review, room-usage pattern analysis, design-debt assessment, or contract-drift check; do not use for ordinary multi-agent coordination.
---

# Confer Quality Audit

Perform an author-side, read-only product audit. Analyze Confer itself; do not create, message, or mutate rooms as part of the audit.

## Evidence Boundary

Read `~/.confer/rooms.json` directly. Do not use Recall or participant transcripts unless the user explicitly expands the audit to conversation content. The saved file is the evidence for what Confer itself can preserve and expose.

Before drawing conclusions:

1. Reconcile the repository with `git status --short --branch` and recent `git log`.
2. Run `jq -f <skill-directory>/scripts/summarize-rooms.jq ~/.confer/rooms.json`.
3. Inspect the raw records needed to understand names, instructions, lifecycle, requested model settings, and native-session presence. Do not reproduce full native session IDs or unrelated private instructions in the report.
4. Compare the observed pattern with the live contract in `docs/SPEC.md`, `README.md`, `src/types.rs`, `src/state.rs`, `src/mcp.rs`, `src/adapters.rs`, and `skills/confer/SKILL.md`.
5. When installed behavior matters, inspect `confer mcp capabilities` and clearly distinguish the installed binary from the current checkout.

If the state file is absent, malformed, or uses an unsupported schema, report that boundary rather than substituting another history source.

The script's `test_like` classification is a heuristic. Report test-like and other rooms separately, but inspect ambiguous rooms before excluding them from the product sample.

## Audit Lenses

Evaluate only claims supported by saved metadata or current code:

- whether one room maps to one task, one phase, or one external session;
- whether seats can evolve as planning, implementation, and review roles change;
- whether prose authorization matches enforced sandbox and filesystem authority;
- whether one seat has safe busy, ordering, queue, cancellation, and retry semantics;
- what survives MCP restart and whether uncertain deliveries remain auditable;
- whether room creation, discovery, deletion, expiration, and pruning match actual accumulation;
- whether requested and resolved agent, model, and reasoning settings are reproducible;
- whether review work binds to a stable artifact fingerprint while writers share the workspace;
- whether replacement behavior preserves the caller's intended role and capability;
- whether `SPEC.md`, the bundled skill, MCP schemas, and implementation describe the same product.

Keep these evidence rules explicit:

- A native session ID proves that a seat started, not that it succeeded or contributed a usable result.
- `created_at` and `updated_at` bound room lifecycle, not exact delivery runtime.
- Overlapping room windows show possible concurrency, not simultaneous file access.
- A null model or reasoning field means the request was unpinned; it does not identify the model the native CLI actually used.
- Agreement, disagreement, code quality, and user-visible outcomes cannot be reconstructed when messages and outputs are not persisted.

## Findings

Separate findings into:

- verified current defects or contract contradictions;
- design debt exposed by repeated room patterns;
- missing evidence caused by the persistence model;
- healthy usage patterns worth preserving.

For each finding, provide severity, exact evidence, user or maintainer impact, root design cause, and the smallest viable correction. Bind code claims to current file paths and line numbers. Do not turn every limitation into a defect when it is an explicit non-goal; instead state which product promise or observed workflow makes the limitation consequential.

Lead with a decisive verdict and sample boundary. Then summarize strengths, findings, recurring patterns, and the minimal recommended target contract. Include the strongest rejected alternative and the owner-owned product trade-off that would select it.

Do not mutate source, state, Git, native sessions, or external systems. A request to implement audit findings is a new scope.
