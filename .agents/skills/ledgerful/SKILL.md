---
name: ledgerful
description: >
  Use for code edits, reviews, impact/risk, verification, drift, ledger provenance. Prefer --json.
  For Helping Hands: run all ledgerful CLI with cwd C:\dev\Helping-Hands\hands (init there;
  never in planning root C:\dev\Helping-Hands). Before edits: doctor --json then change-context --json.
---

# Ledgerful

## Helping Hands — project cwd rule (read first)

| | |
|---|---|
| **Init** | `ledgerful init` only in **`C:\dev\Helping-Hands\hands`** |
| **Run every CLI** | Shell cwd = **`C:\dev\Helping-Hands\hands`** — not `C:\dev\Helping-Hands` planning root |
| **Pair with** | `ai-brains` also only from that same product path |
| **Not inited yet** | Skip CLI; continue with ADRs/research; note unavailability |

```powershell
cd C:\dev\Helping-Hands\hands
ledgerful doctor --json
ledgerful change-context --json
```

This skill file may live under `C:\dev\Helping-Hands\.agents\skills\` or `hands\.agents\skills\`; **CLI cwd is always the product tree**.

---

Use Ledgerful as the local safety layer and engineering intelligence engine for code changes. The
canonical CLI is **`ledgerful`** (short alias **`ldg`** may be present from the shell installer). It
provides impact analysis, hotspot and temporal-coupling signals, verification planning, and
transactional provenance.

## Core Capabilities

- **Search & Discovery**: High-performance regex (Tantivy), optional SCIP edge augment (`index --auto-scip`, off by default), and conceptual semantic search (local embeddings) with parallel HNSW retrieval.
- **Code Symbol Index**: Tree-sitter parsing of Rust, TypeScript, and Python — extracts every public function, struct, enum, trait, module, and HTTP route into the Knowledge Graph. Queryable via `ledgerful search` and `ledgerful ask`.
- **Gemini Token Budgeting**: Automatically calculates character limits based on `config.gemini.context_window`. Appends `[Packet truncated for Gemini submission]` when limits are hit to ensure predictable LLM behavior.
- **Route Extraction**: Detects HTTP routes from Axum, Express, and other frameworks. Stores `method`, `path_pattern`, `handler_name`, `framework`, and confidence score.
- **Call Graph**: Tracks function call relationships (`Direct`, `MethodCall`, `TraitDispatch`, `Dynamic`, `External`) so you can answer "what calls this function?" and "what does this function depend on?".
- **Knowledge Graph**: Durable, billion-edge relational and vector storage (CozoDB-redux/Sled) with native code-aware tokenization (Tree-Sitter). Stores symbols in `project_symbol` table.
- **AI-Brains Bridge**: Exports hotspots, ledger entries, and MADR data to AI-Brains via `ledgerful bridge export --hotspots --ledger [--madr] [--stdout]`. AI-Brains nightly pipeline ingests this output as code symbols into recall (T70). Inbound recall uses `ledgerful bridge query "<text>"` (IPC with CLI fallback).
- **Impact Analysis**: Deep "blast radius" analysis across 20+ specialized providers (Infra, Contracts, Observability, Temporal). Blast edges carry `confidenceClass`; change-context/blast expose `confidenceSummary` counts (not full edges on change-context). Change-set `affectedFlows` lists registered HTTP routes touched (route map, not CRG call-chain traces).
- **Cryptographic Provenance**: Mathematical proof of intent via Ed25519 signing of every ledger entry. Offline verification via `verify --signatures`. Chain continuity via `verify --signatures --chain`. Independent rollback detection: `export head` + off-machine retention + `verify --against-export` (checkpoint extends-or-equals; `--exact` for freeze). See `docs/chain-checkpoint.md`.
- **Intent Capture TUI**: Interactive terminal UI for auditing and refining LLM-drafted intent payloads during the git commit process.
- **Real-time Sync (watch)**: Incremental Knowledge Graph updates, AST re-parsing, and code-aware symbol indexing via the `watch` command — **not** team ledger sync.
- **Predictable Verification**: Bayesian test reordering and CI failure prediction.
- **Documentation Generation**: Export Knowledge Graph data to Markdown/Mermaid passive documentation (`index --export-docs`).
- **Dead Code Detection**: Confidence-based dead code detection blending graph reachability, git activity, and test history (`dead-code` command). Use `dead-code --prune` for interactive opt-in removal wrapped in a pending ledger transaction.
- **Scoped Verification**: `ledgerful verify --scope fast` uses the `test_mapping` index to run only the tests covering changed files (nextest filtersets). Shared infrastructure still runs full; mapping-cannot-scope **refuses** (not surprise full) unless `--allow-full-fallback`. Empty changes → cheap path (Rust: fmt+clippy; non-Rust: zero steps, exit 0). The pre-push hook uses `--scope fast`; CI uses `--scope full`. See `docs/testing.md` / `docs/verify-performance.md`.
- **Nightly Scheduler**: Cross-platform nightly indexing via `ledgerful schedule setup-nightly` (Windows schtasks / Unix crontab), with `--dry-run` and `--uninstall`. Runs `git fetch` + `index --analyze-graph` sequentially, logging to `.ledgerful/logs/nightly.log`.
- **Live Visualization**: WebSocket-based Arc Diagram for real-time Knowledge Graph updates (`viz-server`, `viz-server --stop`).
- **Endpoints**: Indexed endpoint graph with auth, schemas, consumers, and owner links. `ledgerful endpoints --json` / `--changed` (matches handler symbol, impl file, registration file, or blast edges — not registration-only). Change-set `affectedFlows` on impact / change-context / PR is the same route-map signal (sample-capped on reports; filter uncapped).
- **Services Diff**: Declared service map with queue/topic/RPC edges and PR-style boundary diff. `ledgerful services diff`.
- **Data Models**: Durable data model, table, migration, and compatibility-class relations with impact rules for destructive changes. `ledgerful data-models impact --changed`.
- **Config Schema & Diff**: Explicit env var schema metadata (required/secret/owner/provider) and change diff. `ledgerful config schema` / `ledgerful config diff`.
- **Dependency & Advisory Graph**: Cargo/npm/Python lockfile ingestion with cargo-audit/osv advisory matching. Impact rules for vulnerable dependency introduction.
- **Test Mapping**: Durable test nodes linked to endpoints, symbols, services, and data models. `ledgerful verify --explain --entity <path>` for entity-scoped test explanation.
- **Observability Graph**: SLO, metric, alert, and signal nodes from OpenSLO YAML. Source-file-backed diff matching. `ledgerful observability diff` / `observability coverage`.
- **Hotspot Trends**: Persistent hotspot and temporal coupling snapshots with trend deltas. `ledgerful hotspots trend` / `hotspots explain`.
- **Ledger Graph**: Per-transaction entity neighborhood view linking ledger entries to symbols, endpoints, services, ADRs, config keys, and deploy surfaces. `ledgerful ledger graph <tx-id>`.
- **Ledger Validator Lifecycle**: Full validator lifecycle with `ledger validator list`, `disable`, `enable`, `remove`, `doctor`, and hook-repair rollback for sidecar/pending mismatches.
- **Security Boundaries**: Cedar policy parsing with cross-surface links (policy→endpoint/service/config_key/deploy_surface/ADR). `ledgerful security boundaries` / `security impact --changed`.
- **Team Sync [Available — opt-in shared-folder v1]**: Opt-in encrypted ledger entry bundles via `ledgerful sync` (default feature; `[sync].enabled = false` forever until you opt in). Pairing (`LF-PAIR-1` + `sync pair`), secure shared-folder transport/apply (`.lfbundle`, verify-then-apply), and low-friction ops (`sync setup` checklist, gated `setup --enable`, status next-action) are real (0110–0113). Not default-on, not cloud, not CRDT. Never auto-enables; setup/status never prompt for secret. See `docs/team-sync.md`. Not the same as watch “Real-time Sync”.



## Philosophy: CLI-First Intelligence

Ledgerful is a **CLI-first** tool. Prefer the `ledgerful` binary (or `ldg` if that alias is on
`PATH`) for discovery and safety. MCP (`ledgerful mcp` stdio; `ledgerful mcp install` for
Top-N host wiring — see `references/mcp.md`) and the local dashboard
(`ledgerful web start`) are optional surfaces on default builds.

**Machine-readable output:** many commands accept **`--json`** (and related format flags). Agents
should prefer `--json` when parsing stdout so human progress lines do not break consumers. Pair with
quiet/machine expectations documented in `docs/operator-surface-policy.md` when present.

**Index freshness (short card):** full policy in `docs/index-freshness-policy.md`.

- Prefer `--auto-index` on **search / ask / hotspots / dead-code** when stale.
- **`verify --auto-index` only fixes `test_mapping` for `--scope fast`** — not general bootstrap.
- **`scan` / `scan --impact` have no `--auto-index`** — refresh first if freshness matters.
- Doctor green ≠ index fresh (Graph Index Health is age + content when age-fresh; `index --check` remains readiness JSON SoT).
- Light continuous: `ledgerful watch`. Heavy: `schedule setup-nightly` / `index --full` / explicit `--auto-scip`.
- Never idle SCIP. `init` installs no watcher/schedule.

## Git worktrees

Linked worktrees share ledger state with the primary worktree's `.ledgerful` (same pending TX and `ledger.db`). Run `ledgerful` commands from the worktree cwd; do not copy state into the linked tree. Submodules keep their own `.ledgerful`. Set absolute `LEDGERFUL_STATE_DIR` to override.

## Daily 5 (agent default path)

Scannable day-to-day subset — not a replacement for the full Default Workflow below.
Prefer **`--json`** on doctor / change-context / ledger status when parsing.
Packet + colour env: `docs/agent-output-contract.md`. Full command sheet:
`references/commands.md`.

| # | Command | Role |
|---|---|---|
| 1 | `ledgerful doctor --json` | Session/env readiness (`readyForPublish`); if `binary-behind-tree`, reinstall (`cargo install --path . --force`) before trusting `--help` / new flags |
| 2 | `ledgerful change-context --json` | Default pre-edit packet |
| 3 | `ledgerful ledger status --compact` or `--json` | Provenance / pending / drift |
| 4 | `ledgerful search …` (prefer `--auto-index` when stale) | Discovery (not full impact) |
| 5 | `ledgerful verify --scope fast` | Local gate (pre-push style); **≠** full CI; self-repairs **head lag**; empty mapping still needs index / `--auto-index` |

**Step 5 notes (0145):** bare `verify --scope fast` **self-repairs head lag**
(populated mapping, lagging `head_hash`) with one bounded incremental refresh.
Live-clean trees use a cheap EmptyChanges plan even if a saved impact packet is
non-empty. **Empty** `test_mapping` still **refuses** without
`index --incremental` or `--auto-index` — it will **not** surprise-run a
multi-minute full suite. Remediation when refused:

```bash
ledgerful index --incremental
ledgerful verify --scope fast --auto-index
# or deliberate full / old fallback:
ledgerful verify --scope full
ledgerful verify --scope fast --allow-full-fallback
```

Empty tree (no material file changes) → cheap plan (Rust: fmt+clippy, no nextest;
non-Rust: zero steps, exit 0). Shared infra still runs full with an announcement.

**Escalate (not Daily 5):**

- `scan --impact --json` — B2 only (readSetCapped / high risk multi-module / unclear public API / user DoD / change-context not_ready)
- `index --incremental` / `--full` / search `--auto-index` — freshness
- `verify --scope full` / CI — not the local fast gate

**Honesty:** doctor ≠ verify ≠ full CI. Empty-tree packets stay low risk with
`analysisWarnings` (0129). Index/search freshness: prefer `--auto-index` or
`index --incremental` (0128/0126) — not bare full impact as a refresh step.

## Default Workflow

**Default preflight ladder** = doctor → audit → ledger status → **change-context --json**.
Full `scan --impact` is **escalate-only** (B2), never a peer default of change-context.

1. Confirm the tool is healthy (session start / first tool use — same as `AGENTS.md`):

   ```bash
   # Prefer --json when parsing; branch on readyForPublish (zero block findings).
   ledgerful doctor --json
   # Human: ledgerful doctor
   ```

   **`readyForPublish == true`** means the publish-environment path is fit to enter
   (no lifecycle/tool **block** findings). It does **not** mean `verify` passed,
   tests are green, or CI is green. Optional backends (embedding/completion/SCIP/
   sccache/gemini) never set `readyForPublish=false`.

   **Signing hygiene:** when doctor reports `sig-pin` / `sig-version`, follow the
   structured `remediation` commands (or human lines under the finding). Pinning
   `intent.trusted_public_keys` is an identity allowlist — not free-text proof of
   intent. Use `ledger re-sign --all` to upgrade legacy v1 rows before raising
   `min_sig_version=2`; use `--all-invalid` only for key-repair of broken sigs.

2. Check provenance / drift before editing:

   ```bash
   ledgerful audit
   ledgerful ledger status --compact
   # or machine-readable:
   ledgerful ledger status --json
   ```

   Skip status only for pure docs/conductor prose with no ledger work.

3. **DEFAULT pre-edit** for meaningful code/config/policy: budgeted agent change packet
   (schema: `docs/agent-output-contract.md`):

   ```bash
   ledgerful change-context --json
   # CI / fixed base for structure only: --base-ref origin/main
   # (doctor + ledger always report present workspace state)
   # Cap: --max-files 20 (default)
   ```

   Read only the paths in `readSet` first. Use `riskLevel`, `doctor.readyForPublish`,
   `ledger.pendingCount`, `blast.confidenceSummary` (class counts when blast is
   present — not full edges), `testCoverage` (structural test gaps — status enum,
   capped unmapped, notes; **not** line coverage; LCOV COVERAGE does not persist),
   and `changeHints` (greenfield / new-surface + budgeted `suggestedTests` when
   mostly pure-adds or a new package prefix; omit on empty/not_ready; convention
   ≠ proven coverage) for accountability. Empty mapped lists are **not** “fully
   covered.” Deep-dive with `ledgerful tests <entity>`. PR JSON always has
   `testGaps`; CI without index → `unavailable` (honest default). Do not invent
   token-reduction claims from the packet size.

4. **Escalate** to full impact only when a B2 trigger fires (not by default):

   ```bash
   ledgerful scan --impact --json
   ```

   | Trigger | Why |
   |---|---|
   | `readSetCapped == true` | Budget hid files |
   | `riskLevel` high (or medium **and** multi-module / shared infra in readSet) | Accountability |
   | Diff spans many packages/languages or public API unclear | depth-1 default |
   | User/DoD requires full impact | Process |
   | change-context `status` error/not-ready (**not** merely `empty`) | Fallback |

   **De-escalate:** After an escalated `scan --impact`, return to **change-context**
   for subsequent edits unless a B2 trigger re-fires. Do not pin full impact as the
   session default after one escalation.

   `.ledgerful/reports/latest-impact.json` is an **escalate-tier cache only** — never
   a default before step. Prefer live `change-context` (does not rewrite that file).

5. **Skip / lighten** preflight when:

   | Case | Guidance |
   |---|---|
   | Trivial format/lockfile/binary/scratch/explicit bypass | Skip Ledgerful |
   | Pure conductor/docs prose, no product code | doctor optional; no change-context required |
   | `status: "empty"` | Expected when no file changes + no pending ledger — **not** failure; do **not** escalate solely for empty |
   | status empty + `riskLevel` ≠ low | Do **not** escalate solely because riskLevel ≠ low when status==empty |
   | status empty + only federation schema warnings | Schema-unavailable siblings land under `analysisWarnings` with `riskLevel=low` (0129) — ambient federation health, not diff risk; do **not** treat as medium escalation |
   | `search-empty` | Documented in `references/commands.md`; not a reason for full impact |

6. Make the smallest scoped change that satisfies the task.

7. After edits, run:

   ```bash
   ledgerful verify
   # scoped local gate (pre-push style): ledgerful verify --scope fast
   # optional re-check: ledgerful change-context --json
   ```

   Also run any repo-specific tests needed for the touched files.

8. For final gates, avoid overlapping `cargo`, `nextest`, or `ledgerful
   verify` jobs. Parallel read-only inspection is fine, but final verification
   should run sequentially to avoid Windows file-lock and linker contention.

9. Report the outcome: impact/risk signals used, verification run, and any
   unresolved pending transactions, drift, or unavailable Ledgerful command.

## Code Symbol Queries — Use These First

Before searching the web or reading files manually, query Ledgerful's symbol index. It knows every public function, struct, route, and call edge in the codebase.

```bash
# Refresh the index (incremental), or pass --auto-index on the query command below
ledgerful index --incremental

