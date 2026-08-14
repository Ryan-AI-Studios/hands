# Ledgerful Internals & Safety

This document describes the internal behaviors, safety mechanisms, and technical constraints of Ledgerful. Use this for debugging unexpected behavior or understanding system invariants.

## Safety Mechanisms

- **Unique Transaction Index**: A `PENDING` index in the ledger prevents duplicate open transactions for the same entity.
- **Conditional Updates**: Database status updates are conditional to prevent race conditions during concurrent access.
- **Durable State Protection**: The `reset` command preserves `ledger.db` by default to prevent accidental loss of provenance history.
- **Lexical Path Normalization**: Entity paths are auto-normalized to forward-slashes relative to repo root using lexical cleaning (no filesystem canonicalization needed).
- **Process Boundaries**: Commit validators run with timeouts and isolated stdout/stderr to prevent hanging the CLI.
- **Glob Matching**: Validator and category glob patterns use `globset` for proper path matching rather than simple substring checks.
- **Atomic Rollbacks**: The `atomic_change` command rolls back the started transaction if the commit fails, preventing orphaned `PENDING` entries.
- **Federation Confinement**: Import/export logic uses secure path normalization to ensure sibling data cannot leak into unauthorized directories.
- **Viz Server Process Management**: The server writes a PID file to `.ledgerful/state/viz-server.pid` on startup and cleans it up on exit. `viz-server --stop` reads this file to terminate the process safely.
- **Two-Phase Hook Lifecycle**: Git commits use a `commit-msg` + `post-commit` sequence. The `commit-msg` hook writes a `PENDING` record and a validation sidecar. The `post-commit` hook promotes it to `COMMITTED` only if the Git SHA/message matches the sidecar. This prevents "phantom" committed records when a commit is aborted.
- **Dead Code Scoring**: Blends three local signals (CozoDB reachability, git activity recency, test coverage) with configurable weights. Zero-weight advisory-only risk provider — does not affect risk level calculation.
- **Document Generation Resilience**: Individual template failures are logged via `tracing::warn` and skipped; the export continues with remaining templates. Empty KG produces a warning, not a failure.

## Known Upstream Risks

Ledgerful depends on **CozoDB** (`cozo` 0.7.6) for graph storage and query execution. CozoDB itself is pre-1.0 and was last released in December 2023. Its dependency tree carries several known issues that we monitor but have accepted under our current threat model:

| Advisory / Issue | Severity | Dependency | Impact | Rationale |
| --- | --- | --- | --- | --- |
| `RUSTSEC-2026-0041` | **High** | `lz4_flex` 0.10.0 | Potential memory leak on invalid decompression input | Only exercised when CozoDB reads its own sled-backed data files (`.ledgerful/state/ledger.cozo`). Ledgerful is a local CLI tool with no network exposure and no untrusted input path into the graph store. |
| `RUSTSEC-2026-0042` | Low | `lru` 0.12.5 | Stacked Borrows violation (soundness issue) | No known exploitable security impact; upstream has not released a patched version compatible with CozoDB's constraint set. |
| Unmaintained | Low | `adler`, `bincode`, `fxhash`, `instant` | No direct CVEs | These crates are flagged as unmaintained by `cargo audit`. They are deep transitive dependencies with no active maintainer, but no known exploitable vulnerabilities. |

**Threat Model**: Ledgerful operates entirely on the local filesystem. The CozoDB store is only ever written to by Ledgerful itself, reading from git history and local source files. There is no remote network listener, no deserialization of untrusted user input into the graph engine, and no server mode that accepts external queries (the viz server only serves pre-computed static data over WebSocket).

**Mitigation & Monitoring**:
- `cargo audit` is run regularly to catch new advisories.
- If CozoDB elimination becomes necessary, the identified migration path is **SQLite + Petgraph**: structured data persists in SQLite, and the in-memory graph is hydrated into `petgraph` for algorithmic analysis. This would be a major refactor and is reserved for a future milestone if the risk profile changes.
- CozoDB upstream has no active development as of May 2026; we treat the graph store as a pinned, audited subsystem.

## CLI Behavior Details

> [!NOTE]
> All configuration values below are defined in `.ledgerful/config.toml`.

- **Auto-Reconcile Defaults**: `ledger commit` defaults to the value of `config.ledger.auto_reconcile` (which itself defaults to `true`).
- **Drift Categorization**: The watcher assigns categories to drift based on matched glob patterns in the database or config. Unmatched paths default to `FEATURE`.
- **Stale Thresholds**: Transactions older than `config.ledger.stale_threshold_hours` (default: 24h) are marked as `STALE` in `ledger status`.
- **Lexical Cleaning**: All paths are treated as UTF-8 and normalized to a canonical forward-slash format to ensure cross-platform consistency in the ledger.
