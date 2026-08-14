# Ledgerful MCP Server

Ledgerful provides a Model Context Protocol (MCP) server that exposes its intelligence as tools for AI coding agents. Tools do not mutate product code; `search` may create or write local `.ledgerful` index state (cache).

## Registration (preferred)

Use the built-in installer for the **Top-N** platforms only:

| Platform id   | Default scope | Notes |
|---------------|---------------|--------|
| `claude-code` | user          | Top-level `mcpServers` in `~/.claude.json` — never `projects[cwd]` for user scope |
| `cursor`      | user          | `mcpServers` in `~/.cursor/mcp.json` |
| `codex`       | user          | `[mcp_servers.ledgerful]` in `~/.codex/config.toml` |
| `copilot`     | project       | **`servers`** + `"type":"stdio"` in `.vscode/mcp.json` (not `mcpServers`) |

```bash
# Detect installed hosts, or pass --platform <id> (repeatable)
ledgerful mcp install
ledgerful mcp install --platform cursor --scope user
ledgerful mcp install --platform copilot --scope project --dry-run --json

# Presence only (no mutation; file presence ≠ host connected)
ledgerful mcp status --json

# Remove only the ledgerful entry (idempotent if absent)
ledgerful mcp uninstall --platform cursor
```

Launcher modes (`--launcher auto|path|npx`, default `auto`):

- **path:** absolute `ledgerful` (or `ledgerful.exe`) + args `["mcp"]` only
- **npx:** `npx -y @ledgerful/mcp-server` (Windows prefers `npx.cmd`)
- **auto:** path if on PATH, else npx with a warning that published npm may lag the engine

Install is **merge-only** (preserves other MCP servers), writes a sibling `.bak`
by default, and uses atomic temp+rename. It does **not** shell out to
`claude mcp add` / `codex mcp add` / `code --add-mcp`.

**Written ≠ connected.** After install, reload the host. Codex project scope may
need interactive project trust; Claude Code / VS Code may prompt to approve the
server. Status reports config-file presence only.

Bare `ledgerful mcp` (or `ledgerful mcp serve`) still starts the stdio server.

### Manual / npm (optional)

```bash
# PATH binary (default build includes mcp)
ledgerful mcp

# npm wrapper (downloads pinned engine; pin may lag)
npx @ledgerful/mcp-server
```

Copilot / VS Code gallery MCP installs share the same workspace
`.vscode/mcp.json` shape (`servers`) — they coexist; install only upserts the
`ledgerful` entry.

Advanced Claude Code `.mcp.json` may expand `${VAR}` in command fields; install
still writes concrete command/args and never writes secrets or embedding URLs.

## Tools

1. `change_context`: Budgeted agent change packet (impact risk, capped `readSet`, doctor readiness, pending ledger). Prefer after `doctor --json`.
2. `scan`: Run impact scan on current repo.
3. `search`: BM25/regex code search. Tool text is a **single JSON object**
   (`schemaVersion` 1, `results[]`) from CLI `search --json` — not NDJSON lines.
4. `ask`: Semantic Q&A with context assembly (MCP children default to zero cloud egress).
5. `ledger_status`: Current pending/unaudited state.
6. `ledger_search`: Full-text search transactions.
7. `hotspots`: Current hotspot rankings.
8. `endpoints_changed`: API endpoints affected by current diff.
9. `security_boundaries`: Security policy graph summary.
10. `dead_code`: Confidence-ranked dead code candidates in the repo.
11. `verify_plan`: Predicted test list for the current diff, without running tests.

## Known Limitations

- No streaming.
- No **product-code** mutations. `search` may **create or write** local
  `.ledgerful` index state (cache) via `--auto-index` (0134); multi-second
  possible. Large/cold repos may prefer explicit `ledgerful index` before MCP
  search (120s spawn ceiling).

The MCP tool set is a **subset** of the full CLI (no `doctor`, `gate mode`, `config view`, etc. as
MCP tools). Prefer the CLI for those; use MCP when the host only exposes MCP.

## Runtime discovery

```bash
ledgerful mcp --help
ledgerful mcp install --help
# MCP itself is stdio JSON-RPC; list tools via your host's tool-list UI after connect
```

Default builds include the `mcp` feature. Source builds without `--features mcp` (or without
defaults) will not expose the `mcp` subcommand.

## Troubleshooting

- **agent can't find ledgerful on PATH**: Install from
  [`docs/installation.md`](../../../../docs/installation.md) or use
  `npx @ledgerful/mcp-server` / `cargo run --features mcp -- mcp`. Prefer
  `ledgerful mcp install --launcher path` once the binary is on PATH.
  (Tracked portable copy: `docs/Ledgerful/references/mcp.md`.)
- **MCP feature missing**: Rebuild with default features or explicit `--features mcp`.
- **tools not live after install**: Config was written; reload/approve in the host
  (written ≠ connected).