# Optional SCIP edge augment (OFF by default). Use when you need higher-precision
# call edges for impact / blast-radius prep, not as a universal quality KPI.
# Requires a capable indexer (capability probe). Adds structural_edges with
# evidence=scip:ref onto native symbols only — does not replace the native index.
# Under --json, read scip.status, scip.edges_added, scip.edges_updated.
# ledgerful index --auto-scip --json
# ledgerful index --scip path/to/index.scip --json

# Find a function, struct, or type by name
ledgerful search "handleGetUser" --auto-index
ledgerful search "AuthMiddleware"

# Find HTTP routes
ledgerful search "POST /auth"
ledgerful ask "list all HTTP GET route handlers"

# Find what calls a function
ledgerful ask "what calls validateToken"
ledgerful ask "show callers of UserRepository::find_by_id"

# Find all public endpoints
ledgerful ask "find all Axum route handlers"
ledgerful ask "what API endpoints are defined in src/routes"

# Dead code
ledgerful dead-code --threshold 0.75

# Dead code — show everything including standard traits (Eq, Clone, Debug, …)
# By default, standard trait symbols are EXCLUDED because they are used implicitly
# via derive macros or blanket impls and almost always produce false positives.
ledgerful dead-code --include-traits
```

> **Heuristic note**: Dead code analysis blends graph reachability, git inactivity, and
> test coverage. Results are probabilistic, not definitive. Common false-positive patterns:
> - Traits derived via `#[derive(...)]` (Eq, Ord, Clone, Debug, Serialize, …) — suppressed by default.
> - Types ending in `Provider`, `Chunk`, `Record`, `Result` — receive a -0.20 confidence penalty
>   (they are often dispatched dynamically or through serde).
> Use `--include-traits` to restore unfiltered output for auditing purposes.

