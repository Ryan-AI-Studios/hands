# Ledgerful Command Reference

Agent-oriented command sheet (not a complete man page). For the full surface run
`ledgerful --help` / `ledgerful <cmd> --help`. Prefer **`--json`** when an agent must parse output.

## Daily 5 (agent default path)

| # | Command | Role |
|---|---|---|
| 1 | `ledgerful doctor --json` | Session/env readiness (`readyForPublish`); if `binary-behind-tree`, reinstall before trusting `--help` / new flags |
| 2 | `ledgerful change-context --json` | Default pre-edit packet |
| 3 | `ledgerful ledger status --compact` or `--json` | Provenance / pending / drift |
| 4 | `ledgerful search …` (prefer `--auto-index` when stale) | Discovery (not full impact) |
| 5 | `ledgerful verify --scope fast` | Local gate (pre-push style); **≠** full CI; self-repairs **head lag**; empty mapping still needs index / `--auto-index` |

**Step 5 (0145):** bare `verify --scope fast` self-repairs **head lag**; empty
`test_mapping` still refuses (exit ≠ 0, `refusing full suite`) without
`index --incremental` / `--auto-index` — never surprise full. Live-clean tree →
cheap EmptyChanges even with a non-empty saved packet. Shared infra → full + announce.

**Escalate (not Daily 5):** `scan --impact --json` (B2 only); `index --incremental` /
`--full` / search `--auto-index` (freshness); `verify --scope full` / CI (not local
fast gate). doctor ≠ verify ≠ full CI. Empty-tree 0129; index/search freshness 0128/0126.
Packet schema: `docs/agent-output-contract.md`.

## Health & setup

```bash
ledgerful doctor                         # First step: environment / index / config health
ledgerful doctor --json                  # Pure schema-v1 JSON: readyForPublish + findings
ledgerful setup                          # Onboarding wizard
ledgerful update --binary                # Reinstall global binary after engine source edits
ledgerful update --migrate --force       # Migrate state (clears indices, keeps ledger)
```

**`doctor --json`:** branch on **`readyForPublish`** (true iff zero **block** findings).
Optional backends (embed/completion/SCIP/sccache/gemini) never block publish readiness.
`readyForPublish` ≠ verify green — still run `verify --scope fast` (pre-push) / full CI.
Dashboard `doctor-results.json` `failures` = block + non-optional warn (optional excluded).
After `doctor`, sidecar also emits additive `findings` top-N (block+warn, cap 5) so
`change-context` `doctor.topFindings` populates (optional-category warns included).

## Reviewer (RO)

Independent / Codex pure RO path — **not** the implementer preflight. Full matrix:
`docs/reviewer-readonly.md`.

```bash
git status
git diff                                 # or base..HEAD
ledgerful ledger status --json           # read-heavy (prefer existing ledger.db)
ledgerful audit                          # when provenance matters
ledgerful change-context --json          # soft-open when DB exists; optional --base-ref
# SKIP on pure RO: doctor, index, scan --impact, verify, ledger start/commit
```

**Honesty:** full verify / index / cargo / nextest need a **writable** env
(`--sandbox workspace-write` or unrestricted). Never claim full gates in
zero-write RO. Point `LEDGERFUL_STATE_DIR` only at a **populated** `.ledgerful`
(empty temp → false confidence). On RO `not_ready`, continue git-only — do not
loop on `doctor`/`init`/`index`.

## Core Commands

### Agent change packet (default pre-edit)

```bash
ledgerful change-context --json              # DEFAULT pre-edit: budgeted readSet + risk + doctor + ledger
ledgerful change-context --json --base-ref origin/main   # CI / fixed-base structure (prefer over scan --base-ref for agent preflight)
# Cap --max-files 20 default; if readSetCapped or B2 escalate triggers → scan --impact --json
# changeHints (greenfield/mixed/none): mostly pure-adds → suggestedTests ladder + greenfield-ish summary
```

### Impact & Scan

```bash
ledgerful scan --impact                  # Escalate / deep-dive: full change intelligence (not default pre-edit)
ledgerful scan --impact --json           # Machine-readable impact packet (escalate-only for agents)
ledgerful scan --base-ref origin/main    # Diff vs a git ref (CI), not working-tree status
ledgerful scan --base-ref origin/main --impact
ledgerful scan --pr main...HEAD --format json   # PR-style range (mutually exclusive with --impact)
ledgerful scan --out path/to/report.json # Write JSON to file when supported with --json/--impact
ledgerful impact --all-parents           # Include side-branch commits in coupling analysis
ledgerful impact --summary               # One-line triage: RISK | N changed | N couplings
ledgerful impact --dead-code             # Include dead-code confidence analysis
ledgerful impact --telemetry             # Telemetry coverage analysis
ledgerful impact --json                  # Machine-readable impact
ledgerful impact --out path/to/out.json  # Write output file when supported
```

### Verification

