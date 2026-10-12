# Confer agent guide

The supported product contract is [docs/SPEC.md](docs/SPEC.md); feature explorations live in [docs/features.md](docs/features.md). Run `make help` for targets and `make check` before a commit or PR.

## Merge gate

Gate contract. Green on the checks below is the claim that a PR is safe to merge for the listed properties; everything else stays with the human reviewer.

| Property | Local | CI |
| --- | --- | --- |
| Locked dependencies have no known vulnerabilities | `make audit` | `ci.yml`: `make check` (cargo-audit 0.22.2) on ubuntu and macos |
| Formatting | `cargo fmt --all -- --check` | `ci.yml`: `make check` |
| Clippy with `-D warnings`, all targets and features | `make lint` | `ci.yml`: `make check` |
| Tests, all targets and features | `make test` | `ci.yml`: `make check` |
| Release build with locked dependencies | `cargo build --release --locked` | `release.yml` only, on `v*` tags |

Not covered by any check: driving a real agent CLI through an adapter (needs the agent installed and authenticated), MCP tool behavior against a real host, cross-platform behavior beyond ubuntu and macos runners, and documentation claims in `docs/SPEC.md` against live behavior.

Human review owns: changes to the contract in `docs/SPEC.md`, per-agent permission flags, session resume semantics, and anything that widens what a seat may do in the room workspace.

Required status checks: none enforced as of 2026-10-12; `main` has no branch protection or ruleset. Ledger: Issues labeled `gate-miss` and `gate-false-block`.