These queries work because Ledgerful indexes:
- Every `pub fn`, `pub struct`, `pub enum`, `pub trait` via tree-sitter
- HTTP route registrations (Axum `Router::route`, Express `app.get`, etc.)
- Function call edges via static analysis (native call graph)
- Optional SCIP reference edges (`evidence=scip:ref`) when you opt in with `--auto-scip` / `--scip`

Symbols ingested by the bridge become AI-Brains memories (T70) and are returned
by `ai-brains recall "<topic>"` alongside session memories. To verify the
bridge is alive end-to-end, run `ai-brains preflight --summary` and confirm
hotspots and decisions are listed.

## Audit Smoke Tests

When reviewing CLI/config behavior, supplement unit tests with command-level
smoke tests against the current build output, usually `target\debug\ledgerful.exe`
on Windows. Prefer focused temporary repositories and verify failure cases as
well as success cases.

Useful checks include:

- JSON mode remains parseable on failure paths (`config verify --json`, invalid
  `config.toml`, invalid `rules.toml`, unknown `--section`).
- Dry-run commands do not create persistent state or perform external probes
  unless that is explicitly part of the dry-run contract.
- Requested vs effective config values are visible when runtime clamping or
  defaults change the final behavior.
- Internal callsites that construct CLI argument structs still populate new
  fields explicitly.