```bash
ledgerful verify                         # Run configured or predicted verification (full scope)
ledgerful verify --scope fast            # Scoped via test_mapping; refuse if cannot map (not surprise full)
ledgerful verify --scope fast --auto-index   # Refresh mapping once; still cannot → refuse
ledgerful verify --scope fast --allow-full-fallback  # Opt-in 0061 full on mapping miss
ledgerful verify --scope full            # Full suite (default; CI should always use this)
ledgerful verify -c "cargo clippy -- -D warnings"   # Manual single command
ledgerful verify --no-predict            # Skip predictive suggestions
ledgerful verify --dry-run               # Show the plan without executing
ledgerful verify --signatures            # Offline Ed25519 verification of ledger records
ledgerful verify --signatures --chain    # Chain linkage end-to-end (presented chain only)
ledgerful verify --signatures --against-export PATH  # Checkpoint vs retained head (zip or bare JSON)
ledgerful verify --signatures --against-export PATH --exact  # Freeze: full head equality
ledgerful verify --json                  # Machine-readable report (when supported)
ledgerful verify --explain --entity PATH # Entity-scoped test explanation
```

**Chain checkpoint hygiene:** local `--chain` cannot detect full DB+head rollback.
Periodically `ledgerful export head`, retain **off-machine** (not only under
`.ledgerful/`), then `verify --against-export`. Default is extends-or-equals;
see `docs/chain-checkpoint.md`.

**`--scope fast`** uses the `test_mapping` index to run only the test modules
that cover the changed files, emitting a nextest filterset command (e.g.
`cargo nextest run -E 'test(cli_scan) + test(dead_code_prune)'`). Shared
infrastructure still runs full + announce. Mapping-cannot-scope **refuses**
(exit ≠ 0; `scopeExecuted: "refused"` under `--json`) unless
`--allow-full-fallback`. Empty change set → cheap path (Rust: fmt+clippy only;
non-Rust: zero steps, exit 0). Pre-push uses `--scope fast` without allow. See
`docs/verify-performance.md` / `docs/testing.md`.

### Reset

```bash
ledgerful reset                          # Preserves config, rules, and ledger.db
ledgerful reset --remove-config          # Remove .ledgerful/config.toml
ledgerful reset --remove-rules           # Remove .ledgerful/rules.toml
ledgerful reset --include-ledger --yes   # Destructive: wipe ledger.db
ledgerful reset --all --yes              # Destructive: wipe the entire .ledgerful tree
```

### Intent & Capture (Milestone O)

```bash
ledgerful intent demo                    # Launch the interactive intent capture TUI demo
ledgerful verify --signatures            # Mathematical verification of the entire ledger
```

### Audit & Search

```bash
ledgerful audit [--entity PATH] [--include-unaudited]  # Holistic provenance view
ledgerful ledger audit [--entity PATH]                 # Same as above (legacy alias)
ledgerful ledger search QUERY [--category CAT] [--days N] [--breaking] [--limit N] # FTS5 search
```

## Ledger Subcommands (Provenance)

```bash
ledgerful ledger start PATH --category CAT [--message TEXT] [--issue REF]
ledgerful ledger commit TX_ID --summary TEXT --reason TEXT [--change-type TYPE] [--breaking] [--auto-reconcile | --no-auto-reconcile]
ledgerful ledger rollback TX_ID --reason TEXT
ledgerful ledger atomic PATH --summary TEXT --reason TEXT [--category CAT]
ledgerful ledger status [--entity PATH] [--compact] [--json] [--exit-code] [--verify-signatures]
ledgerful ledger status --global [--repo NAME] [--reindex] [--opt-in|--opt-out]  # multi-repo rollup
ledgerful ledger reconcile [--tx-id ID] [--pattern GLOB] [--all] [--reason TEXT]
ledgerful ledger adopt [--pattern GLOB] [--all] --category CAT --summary TEXT --reason TEXT
ledgerful ledger stack [CAT]                              # Show tech stack and validators
ledgerful ledger register rule TERM --category CAT --reason REASON
ledgerful ledger register validator NAME --command CMD --category CAT [--timeout SEC]
ledgerful ledger adr [--output-dir DIR]                   # Export decisions to MADR
ledgerful ledger graph <tx-id>                            # Entity neighborhood for a transaction
```

## Gate, policy, config (agent-critical)

```bash
ledgerful gate mode                      # Show observe/enforce posture
ledgerful policy check                   # Evaluate declared CI policy
ledgerful config view | verify | schema | diff | set | unset
```

## Topology & security (common review commands)

```bash
ledgerful endpoints [--json] [--changed]
ledgerful services diff
ledgerful data-models impact --changed
ledgerful security boundaries
ledgerful security impact --changed
ledgerful dependencies list | audit
ledgerful observability diff | coverage
ledgerful tests                          # test mapping lookup
ledgerful ci diff | deploy impact
```

## Dead Code Detection

```bash
ledgerful impact --dead-code                         # Include dead-code analysis in impact
ledgerful dead-code [--threshold 0.75] [--limit 50] [--auto-index]
ledgerful dead-code --prune [--threshold 0.75]       # Interactively prune high-confidence dead code
```

