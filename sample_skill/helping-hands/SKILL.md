---
name: helping-hands
description: >
  Live-drive Helping Hands MCP/CLI on this Windows desktop (observe, click, type, scroll).
  Chrome fusion: chrome_connected, chr: ids, native-host pipe, extension service-worker
  reload when Inactive. Use when using Hands live, chrome_connected is false, chr: ids
  are missing, or the user mentions the Chrome extension, native host, sideload, fused
  observe, or /helping-hands. Install is the Hands repo README, not this file.
---

# Helping Hands — live Chrome fusion

First-time install (HKCU, filled JSON, Load unpacked) lives in the Hands repo
**`README.md`** (“Install on this Windows PC”). Do not copy that here. This skill
is the **live** loop: get `chrome_connected: true` and use `chr:` on a real https
tab.

Product cwd: the Hands git root (the directory that contains `Cargo.toml` and
`extension/`). `attach` does not sideload. `observe` does not launch Chrome. No
Playwright, CDP, or `--remote-debugging-port`.

## Two processes

| Process | Who starts it | How you know it is up |
|---------|---------------|------------------------|
| Native host | **Chrome** (after sideload + HKCU) | `hands.exe` + `chrome-extension://fdnpjnnnmfhlpgaabjflhjoepmejcnha/` (`--parent-window=0` is normal) |
| MCP / CLI | Harness | `hands.exe mcp` or CLI tools |

They must be the **same built exe**. Live: `HANDS_CHROME_SNAPSHOT` **unset**.
Pipe: `\\.\pipe\hands-chrome` (`HANDS_CHROME_PIPE`). Extension id
`fdnpjnnnmfhlpgaabjflhjoepmejcnha`. Host name `com.helpinghands.host`.

`chrome_connected` is **snapshot-ok** (pipe + service worker + a tab the content
script can answer), not merely “named pipe exists”. Doctor splits those: **pipe**
vs **snapshot**. `chr:<u32>` appears only when **Chrome is the foreground window**
and the page is a normal `https://` tab. Content scripts do **not** run on
`chrome://` (including `chrome://extensions`). Prefer `chr:` for page content;
Chrome UIA churns.

## What the extension is (and is not)

Isolated-world DOM walk + native-host pipe. It does **not** click, type, or hide
automation. Clicks on `chr:` still go through OS `SendInput` at the resolved rect.

| Fusion up | Fusion down |
|-----------|-------------|
| `extract.url` / `title` / capped `main_text` | URL often missing; UIA scraps + screenshot |
| `chr:` ids (page-local walk index) | `uia:` / pixel / grid |
| listing `cards` (title, price, miles, href) when the page looks like results | guess from screenshot |

`chr:` dies on navigation (an insert-before can shift later indexes) — re-observe.
`navigator.plugins` will not list Helping Hands. Fusion is not stealth.

Off-screen `chr:` (rect `y` far below the Chrome client, or a huge height whose
center is off the virtual screen) will fail click. Pick an on-screen id, or
scroll first. Do not pass `detail=dom` to “see more”; the sidecar already has it.

## Before driving a page

1. `attach` (plan: true is dry-run). Daily Chrome, no automation flags.
2. `observe`. If `chrome_connected: true` and you see `chr:` ids, use them.
3. If `chrome_connected: false` or no `chr:` on an https FG Chrome tab, diagnose
   — do not keep clicking `uia:` / pixels and call fusion used.

Doctor (read-only; does not write HKCU). MCP: `native_host_doctor`. If this MCP
build lacks that tool, run it from the **same exe Chrome will spawn** (`path` in
`%LOCALAPPDATA%\hands\com.helpinghands.host.json`). Current tree has the
subcommand; an older release exe may not.

```powershell
cd <hands-git-root>
.\target\release\hands.exe native-host-doctor
# if that exe has no such subcommand: cargo run -- native-host-doctor
```

JSON + HKCU green with **pipe down** means the host is not connected — almost
always the extension service worker, not a missing registry key.

## Diagnose before more clicks

Install / HKCU / JSON path: README “Prove the host” / symptom table. This table
is the **live** miss.