## Repository Configuration

Ledgerful's `.ledgerful/rules.toml` and `.ledgerful/config.toml` are
repo-local policy, not portable defaults. When installing or copying this skill
into another repository, review and update:

- `required_verifications`: use commands that actually exist in that repo
  rather than aliases such as `lint`, `test`, or `build` unless the repo defines
  those commands.
- `verify.default_timeout_secs`: set a timeout that fits the repo's slowest
  expected verification command.
- `protected_paths`: keep enforcement scoped to paths that make sense for the
  repository.

If `ledgerful verify` fails with "Command not found" or times out while the
same command passes manually, fix the repo-local config before treating it as a
code failure.

`ledgerful init` sanitizes every starter template before creating
`.ledgerful/config.toml`. Secret-bearing keys and credentialed connection
URLs are omitted, including values from `LEDGERFUL_DEFAULT_CONFIG` and
`~/.ledgerful/default-config.toml`. Keep credentials in the environment or
an ignored repo-local `.env` (`GEMINI_API_KEY`, `OLLAMA_CLOUD_API_KEY`, or the
legacy `OLLAMA_API_KEY`); Ledgerful does not interpolate `${VAR}` expressions
inside TOML.

## Dependency Alert Workflow

For Dependabot or audit findings:

- Identify whether the vulnerable crate is direct or transitive with
  `cargo tree -i <crate>@<version>`.