`dead-code --prune` iterates through high-confidence findings and prompts
`[Y/n]` per symbol via `inquire`. Approved removals are written to disk and
documented in a `PENDING` ledger transaction with `DELETED` token provenance,
so tests must pass before `ledger commit` finalizes the deletion.

## Live Visualization (feature: viz-server)

```bash
ledgerful viz-server [--port 8765] [--bind 127.0.0.1] [--open]   # Start WebSocket Arc Diagram server
ledgerful viz-server --stop                                       # Stop a running viz server
```

## Watch

```bash
ledgerful watch [--interval 1000] [--json]          # Watch repository for changes
ledgerful watch --no-graph-sync                     # Disable live KG updates during watch
```

## Hotspots & Federation

```bash
ledgerful hotspots --limit 20 --commits 500 [--auto-index]
ledgerful hotspots --json
ledgerful federate status
```

### Indexing & Search

```bash
ledgerful index --docs              # Index markdown documentation
ledgerful index --contracts         # Index OpenAPI/Swagger contracts
ledgerful index --export-docs       # Export KG data to Markdown/Mermaid docs
ledgerful index --export-docs --doc-type module_map --doc-type symbol_index  # Export specific doc types
ledgerful index --full              # Full re-index
ledgerful index --incremental       # Fast refresh
ledgerful search "symbolOrQuery" [--json] [--json-lines] [--auto-index]
```

**`search --json` (0136):** single agent envelope (`schemaVersion: 1`, `results[]`).
Whole-stdout `ConvertFrom-Json` / `JSON.parse` works on multi-hit. Prefer this
for agents (Daily 5). Schema: `docs/agent-output-contract.md` → search section.

**`--json-lines`:** legacy NDJSON BridgeRecord stream (pre-0136 `--json`);
line-by-line only — never whole-parse. Conflicts with `--json`.

**Empty search index:** if doctor reports `search-empty` (or human Index Health
`Empty (0 documents)`), run `ledgerful index` before abandoning for ripgrep.
First `search` also rebuilds when `document_count==0` (CLI empty path). MCP
`search` passes `--auto-index` (0134) for staleness refresh; empty
`document_count==0` rebuild remains complementary inside CLI.
`search --auto-index` full-rebuilds Tantivy when SQLite index work ran
(FullBootstrap/Incremental); no extra FTS rebuild when auto-index no-ops.
No matches ≠ empty index. Under `search --json`, check optional
`searchIndexStatus` (e.g. `was_empty` / `empty_after_rebuild`) before treating
`results` alone as the full story.

## Gemini-Assisted Reporting

```bash
ledgerful ask what is change-context  # unquoted multi-word OK (flags before words)
ledgerful ask "What should I verify next?" [--auto-index]
ledgerful ask --mode suggest "What checks should I run?"
ledgerful ask --mode review-patch "Review the current diff."
ledgerful ask --narrative
```

## Nightly Graph Indexing Scheduler

```bash
ledgerful schedule setup-nightly                # Install nightly `git fetch` + `index --analyze-graph`
ledgerful schedule setup-nightly --dry-run      # Print the generated scheduler syntax without registering it
ledgerful schedule setup-nightly --uninstall   # Remove the scheduled task
ledgerful schedule run-nightly                   # Run the sequence directly (git fetch, then index --analyze-graph)
```

- On **Windows** the command registers a `schtasks` daily task at 02:00 named `LedgerfulNightlyIndex`.
- On **macOS/Linux** it installs a crontab line at `0 2 * * *` that runs `ledgerful schedule run-nightly`.
- Output is appended to `.ledgerful/logs/nightly.log` with RFC3339 timestamps.

## Categories

Use with `ledgerful ledger start|atomic|… --category <CAT>`. Matches engine `Category` (`types.rs`):

| Category | Covers |
|---|---|
| `ARCHITECTURE` | High-level system design, multi-module contracts |
| `FEATURE` | New user-facing or internal functionality |
| `BUGFIX` | Defect repairs |
| `REFACTOR` | Structural improvement without behavior change |
| `INFRA` | CI, git hooks, Docker, build system |
| `SECURITY` | Auth, authz, crypto, disclosure, supply-chain security work |
| `TOOLING` | Internal scripts, dev tooling |
| `DOCS` | Documentation, README, ADRs |
| `CHORE` | Dependencies, formatting, minor cleanup |

## Not exhaustively listed here

Feature-gated or less agent-critical surfaces — use `--help` / product docs:

- `sync` **[Available — opt-in shared-folder v1]** — encrypted team ledger bundles; `[sync].enabled=false` default forever until you opt in. Pairing real (`LF-PAIR-1` + `sync pair`), setup checklist / gated `setup --enable` / status next-action real; never auto-enables; setup/status never prompt for secret. See `docs/team-sync.md`. Not `watch` Real-time Sync / not `federate`.
- `web`, `usage`, `openapi`, `export evidence`, `export head` (thin chain_head.json checkpoint), `bridge`, `mcp`, `demo`, `timings`, `viz-server`
- Full `ledger` advanced subcommands (`re-sign`, `gc`, `export-public`, validators, ADR subcommands, …)