| You see | Meaning | Do |
|---------|---------|----|
| Observe on `chrome://extensions` (or Errors): `chrome_connected: false`, empty `cards` / `url` | Content script never runs here. Doctor snapshot fail is often `{error:"no-content"}` in ~40 ms, not a dead pipe. | Leave `chrome://`. Open `https://`. Chrome FG. Re-observe. |
| Inspect views **`service worker (Inactive)`** | Chrome dropped native messaging. | Card **Reload** (not the toolbar). Then https + observe. |
| Doctor **pipe up + snapshot failed within 400 ms** on an **https** FG tab | Usually a leftover `hands.exe chrome-extension://…` holding `FILE_FLAG_FIRST_PIPE_INSTANCE` after the worker died (zombie). A PowerShell named-pipe snapshot that returns 0 bytes in ~20 ms is the same. Current host **exits when Chrome stdin closes**. | Kill only those `chrome-extension://` `hands.exe` (keep MCP). Then card Reload. Confirm a **new** PID. Then https. |
| Errors: `Unchecked runtime.lastError: Native host has exited` (`:0` anonymous) | `onDisconnect` did not read `chrome.runtime.lastError` (or leftover chip from an older worker). Fusion can still be up. | Current `sw.js` reads lastError. Card Reload. Opening Errors may still show the old line. |
| `cargo build --release` Access denied while MCP is running | MCP has `target\release\hands.exe` open. | Disable Hands MCP (do not uninstall), rebuild, re-enable; **or** `cargo build --release --target-dir target\rel-live`, point `%LOCALAPPDATA%\hands\com.helpinghands.host.json` at that exe (`native-host-manifest --exe`), card Reload. Same protocol; prefer one exe again after MCP is free. |
| Snapshot `{error:"no-tab"}` with DevTools focused | SW `tabs.query` lastFocusedWindow was the inspector. | Focus the https Chrome window, not DevTools / the TUI. |

## Inactive service worker (usual live miss)

Sideloaded + enabled is not enough. Inspect views **`service worker (Inactive)`**
means Chrome dropped native messaging.

1. Open `chrome://extensions` on the **profile they browse with**.
2. Developer mode on. Card **Helping Hands**, id
   **`fdnpjnnnmfhlpgaabjflhjoepmejcnha`**. Wrong id → host origin reject.
3. If doctor is pipe-up / snapshot-fail on https, kill leftover
   `chrome-extension://` `hands.exe` **before** Reload (see table).
4. Click **Reload on the Helping Hands card** (not the Chrome toolbar Reload).
5. Confirm a process:
   `hands.exe chrome-extension://fdnpjnnnmfhlpgaabjflhjoepmejcnha/ --parent-window=0`
6. Leave `chrome://`. Open a **https** tab. Chrome must be foreground.
7. `observe` again. Success: `"chrome_connected": true` and at least one
   `"id": "chr:…"`. `extract.url` fills when fusion is up. Listing pages should
   grow `extract.cards` (title / price / href) — that is the extension helping.
8. Prove with a **safe** `click` on a `chr:` id (gallery / in-page nav). Do not
   use Get Started, Get Your Terms, Save, checkout, or other confirm-gated
   targets as the proof click.

If the card is missing, follow README sideload (Load unpacked →
`<hands-git-root>\extension`). Product code never clicks Developer mode or
writes HKCU; a harness may, when the user asked to use the extension.

## After Chrome restart

Chrome caches the native-host list. If fusion dies after restart, reload the
Helping Hands card again, then https + observe. Symptom table: README.

## Observe discipline (tokens / speed)

Default observe is the FG window: **≤20 on-screen hittable elements, ≤4 KiB**.
Sidecar at `observe_path` holds the rest. `detail=dom` is the fat 16 KiB walk —
do **not** pass it unless default extract + the 20 ids cannot answer the step.

- Do not re-observe after every tiny actuate. Click, then observe when the page
  should have changed.
- Prefer `extract` / `cards` / on-screen `chr:` over reading the PNG. Open the
  screenshot only when text is not enough (layout, photo, occluded control).
- `wait_settle` the Chrome client ROI, not the whole desktop. `key` `ctrl+l` is
  Control+L (omnibox). If that misses, click **Address and search bar** then
  `ctrl+a` + type.
- If a default envelope still lists >>20 elements, the connected MCP is an older
  `hands.exe` than this tree. Still do not request `detail=dom`.

## Other live constraints

- Physical mouse / Pause-Break freezes the desk lease. Wait ~2 s idle, then
  retry; `wait_settle` does not rearm by itself.
- Cookie Accept, location/ZIP, dismiss sign-in: gray zone. Confirm fence: Save,
  Follow, Easy Apply, dealer lead forms, checkout. No auto-confirm.
- Challenge UI: two observe-cycles that used actuation, then yield. Not a solver.
  A marketing mock CAPTCHA in page copy is not a challenge.