- If the vulnerable crate is transitive through a direct dependency, prefer
  upgrading the direct dependency over adding a downstream patch.
- If the vulnerable path enters through a git dependency, verify whether the
  upstream fix is visible to downstream consumers. Workspace-level
  `[patch.crates-io]` entries in the dependency repository are not transitive.
- Record external remediation handoffs in a conductor track when another repo
  owns the durable fix.
- After dependency changes, run focused dependency checks plus `ledgerful
  verify`.

## When To Skip

Skip Ledgerful only for trivial formatting, simple dependency lockfile updates,
binary/media changes, temporary scratch files, or when the user explicitly says
to bypass it.

## If Commands Fail

- If `ledgerful` is unavailable, continue with normal repo tools and tell the
  user Ledgerful signals were unavailable.
- If `ledger status` shows unaudited drift, reconcile or adopt before continuing
  unless the user directs otherwise.
- If `scan --impact` cannot complete, continue cautiously and include the error
  in the final report.
- If a command reports that the index is `[STALE]`, append `--auto-index` to **`search`, `ask`,
  `hotspots`, `dead-code`** (prefer proactively). Time-stale and content-hash drift both refresh;
  never-indexed runs full bootstrap. Use `index --check --json` / `doctor --json` for readiness.
- **`verify --auto-index` only repairs `test_mapping` for `--scope fast`** — not general index
  bootstrap. Before **`scan --impact`**, refresh first — that command has no `--auto-index`.
