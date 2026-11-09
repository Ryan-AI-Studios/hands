# Hittest fixtures and goldens

The HTML pages are static, local, no network. `replies/*.json` are the expected
`content.js` assessment / `at_point` shapes. They are **contract-shape fixtures**, not a live
DOM test: CI cannot execute extension JS.

## Provenance

- **Live-derived (pass-3 headless Chromium run on this repo's `content.js`, 1200x600):**
  `cookie-banner`, `modal-backdrop`, `small-target`, `open-shadow-overlay`, `closed-shadow`,
  `aria-disabled-inert` notes, `pointer-events-none`, `fixed-in-overflow`,
  `absolute-above-clipper`, `body-overflow`, `light-dom-custom-element`, `nested-scroll` verdict.
- **Live-field corrections from that run:** `aria-disabled-inert` confidence `medium`;
  `clipped-container` target `(101,41,200,40)` / centre `(201,61)` / clipped name `Sign in` /
  clipped rect `(100,40,122,62)` / blocker `html`; `transformed-ancestor` target
  `(120,60,200,40)` / centre `(220,80)`; `nested-scroll` blocker `html` / clipped name `Sign in`;
  `point-topmost` confidence `medium` (non-empty `shadow_path`); `iframe-overlay` name from the
  fixture's `aria-label`.
- **Hand-derived (documented geometry + algorithm):** the remaining fields in the above pages and
  the point-mode shapes. `point-topmost` now has `point-topmost.html` (open shadow host with a
  `role=button` button), so its `role` and `shadow_path` are producible.
- **Shape-only point goldens:** `point-body` (blank page), `point-closed-shadow` (closed shadow),
  `point-iframe` (iframe). They assert the spec's unknown branches; they are not claimed as separate
  live runs beyond the pass-3 harness.
- Full-viewport blocker heights are viewport-dependent. Goldens use 1200x800; the live harness used
  1200x600. Only the height field differs; verdict/tag/role/name are stable.

## Compare output

Pass-3 headless compare log:
`C:\Users\RyanB\AppData\Local\Temp\opencode\0128-pass3-live-fixtures.log`.
The gate's summary is in `conductor/0128-DomHitTestOcclusion/review.codex.md`.

## Golden → fixture map

| Golden | Fixture | Expected verdict / key fields |
|---|---|---|
| `unobstructed.json` | `unobstructed.html` | true, high |
| `sticky-header.json` | `sticky-header.html` | false/blocked, `header[role=banner]`, high |
| `modal-backdrop.json` | `modal-backdrop.html` | false/blocked, `div[role=dialog]` “Modal backdrop”, high |
| `cookie-banner.json` | `cookie-banner.html` | false/blocked, `div[role=dialog]` “Cookie banner”, high |
| `pointer-events-none.json` | `pointer-events-none.html` | true, high, no blockers |
| `clipped-container.json` | `clipped-container.html` | true/medium, target `(101,41,200,40)`, clipped_by `section` name `Sign in` rect `(100,40,122,62)`, blocker `html` |
| `transformed-ancestor.json` | `transformed-ancestor.html` | true/high, target `(120,60,200,40)`, centre `(220,80)` |
| `open-shadow-overlay.json` | `open-shadow-overlay.html` | false/blocked, inner `div[role=dialog]` "Shadow overlay", high |
| `closed-shadow.json` | `closed-shadow.html` | false/blocked, inner `div[role=dialog]`, `shadow_pierced_closed`, medium |
| `closed-shadow-uninspectable.json` | (hypothetical no-`chrome.dom`) | false/shadow_closed, kind shadow, low |
| `partial-offscreen.json` | `partial-offscreen.html` | true/medium, onscreen 0.833, visible 0.8 |
| `aria-disabled-inert.json` | `aria-disabled-inert.html` | true/medium, note `aria_disabled` only |
| `nested-scroll.json` | `nested-scroll.html` | false/clipped_out, visible 0.2, blocker `html`, clipped name `Sign in` |
| `iframe-overlay.json` | `iframe-overlay.html` | false/cross_frame, iframe name “Opaque frame” |
| `small-target.json` | `small-target.html` | true/high |
| `fixed-in-overflow.json` | `fixed-in-overflow.html` | true/high |
| `absolute-above-clipper.json` | `absolute-above-clipper.html` | true/high |
| `body-overflow.json` | `body-overflow.html` | true/medium, note `containing_block_skip` |
| `light-dom-custom-element.json` | `light-dom-custom-element.html` | true/high |
| `point-topmost.json` | `point-topmost.html` | point true/medium, role button, shadow_path `app-shell` |
| `point-body.json` | `blank-page.html` | point null/not_interactive |
| `point-closed-shadow.json` | `closed-shadow.html` | point null/shadow_closed (shape-only) |
| `point-iframe.json` | `iframe-overlay.html` | point null/cross_frame (shape-only) |
| `open-shadow-pointer-events-none.json` | `open-shadow-pointer-events-none.html` | point true/high, host is target |
| `resolve-assessed.json` | element-mode resolve shape | selected point from `rectCss` centre |
