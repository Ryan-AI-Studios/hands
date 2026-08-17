# Hands

Windows **eyes-and-hands** MCP/CLI for [Helping Hands](https://github.com/Ryan-AI-Studios/hands).

This directory is the **product git root** (Execution Repo). Planning, ADRs, and conductor tracks live one level up at `C:\dev\Helping-Hands\` and are **not** part of this repository.

## Clone

```text
https://github.com/Ryan-AI-Studios/hands
```

## Tools (this directory only)

```powershell
cd C:\dev\Helping-Hands\hands
ai-brains preflight --summary
ledgerful doctor --json
# workRoot/stateDir must be this directory, not C:\dev\Helping-Hands
```

`ai-brains context` / `ledgerful init` already ran here. Re-run them only if `.env` / `.ledgerful` are missing. Never init in the planning root.

## Build / test

The `hands` crate lives in this directory (Windows-first; `rust-toolchain.toml` pins the toolchain).

```powershell
cd C:\dev\Helping-Hands\hands
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

```powershell
cargo run -- mcp --help
cargo run -- observe --help
cargo run -- click --help
cargo run -- hover --help
cargo run -- type --help
cargo run -- key --help
cargo run -- scroll --help
cargo run -- wait-settle --help
cargo run -- stop --help
cargo run -- confirm --help
cargo run -- attach --help
cargo run -- pick --help
cargo run -- ground --help
cargo run -- logs --help
cargo run -- native-host --help
cargo run -- native-host-manifest --help
```

`hands mcp` serves stdio MCP (`observe`, `click`, `hover`, `type`, `key`, `scroll`, `wait_settle`, `stop`, `confirm`, `attach`, `pick`, `ground`, `logs`). `hands observe [--detail dom] [--session-id <id>]` prints a compact observe envelope (screenshot **path**, 100px grid descriptor, UIA map, Chrome `chr:` ids when the native host is connected, capped extract). Image bytes are never inlined. Envelope field `chrome_connected` is true when a host or `HANDS_CHROME_SNAPSHOT` fixture is present. **`observe` does not launch Chrome.** **`observe` does not call Gemma.**

`hands pick --query <q> [--elements-json <path>] [--observe-path <path>] [--session-id <id>]` asks local Gemma (`http://127.0.0.1:8081`) to choose one allowlisted element id from a text list. `hands ground --query <q> [--observe-path <path>] [--screenshot <path>] [--element-id <id>] [--x --y --w --h] [--session-id <id>]` sends a PNG crop only when `/v1/models` reports multimodal; otherwise it degrades to text pick. **8081 down is a tool error**, never a compile/test gate. Override with `HANDS_GEMMA_URL` (loopback `http` only), `HANDS_GEMMA_TIMEOUT_MS` (default 90000, min 5000), `HANDS_GEMMA_FORCE_TEXT` (`1`/`true`/`yes`, case-insensitive) to skip images, and optional `HANDS_GEMMA_API_KEY` (Bearer; never logged). mmproj install and the live owner demo stay **0011**. `hands pick` / `hands ground` do **not** install the desk lease.

`hands attach [--plan] [--session-id <id>]` attaches to a visible `Chrome_WidgetWin_1` whose image is `chrome.exe`, or launches `chrome.exe about:blank` with **zero `--` flags** (default profile, new tab). `--plan` reports `exe`/`argv` and never calls `CreateProcessW`. Override the exe with `HANDS_CHROME_EXE` (set + missing file is a tool error; no App Paths fallthrough). Attach does not sideload, does not kill Chrome, and does not install the desk lease. Sideload/register remains **0011**.

Chrome map artifacts live in `extension/` (unpacked MV3, isolated world, id `fdnpjnnnmfhlpgaabjflhjoepmejcnha`) and `native-host/` (`com.helpinghands.host`). MCP/CLI talk to the host over `\\.\pipe\hands-chrome` (override `HANDS_CHROME_PIPE`). Tests can set `HANDS_CHROME_SNAPSHOT` to a JSON fixture (host-double; do not also hit the pipe). `chr:` ids are a canonical walk index only (`chr:0`, `chr:42` — no leading zeros, sign, or whitespace). Toolbar/DPR conversion is **approximate**; the 0011 live demo stays on the primary monitor. Owner sideload + native-host register is **0011**. No Playwright, CDP, or `--remote-debugging-port`.

This binary owns the confirm fence. `click` and `key enter`/`return` refuse irreversible/gray-zone controls unless a matching domain+category allow exists (`ok: false` plus a compact `fence` object; no cursor move). `type` containing a newline is a tool error — use `key enter` to submit. After a refuse, the harness must call `confirm` (`once` / `session` / `persist`) and retry. Grok is always-approve, so a TUI prompt is not the fence.

Input commands (`click` / `hover` / `type` / `key` / `scroll` / `wait-settle`) install a desk lease for the duration of the process: physical mouse/keyboard freezes injection; Pause/Break always aborts. Pause/Break and `stop` wipe session/once allows (desk-wide) and leave persist. **Logs stay.** Each tool call, fence refuse, and confirm grant appends one JSONL line under `%LOCALAPPDATA%\hands\logs\<session>.jsonl` (override `HANDS_LOGS_DIR`). `type` persists only `type_meta.len` — never the typed string or clipboard body. Observe logs the screenshot path and `elements_total`, not `main_text` or element texts. `hands logs --session-id <id>` tails events (default 50); `hands logs --list` lists files. Missing `--session-id` without `--list` is a tool error (no mint). `session_id=desk` is reserved for Pause/`stop` desk-wide events (`desk.jsonl` is unbounded; this track does not rotate). `hands confirm`, `hands attach`, `hands pick`, `hands ground`, and `hands logs` do **not** install the desk lease. `hands stop` as a one-shot CLI still clears session allows; injection itself is a documented no-op without a live MCP lease (use MCP `stop`, or Pause/Break during a live command).

## What this is

A harness (Grok Build, Codex, Claude Code, OpenCode) uses this process to **see** the desktop and **move** the real mouse/keyboard on daily Chrome — no Playwright/CDP.

Product intent lives in the planning tree: `C:\dev\Helping-Hands\SHARED-UNDERSTANDING.md`.