- Prefer **`--json`** when an agent must parse command output.
- Do not edit `.ledgerful/` state files directly.
- Doctor / `readyForPublish` green does **not** mean the search/graph index is fresh (Graph Index Health is age + content when age-fresh; `index --check` remains readiness JSON SoT).

## Ledger Provenance

For tracked manual edits:

```bash
ledgerful ledger start <entity> --category <CAT> --message "Intent"
# edit files
ledgerful ledger commit <tx-id> --summary "Done" --reason "Why"
```

For surgical one-command provenance:

```bash
ledgerful ledger atomic <entity> --category <CAT> --summary "Task" --reason "Goal"
```

For lightweight notes or lessons learned:

```bash
# Both positional and --message formats are supported
ledgerful ledger note <entity> "Note content"
ledgerful ledger note <entity> --message "Note content"
```

### Git Hook Lifecycle (Milestone O)

Ledgerful uses a two-phase commit lifecycle to ensure zero phantom records:
1. **`commit-msg`**: Captures intent (agent ledger SoT first; else TUI / conventional / silent LLM). Creates or links a `PENDING` transaction and a sidecar file.
2. **`post-commit`**: Automatically promotes the `PENDING` transaction to `COMMITTED` once the Git commit is finalized. If the Git commit fails, the record remains pending or is safely rolled back on the next attempt.

