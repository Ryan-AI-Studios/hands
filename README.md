# Hands

Windows **eyes-and-hands** MCP/CLI for [Helping Hands](https://github.com/Ryan-AI-Studios/hands).

A harness (Grok Build, Codex, Claude Code, OpenCode) uses this process to **see** this PC’s desktop and **move** the real mouse/keyboard on **daily Chrome** — no Playwright, no CDP, no `--remote-debugging-port`.

This directory is the **product git root**. Planning, ADRs, and conductor tracks live one level up at `C:\dev\Helping-Hands\` and are **not** part of this repository.

**Sideload is you.** The binary never clicks Developer Mode, never writes HKCU, never edits `C:\LLM`, and never starts `router.bat`.

---

## Clone

```text
https://github.com/Ryan-AI-Studios/hands
```

On this PC the checkout is:

```text
C:\dev\Helping-Hands\hands
```

If you clone somewhere else, substitute that path everywhere below. Keep using **PowerShell** for the commands that contain `$env:` or `$PWD`.

---

## Install on this Windows PC (full bring-up)

Do these in order. Skipping “reload after `REG ADD`” is how the first live install stayed `chrome_connected: false`.

### What you are installing (two processes)

| Process | Who starts it | Command line | Role |
|---------|---------------|--------------|------|
| Native host | **Chrome** (after sideload + HKCU) | `hands.exe` + `chrome-extension://fdnpjnnnmfhlpgaabjflhjoepmejcnha/` | Speaks Chrome native messaging; serves `\\.\pipe\hands-chrome` |
| MCP / CLI | **You** or the harness | `hands.exe mcp` or `hands.exe observe` / `click` / … | Tools. MCP already installs the desk lease; CLI input commands install it for that process |

They must be the **same built exe** (prefer `target\release\hands.exe`). The committed file `native-host\com.helpinghands.host.json` is a **template** (`path` is the placeholder `"hands.exe"`). **Do not** overwrite that template with a machine path.

### 0. Prerequisites

| Need | Detail |
|------|--------|
| OS | Windows, this login. CI cannot sideload. |
| Daily Chrome | `chrome.exe` with your real profile. No automation flags. Do not kill other tabs to “clean up.” |
| Rust | This repo pins **1.97.1** via `rust-toolchain.toml`. Do **not** `rustup default` to another channel. First `cargo` in this dir installs the pin. |
| PowerShell | Use it for `$env:LOCALAPPDATA` and `$PWD`. `cmd.exe` will **not** expand `$env:…`. |
| Unset fixture | Live demo: `HANDS_CHROME_SNAPSHOT` must be **unset** (that env is a test host-double). |
| Optional Gemma | File `C:\LLM\models\mmproj-gemma-4-E4B-it-Q8_0.gguf` (ggml-org, **not** Unsloth) + router `--mmproj`. Not a Hands compile gate. |
| Optional `do_task` | `HANDS_XAI_API_KEY` or `XAI_API_KEY`. Missing key is a tool error, not a build failure. |

Forbidden: Playwright, Puppeteer, CDP, `--remote-debugging-port`, `--enable-automation`, CAPTCHA solvers, HKLM, Chrome Web Store publish, committing filled host JSON or harness configs into this repo.

### 1. Build the release exe

```powershell
cd C:\dev\Helping-Hands\hands
cargo build --release
# expect: C:\dev\Helping-Hands\hands\target\release\hands.exe
```

Sanity:

```powershell
.\target\release\hands.exe --help
.\target\release\hands.exe native-host-manifest --help
```

### 2. Print a filled native-host manifest (do not save over the template)

```powershell
cd C:\dev\Helping-Hands\hands
.\target\release\hands.exe native-host-manifest --exe "$PWD\target\release\hands.exe"
```

You should see JSON like:

```json
{
  "allowed_origins": [
    "chrome-extension://fdnpjnnnmfhlpgaabjflhjoepmejcnha/"
  ],
  "description": "Helping Hands native messaging host",
  "name": "com.helpinghands.host",
  "path": "C:\\dev\\Helping-Hands\\hands\\target\\release\\hands.exe",
  "type": "stdio"
}
```

Checks before you save it:

- `path` is an **absolute** existing `hands.exe` (double backslashes in JSON are correct).
- `allowed_origins` is exactly `chrome-extension://fdnpjnnnmfhlpgaabjflhjoepmejcnha/` (trailing slash).
- The file you write is **only** that JSON. No PowerShell after the closing `}`.

### 3. Save the filled JSON under LocalAppData

**Do not** edit `native-host\com.helpinghands.host.json` in git.

```powershell
$mf = Join-Path $env:LOCALAPPDATA "hands\com.helpinghands.host.json"
New-Item -ItemType Directory -Force -Path (Split-Path $mf) | Out-Null
$json = @'
{
  "name": "com.helpinghands.host",
  "description": "Helping Hands native messaging host",
  "path": "C:\\dev\\Helping-Hands\\hands\\target\\release\\hands.exe",
  "type": "stdio",
  "allowed_origins": [
    "chrome-extension://fdnpjnnnmfhlpgaabjflhjoepmejcnha/"
  ]
}
'@
# UTF-8 without BOM. Windows PowerShell 5 `Set-Content -Encoding utf8` writes a BOM — avoid it.
[System.IO.File]::WriteAllText($mf, $json.Trim() + "`n")
Get-Content -LiteralPath $mf -Raw
```

If you cloned elsewhere, change `path` to that `hands.exe`.

### 4. Sideload the unpacked extension (on the Chrome profile you actually use)

1. Open **the Chrome profile you browse with** (on this PC: Default / `rbourgoin@gmail.com`). Sideloading on another profile does nothing for daily Chrome.
2. Go to `chrome://extensions`.
3. Turn **Developer mode** on (top right).
4. **Load unpacked** → folder:

   ```text
   C:\dev\Helping-Hands\hands\extension
   ```

5. Confirm the card **Helping Hands** shows id **`fdnpjnnnmfhlpgaabjflhjoepmejcnha`**.

If the id differs, stop. `allowed_origins` will reject the host (`Access to the specified native messaging host is forbidden`). The committed `"key"` in `extension\manifest.json` is what pins that id.

### 5. Register the native host (HKCU only)

**PowerShell** (so `$env:LOCALAPPDATA` expands):

```powershell
$mf = Join-Path $env:LOCALAPPDATA "hands\com.helpinghands.host.json"
# confirm this is a real file, not a string containing '$env:'
Test-Path -LiteralPath $mf
REG ADD "HKCU\Software\Google\Chrome\NativeMessagingHosts\com.helpinghands.host" /ve /t REG_SZ /d $mf /f
```

**Verify the registry value is the expanded path:**

```powershell
Get-ItemProperty "HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.helpinghands.host" |
  Select-Object -ExpandProperty '(default)'
# must print e.g. C:\Users\<you>\AppData\Local\hands\com.helpinghands.host.json
# must NOT print $env:LOCALAPPDATA\hands\...
```

Do **not** use HKLM. Do **not** point the registry at the committed template in the repo.

### 6. Restart Chrome or reload the extension

Chrome caches the native-host list. After `REG ADD`:

- **Restart that Chrome** (close the window, open Chrome again), **or**
- On `chrome://extensions`, click **Reload** on Helping Hands.

The service worker should leave **Inactive** and Chrome should spawn:

```text
...\target\release\hands.exe chrome-extension://fdnpjnnnmfhlpgaabjflhjoepmejcnha/ --parent-window=0
```

`--parent-window=0` is normal (service worker). Hands ignores it.

Do not add automation flags. Do not kill other tabs as cleanup.

### 7. Prove the host (CLI)

```powershell
cd C:\dev\Helping-Hands\hands
if ($env:HANDS_CHROME_SNAPSHOT) { Remove-Item Env:HANDS_CHROME_SNAPSHOT }

.\target\release\hands.exe attach --plan
# attached:true, launched:false if daily Chrome is already up

.\target\release\hands.exe attach
.\target\release\hands.exe observe
```

Success: JSON has `"chrome_connected": true` and at least one `"id": "chr:…"`. Open a normal `https://` tab (not `chrome://extensions`) — content scripts do not run on `chrome://` pages.

| Symptom | Fix |
|---------|-----|
| first observe `chrome_connected: false` / empty Chrome list | `hands native-host-doctor` (MCP: `native_host_doctor`). Read-only; does not write HKCU. |
| `chrome_connected: false`, no `hands.exe` with `chrome-extension://` | Reload the extension after a good `REG ADD`. Confirm you sideloaded on **this** profile. |
| Specified native messaging host not found / not registered | HKCU default must be the **full path to the JSON file**. Restart Chrome. |
| Access … forbidden | Extension id ≠ `fdnpjnnnmfhlpgaabjflhjoepmejcnha`, or `allowed_origins` typo. |
| Native host has exited / Error when communicating | Host stderr + Chrome native-messaging log. JSON must be valid (no extra PowerShell). |
| Observe hangs ~2 s with a connected host | Large-frame pipe stall (known leftover). Stop and report; do not add CDP. |

### 8. Register Hands as user-scope MCP

Same release exe. **Do not** commit `.mcp.json`, `.grok/config.toml`, or `.codex/config.toml` into `hands\`.

Re-check flags before you copy (they drift):

```powershell
grok mcp add --help    # expect --scope, default user
claude mcp add --help  # expect --scope, default local — you MUST pass user
codex mcp add --help   # expect NO --scope (writes ~/.codex/config.toml)
```

**Grok Build**

```powershell
grok mcp add --scope user hands -- C:\dev\Helping-Hands\hands\target\release\hands.exe mcp
grok mcp list
grok mcp doctor hands
```

In the TUI: `/mcps` → enable `hands`. Tools are namespaced `hands__observe`, etc. Optional in `~/.grok/config.toml`:

```toml
[mcp_servers.hands]
command = "C:\\dev\\Helping-Hands\\hands\\target\\release\\hands.exe"
args = ["mcp"]
startup_timeout_sec = 30
tool_timeout_sec = 180
```

Grok’s default `tool_timeout_sec` is already large (~6000). Raise it if you lowered it.

**Claude Code** (default scope is **local** — you must pass `user`)

```powershell
claude mcp add --scope user hands -- C:\dev\Helping-Hands\hands\target\release\hands.exe mcp
claude mcp list
```

Expect `hands: … √ Connected`. Session `/mcp` → `observe`.

**Codex** (no `--scope` flag)

```powershell
codex mcp add hands -- C:\dev\Helping-Hands\hands\target\release\hands.exe mcp
```

Then edit `~/.codex/config.toml` (docs defaults are ~10 s startup / **60 s** tools — too short for `observe` / `do_task`):

```toml
[mcp_servers.hands]
command = "C:\\dev\\Helping-Hands\\hands\\target\\release\\hands.exe"
args = ["mcp"]
startup_timeout_sec = 30
tool_timeout_sec = 180
```

```powershell
codex mcp list
```

**OpenCode** — edit `%USERPROFILE%\.config\opencode\opencode.json` or `opencode.jsonc`. `command` is an **array**, not `args`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "hands": {
      "type": "local",
      "command": ["C:\\dev\\Helping-Hands\\hands\\target\\release\\hands.exe", "mcp"],
      "enabled": true
    }
  }
}
```

Restart OpenCode, then `opencode mcp list` → `✓ hands connected`.

Grok always-approve is **not** an inner confirm. Wiring MCP does not grant Easy Apply. The fence stays in this binary.

### 9. Optional: local Gemma (pick / ground)

Not required to compile or to click.

1. Official projector only: [mmproj-gemma-4-E4B-it-Q8_0.gguf](https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/blob/main/mmproj-gemma-4-E4B-it-Q8_0.gguf) (not Unsloth).
2. On this PC it already lives at `C:\LLM\models\mmproj-gemma-4-E4B-it-Q8_0.gguf` and `C:\LLM\router` already passes `--mmproj`. Do not edit the router from a Hands track.
3. Start the router only if you want a live crop (`http://127.0.0.1:8081`). **8081 down is a tool error.**

### 10. Optional: `do_task`

```powershell
# PowerShell
$env:HANDS_XAI_API_KEY = "<key>"   # or XAI_API_KEY
.\target\release\hands.exe do-task --goal "find a Camry on cars.com"
```

Fence / yield still hard-stop the loop. Missing key → skip, not a failed install.

### 11. First-use smoke (primary monitor)

```powershell
.\target\release\hands.exe attach
# in daily Chrome, open https://www.cars.com
.\target\release\hands.exe observe
# click the search box via a chr: id AND via the matching grid cell (hittable, not pixel-perfect)
.\target\release\hands.exe click --element-id chr:<n>
.\target\release\hands.exe click --grid g:<col>:<row>
# Notepad: observe, click a uia: edit, type a short harmless string
# During a live hover or type, press Pause/Break — injection stops; session allows wipe; logs stay
```

Gray-zone **free**: cookie Accept, dismiss sign-in / Not now. **Do not** click dealer **Check Availability** unless you mean to confirm a lead.

`hands scroll --dy -6` scrolls toward the user (page-down). `--dy=-6` is also valid.

### Rollback (does not kill Chrome)

```powershell
REG DELETE "HKCU\Software\Google\Chrome\NativeMessagingHosts\com.helpinghands.host" /f
# chrome://extensions → Remove Helping Hands
# grok mcp remove / claude mcp remove / codex equivalent; delete OpenCode mcp.hands
```

Leave daily Chrome running.

---

## Developer tools (this directory only)

```powershell
cd C:\dev\Helping-Hands\hands
ai-brains preflight --summary
ledgerful doctor --json
# workRoot/stateDir must be this directory, not C:\dev\Helping-Hands
```

`ai-brains context` / `ledgerful init` already ran here. Re-run them only if `.env` / `.ledgerful` are missing. Never init in the planning root.

## Build / test

```powershell
cd C:\dev\Helping-Hands\hands
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Pinned toolchain: **1.97.1** (`rust-toolchain.toml`). Do not jump channels for this crate.

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
cargo run -- challenge --help
cargo run -- do-task --help
cargo run -- logs --help
cargo run -- native-host --help
cargo run -- native-host-manifest --help
```

## CLI / MCP contract (short)

`hands mcp` serves stdio MCP (`observe`, `click`, `hover`, `type`, `key`, `scroll`, `wait_settle`, `stop`, `confirm`, `attach`, `pick`, `ground`, `challenge`, `do_task`, `logs`).

`hands observe [--detail dom] [--session-id <id>]` prints a compact observe envelope. Default observe is the foreground window (≤20 elements, ≤4 KiB envelope); sidecar / `detail=dom` hold the rest. Screenshot is still the virtual-screen **path**. `chr:` ids appear only when Chrome is the foreground window; `chrome_connected` remains an honest host-up bit. `extract.dialogs` leads when a cookie / account / dialog is visible; those ids stay clickable via `click --element-id`. Cards may include miles/dealer/distance; `extract.empty_state` holds empty-radius copy. Default-map elements carry `grid` (`g:col:row` of the resolved center); prefer that over guessing. Image bytes are never inlined. **`observe` does not launch Chrome.** **`observe` does not call Gemma.**

`hands pick` / `hands ground` call local Gemma at `http://127.0.0.1:8081` (`HANDS_GEMMA_URL`, loopback http only). `HANDS_GEMMA_TIMEOUT_MS` (default 90000, min 5000), `HANDS_GEMMA_FORCE_TEXT` (`1`/`true`/`yes`) skips images, `HANDS_GEMMA_API_KEY` optional Bearer (never logged). **8081 down is a tool error.** `pick` always sends a text list. `ground` sends a PNG crop only when `/v1/models` reports multimodal. These do **not** install the desk lease.

`hands challenge [--status] [--watch] [--observe-path <path>] [--session-id <id>]` reports the in-process challenge episode. A visible “are you human” UI can be tried as computer-use for **two observe-cycles that used actuation**. After that, actuation refuses (`yielded`) with **no SendInput**. Resume only when the UI is gone. Idle is not resume. Not a solver.

`hands do-task --goal <text> [--model <id>] [--max-steps N] [--session-id <id>]` is an optional **client of those primitives**. Default model `grok-4.6` via `POST https://api.x.ai/v1/responses` (`HANDS_XAI_API_KEY` then `XAI_API_KEY`). Missing key is a tool error. Fence refuse or yield **stops** the loop. CLI **does** install the desk lease.

`hands attach [--plan] [--session-id <id>]` attaches to a visible `Chrome_WidgetWin_1` whose image is `chrome.exe`, or launches `chrome.exe about:blank` with **zero `--` flags**. `--plan` never spawns. `HANDS_CHROME_EXE` overrides the exe (set + missing file is a hard error). Attach does not sideload, does not kill Chrome, and does not install the desk lease.

Chrome artifacts: `extension/` (unpacked MV3, isolated world, id `fdnpjnnnmfhlpgaabjflhjoepmejcnha`) and `native-host/` (`com.helpinghands.host`). MCP/CLI talk to the host over `\\.\pipe\hands-chrome` (`HANDS_CHROME_PIPE`). Tests may set `HANDS_CHROME_SNAPSHOT` (host-double). `chr:` ids are a walk index (`chr:0`, `chr:42` — no leading zeros).

This binary owns the confirm fence. `click` and `key enter`/`return` refuse irreversible/gray-zone controls unless a matching domain+category allow exists. `type` containing a newline is a tool error — use `key enter` to submit. After a refuse, call `confirm` (`once` / `session` / `persist`) and retry. Grok is always-approve; the TUI is not the fence.

Input commands (`click` / `hover` / `type` / `key` / `scroll` / `wait-settle`) install a desk lease for that process: physical mouse/keyboard freezes injection; Pause/Break always aborts and wipes session/once allows (persist stays). **Logs stay** under `%LOCALAPPDATA%\hands\logs\` (`HANDS_LOGS_DIR`). `hands confirm`, `attach`, `pick`, `ground`, `challenge`, and `logs` do **not** install the lease. `hands do-task` **does**. CLI `stop` posts a desk-wide request (`%LOCALAPPDATA%\hands\stop-request.json`, override `HANDS_STOP_REQUEST_PATH`); another Hands `type` / `hover` honors it as Stop, session allows wipe, and logs stay. Pause/Break still works during a live command.

## What this is

Product intent lives in the planning tree: `C:\dev\Helping-Hands\SHARED-UNDERSTANDING.md`.
