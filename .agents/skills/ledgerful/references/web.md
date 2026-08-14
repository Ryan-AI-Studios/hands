# Ledgerful Web Dashboard

Local-first browser UI for Ledgerful. The `ledgerful web` subcommand starts a
loopback-only axum server that serves a Next.js static export and exposes a
read-only JSON API backed by the local `.ledgerful/` state.

## CLI Surface

```bash
# Start in the foreground on the default port 127.0.0.1:52001
ledgerful web start

# Daemonize (writes PID to .ledgerful/web.pid)
ledgerful web start --background

# Stop a background server
ledgerful web stop

# Check whether a server is running
ledgerful web status

# Common flags
ledgerful web start --port 52001 --host 127.0.0.1
ledgerful web start --allow-public --host 0.0.0.0   # prints a large red warning
ledgerful web start --open                           # open the session URL in the default browser
ledgerful web start --spa-dir /path/to/ledgerful-frontend/out
```

On startup the server prints the full authenticated URL, for example:

```
http://127.0.0.1:52001/?token=a7f3c9...
```

The token is a 32-byte random value hex-encoded to 64 characters. It is never
written to disk, never logged after startup, and regenerated on every start.

## Screens

| Route | What it shows |
|---|---|
| `/` | Dashboard: project risk summary, pending transactions, unaudited drift, recent changes, top hotspots. |
| `/changes` | Change feed from the git working tree with risk badges. |
| `/ledger` | Paginated transaction table with category filters and FTS5 search. |
| `/ledger/detail?txId=...` | Single transaction metadata, files changed, signature, and public key. |
| `/hotspots` | Hotspot rankings and a 90-day trend chart. |
| `/graph` | Knowledge-graph table of nodes and edges (force-directed graph is v1.1). |
| `/projects` | Known local projects with health indicators. |
| `/status` | System health: index ready, graph ready, model reachability, pending/unaudited counts. |
| `/settings` | Redacted read-only configuration view. |

## API Endpoints

All endpoints return JSON. `/api/*` requires the session token via `?token=` or
`Authorization: Bearer <token>`. `/health` is unauthenticated.

| Method | Path | Purpose |
|---|---|---|
| GET | `/health` | Liveness probe: `{"status":"ok"}`. |
| GET | `/api/snapshot` | Compact project snapshot. |
| GET | `/api/changes?days=7` | Working-tree changes. |
| GET | `/api/hotspots?limit=20` | Top hotspots. |
| GET | `/api/hotspots/trend?days=90` | Hotspot score time series. |
| GET | `/api/ledger?limit=50&category=FEATURE` | Recent ledger transactions. |
| GET | `/api/ledger/{tx_id}` | Single transaction. |
| GET | `/api/ledger/search?q=<query>` | Full-text ledger search. |
| GET | `/api/reports/latest-impact.json` | Raw impact report. |
| GET | `/api/reports/latest-verify.json` | Raw verification report. |
| GET | `/api/knowledge-graph?limit=200&focus=changed` | CozoDB subgraph with 60-second in-memory cache. |
| GET | `/api/endpoints/changed` | API endpoints touched by the current diff. |
| GET | `/api/security/boundaries` | Security boundary summary. |
| GET | `/api/status` | System health checks. |
| GET | `/api/projects` | Known projects. |
| GET | `/api/config` | Redacted local config view. |

## Security Model

- **Loopback by default.** The server binds to `127.0.0.1:52001` and refuses to
  bind to a public interface unless `--allow-public` is passed.
- **`--allow-public` warning.** Binding off loopback prints a large red security warning in the
  CLI (token is still required; network exposure is the hazard). Prefer loopback for normal use.
- **Ephemeral session token.** A fresh token is generated on every start and is
  compared in constant time (`subtle::ConstantTimeEq`).
- **Read-only API.** No POST/PUT/DELETE endpoints in v1.
- **Per-IP rate limiting.** Default 60 requests/minute sliding window.
- **`Server` header.** `ledgerful-web/<version>` is added to every response.

## Dev Loop

Dashboard UI source is the separate **`ledgerful-frontend`** repo (sibling checkout is common,
e.g. next to the engine clone). Paths below are examples — substitute your local roots.

```powershell
# Terminal 1: Rust API server (from engine repo root)
cargo run --bin ledgerful -- web start --port 52001 --spa-dir <frontend-repo>/out

# Terminal 2: Next.js dev server with hot reload (from frontend repo root)
npm run dev          # listens on http://localhost:3001 in this project
```

In dev mode, Next.js rewrites `/api/*` to `http://127.0.0.1:52001/api/*`. The
production Rust server serves the built static export from `out/` directly, so
API calls hit the same origin.

Convenience scripts may exist as `scripts/dev_web.ps1` / `scripts/dev_web.sh` in the engine repo
and `scripts/dev.ps1` in the frontend repo.

## Troubleshooting

| Symptom | Fix |
|---|---|
| Port 52001 already in use | `ledgerful web start --port 9002` (or any free port). |
| Server won't start | Run `ledgerful doctor` to verify `.ledgerful/` state, ledger DB, and CozoDB graph. |
| 404 on hard refresh of `/ledger/detail?txId=...` | Make sure `--spa-dir` points to a built Next.js static export (`npm run build` produces `out/`). The Rust fallback serves `index.html` for unknown routes. |
| Next.js dev server conflicts with Sourcebot | Next.js dev runs on port `3001` by default in this repo to avoid the common `3000` conflict. |
| 403 from the API | The token rotated on server restart. Copy the new authenticated URL from the server startup output. |
| Static export is missing styles or JS | Rebuild the frontend with `npm run build` and restart the Rust server. |

## Embedded Bundle Exclude Convention

The `SpaAssets` struct in `src/commands/web/server.rs` uses `#[exclude = "..."]` attributes
(via `rust-embed`'s `include-exclude` feature) to prevent unused files from being compiled
into the binary. This is important because `rust_embed::Embed` bakes **every file** in the
`folder` directory into the binary at build time.

When adding new public-only assets to `ledgerful-frontend/public/` that are not needed by
the dashboard UI, add them to the exclude list in `server.rs` to avoid binary bloat.

Current exclusions (using `**/` glob prefix for subdirectory robustness):
- `**/Banner.png` — unused marketing asset (~906 KB)
- `**/Icon.png` — unused marketing asset (~833 KB)

## Related Documents

- Engine repo: `docs/architecture/web-dev-loop.md` (when present)
- Frontend repo: `docs/Backend-Notes.md` (API contract for the dashboard)