**Provenance source of truth (0122):** agent `ledger start` / `ledger commit` is intentional SoT. The commit-msg hook must not invent a parallel silent LLM intent or open a second TX when the agent already owns intent. Greppable lines use prefix `[Ledgerful] Provenance SoT:` (target `cli_summary`).

| Agent action | Hook behavior |
|---|---|
| `ledger commit` + git msg with `Ledger: {tx}` | AlreadyCommitted (skip) |
| `ledger start` only (one PENDING) | LinkPending |
| N>1 PENDING, no `Ledger:` (incl. multi-worktree shared DB) | Ambiguous → HookFallback |
| No ledger activity | HookFallback (LLM/silent/TUI) |

**Message binding:** include `Ledger: {tx_id}` on its own line (default `--with-git` template), or optional `Ledger-Tx: {tx_id}`. Bare UUIDs in prose are ignored. Linked worktrees share one `.ledgerful` DB — concurrent multi-worktree agents with two open PENDINGs and no `Ledger:` line hit Ambiguous → HookFallback; always include the TX ref when disambiguation matters.

### Cryptographic Security

If `intent.require_signing = true` is set in `.ledgerful/config.toml`, all ledger entries must be signed by the developer's local Ed25519 key (generated during `init`).

To verify the integrity of the entire ledger:
```bash
ledgerful verify --signatures
ledgerful verify --signatures --chain
```
This performs an offline mathematical validation of every record against its signature and public key, plus chain linkage of the presented chain.

**Independent head retention (operator hygiene):**
```bash
ledgerful export head --out ./checkpoints/head.json
# copy off-machine, then later:
ledgerful verify --signatures --against-export ./checkpoints/head.json
```
Local `--chain` alone cannot detect full rollback when the adversary controls DB + head. See `docs/chain-checkpoint.md`.

**Ledgerful-itself public head (0120):** thin checkpoint at `https://www.ledgerful.dev/ledger/chain_head.json` — download then `verify --signatures --against-export` (no `--against-url`). Customer repos use `export head` + off-machine retention (0119).

## Publish Hygiene

**Dual green (do not collapse):**

| Signal | Means | Does **not** mean |
|---|---|---|
| `doctor` / `readyForPublish` | Zero **block** doctor findings; env fit to enter publish path | Verify/tests/CI green |
| Pre-push hook | `verify --scope fast` + ledger cleanliness (quiet success; structured fail block on stdout — binary-first after PATH upgrade) | Full fmt/clippy/nextest/CI |
| `verify --scope full` / CI | Repo full gate | Doctor readiness |

When asked to push, catch up `main`, or prune branches:

1. Fetch current remote state first:

   ```powershell
   git fetch --all --prune
   git rev-list --left-right --count origin/main...HEAD
   ```

2. If `origin/main` moved, reconcile before staging or pushing. Do not rebase or
   reset over user work without explicit direction.

3. Confirm publish-env readiness (optional parse):

   ```bash
   ledgerful doctor --json   # branch on .readyForPublish
   ```

4. Stage only the intended scope, commit, then push:

   ```powershell
   git push origin main
   ```

   The pre-push hook runs `ledgerful verify --scope fast` (scoped test
   selection via `test_mapping`) plus `ledgerful ledger status`; treat
   that as the authoritative publish gate and report its result. For the
   full suite, run `ledgerful verify --scope full` manually or in CI.
   Doctor green ≠ pre-push green ≠ full CI.

5. Prune conservatively:

   ```powershell
   git remote prune origin --dry-run
   git branch --merged main
   ```

   Delete local branches only when they are listed as merged into `main` and are
   not the active branch. Branch pruning can legitimately be a no-op.

## Reasoning Rules

