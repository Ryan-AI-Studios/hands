---
name: ai-brains
description: >
  Load at session start (run preflight), before risky changes, or when searching past decisions.
  For Helping Hands: run all ai-brains CLI commands with cwd C:\dev\Helping-Hands\hands
  (init there; never in planning root C:\dev\Helping-Hands).
---

# AI-Brains + Ledgerful: Product Development Skill

## Helping Hands — project cwd rule (read first)

| | |
|---|---|
| **Init** | `ai-brains context` (and related setup) only in **`C:\dev\Helping-Hands\hands`** |
| **Run every CLI** | Shell cwd = **`C:\dev\Helping-Hands\hands`** — not `C:\dev\Helping-Hands` planning root |
| **Pair with** | `ledgerful` also only from that same product path |
| **Not inited yet** | Skip CLI; continue with ADRs/research; note unavailability |

```powershell
cd C:\dev\Helping-Hands\hands
ai-brains preflight --summary
ai-brains sync query "<topic>"
```

This skill file may live under `hands\.agents\skills\`; **CLI cwd is always the product tree**.

## What Each Tool Does

| Tool | Role |
|------|------|
| **Ledgerful** | Change intelligence for the current repo — impact analysis, hotspots, ledger provenance, verification planning |
| **ai-brains** | Persistent memory vault — cross-session recall, past decisions, safety signals, code symbol search across projects |

They are interoperable: Ledgerful nightly feeds code symbols, hotspots, and ledger ADRs into ai-brains recall so a single recall query covers both past decisions and live code structure.

## New Repo Setup (Do This Once)

```powershell
# Helping Hands: always from the product repo
cd C:\dev\Helping-Hands\hands

# 1. Initialize project context — writes .env with AI_BRAINS_PROJECT_ID and AI_BRAINS_SESSION_ID
ai-brains context

# 2. Sync Ledgerful hotspots into vault as pinned safety signals (after ledgerful init)
ai-brains safety sync

# 3. Verify preflight works
ai-brains preflight --summary
```

`AI_BRAINS_VAULT_PATH` and `AI_BRAINS_PROJECT_ID` are resolved from `.env` automatically after `context` runs — no need to pass `--vault-path` or `--project-id` on every command. For this project, that `.env` should live under **hands** (product).

## Session Start Workflow

```powershell
cd C:\dev\Helping-Hands\hands

# Safety signals from past sessions for this project
ai-brains preflight --summary

# Current repo state (after ledgerful init)
ledgerful ledger status --compact
ledgerful doctor
```

## When to Reach for Each

| Goal | Command |
|------|---------|
| Unified search — past decisions + live code symbols | `ai-brains sync query "<topic>"` |
| Past decisions or cross-session architectural history only | `ai-brains recall "<topic>" --semantic` |
| Safety signals before a risky edit | `ai-brains preflight --summary` |
| Find a live function, route, or symbol by name | `ledgerful search "functionName"` |
| Natural language code queries | `ledgerful ask "find all GET handlers"` |
| Blast radius of a proposed change | `ledgerful scan --impact` |
| Provenance — why was this changed? | `ledgerful ledger search "<topic>"` |

`ai-brains sync query` is the primary search path — it queries both tools in one shot. Use `recall` alone only when you want memory-only results.

## Recall (Memory Retrieval)

```bash
# Unified recall across ai-brains + Ledgerful (preferred)
ai-brains sync query "<topic>"

# FTS keyword recall
ai-brains recall "<topic>" --limit 5

# Semantic recall (requires Ollama + nomic-embed-text on port 8083)
ai-brains recall "<topic>" --semantic --limit 5

# Graph-boosted recall (scores boosted by graph neighbors)
ai-brains recall "<topic>" --semantic --graph-boost 0.1 --limit 5
```

`ai-brains recall` returns JSON by default. `ai-brains sync query` returns human-readable output.

`ai-brains recall` also returns code symbols (functions, routes) ingested from Ledgerful nightly — a single recall query covers both past decisions and code structure.

## Pinning Decisions and Constraints

```bash
# Pin a decision directly to the vault
ai-brains pin "DECISION: Switched auth to JWT for stateless scaling"

# Pin linked to a Ledgerful ledger transaction (strongest provenance)
ai-brains pin "DECISION: Migrated to CozoDB for graph queries" --tx-id <ledgerful-tx-id>

# Pin a constraint with a tag
ai-brains pin "CONSTRAINT: Never store PII in telemetry" --tag security
```

Pinned memories with DECISION/CONSTRAINT/HOTSPOT prefixes are promoted to `preflight` context automatically.

## Vault Operations

```bash
# Ingest project docs, notes, or specs
ai-brains ingest --project-id <id> --path <path>

# Sync Ledgerful hotspots into vault as pinned safety signals
ai-brains safety sync

# Run nightly pipeline manually (summarization, embeddings, symbol sync)
ai-brains nightly

# Detect project from current git repo
ai-brains project detect

# Resolve a project alias
ai-brains project resolve "<name>"
```

## Nightly Pipeline

Runs automatically on schedule. Keeps recall fresh without manual action:

1. Session summarization and hierarchical synthesis
2. Embedding backfill and stale refresh
3. MADR ingestion from Ledgerful ledger
4. Ledgerful symbol index sync into recall

If recall results feel stale, run `ai-brains nightly` manually.

## Safety & Privacy

- Privacy filter runs on all ingested content — never expose API keys, passwords, or tokens
- Redaction automatically masks `sk-...`, `ghp_...`, `SG.*` patterns
- Pinned memories (HOTSPOT, CONSTRAINT, DECISION) are promoted to preflight context
- Graph failures are non-fatal — memory append always succeeds even if graph projection fails

## Windows Notes

- `AI_BRAINS_VAULT_PATH` env var eliminates need to pass `--vault-path` on every command
- Set `$OutputEncoding = [System.Text.Encoding]::UTF8` in PowerShell sessions
- Cap nightly sessions with `$env:AI_BRAINS_NIGHTLY_BATCH = "50"` if runs are slow