- If temporal coupling is above 70% for an unchanged file, inspect that file.
- If hotspots are reported, bias verification toward those files first.
- If KG reachability identifies downstream nodes, inspect them before finalizing.
- Treat hooks and CI gates as enforcement. Treat this skill as guidance.

## Maintenance & Upgrades

To keep your Ledgerful environment synchronized with the latest engine features:

```bash
# Safely migrate repository state (clears indices, preserves ledger)
ledgerful update --migrate --force

# Rebuild indices after migration
ledgerful index --semantic
```

## Working On Ledgerful Itself

After changing Ledgerful source code, you can use the built-in update command to reinstall the global binary:

```bash
ledgerful update --binary
```

Alternatively, run manually from the source root:

```bash
cargo install --path .
```

Treat the install step as part of done criteria after Ledgerful source edits,
before publishing or handing the work back.

## Independent / Cross-Model Review (read-only)

For high-risk diffs, a read-only independent review (Codex `codex exec -s
read-only`, restricted subagent, second model) can ground DoD audit without a
writable implementer tree. Full durable matrix:
[`docs/reviewer-readonly.md`](../../../docs/reviewer-readonly.md).

### Honesty ceiling

Full `verify` / cargo / nextest / `index` rebuild / `ledger start|commit`
**require a writable environment**. Never claim full gates in pure zero-write RO.
On storage failure: report unavailable — do not invent impact.

### Command matrix (agent-critical)

| Class | Examples | Pure RO |
|---|---|---|
| A Git | `git status` / `diff` / `log` | Always |
| B Read-heavy | `ledger status`, `audit` | Prefer existing `ledger.db` |
| C Write-open | `doctor` (always); `change-context` soft-opens when DB exists | Doctor: **skip** on pure RO |
| D Write/exec | `index`, `scan`, `verify`, ledger start/commit | **Not** reviewer job |
| E Network | ask/embed probes, caches | Separate from FS RO |

**Hosts:** Codex `-s read-only` (native Windows OK) = pure RO. Codex
`--sandbox workspace-write` (not deprecated `--full-auto`) for Class C/D when
orchestrator authorizes. Claude Bash sandbox = **cwd + `$TMPDIR` writable by
default** — **≠** Codex pure RO.

### Reviewer ladder (B3)

1. `git status` + `git diff` (always).
2. If `ledgerful` on PATH and populated `.ledgerful` (or absolute
   `LEDGERFUL_STATE_DIR` → populated state):
   - `ledgerful ledger status --json` (or `--compact`)
   - `ledgerful audit` when provenance matters
   - `ledgerful change-context --json` (optional `--base-ref`)
   - **Skip** `doctor --json` on pure RO unless workspace-write / pre-written
     `doctor-results.json`
3. If change-context fails RO/permission: git-only + note grounding unavailable
   under pure RO (do not use that phrase for Claude cwd-writable without evidence).
4. **Never** run `verify` / `index` / `scan --impact` as the reviewer unless
   workspace-write (or stronger) **and** the orchestrator authorized write-class
   gates (`codex-review` skill: orchestrator owns gates).

### Env footgun

`LEDGERFUL_STATE_DIR` must be absolute and point at an **existing populated**
`.ledgerful`. Empty temp → empty index false confidence. Worktrees share main
state by default (0108); do not copy state into the linked tree.

### Codex invocation hygiene

```powershell
# -s read-only only. Do NOT invent -a never or --full-auto.
cmd /c "codex exec -C ""C:\dev\Ledgerful"" -s read-only -m gpt-5.4 -o output\review.md ""Review the current diff for regressions. Do not modify files."" < NUL"
```

If the command appears stuck, inspect the output file before waiting longer; the
review may already have written useful findings.

## References

- Command details: `references/commands.md` (agent-oriented subset + categories; use `ledgerful --help` for full surface)
- Install fallback: `references/install.md`
- Architecture/internal notes: `references/internals.md`
- MCP server: `references/mcp.md`
- Local web dashboard: `references/web.md`
