const TEXT_CAP = 80;
const MAIN_TEXT_CAP = 2000;
const CARD_TITLE_CAP = 80;
const CARD_PRICE_CAP = 24;
const CARD_HREF_CAP = 200;
const CARD_CAP = 8;
const CARD_WALK_CAP = 24;
const CARD_MILES_CAP = 16;
const CARD_DEALER_CAP = 48;
const CARD_DISTANCE_CAP = 40;
const CARD_OF_CAP = 12;
const CARD_KIND_CAP = 12;
const CARD_DELIVERY_CAP = 40;
const RESULT_COUNT_CAP = 24;
const LOCAL_MATCHES_CAP = 24;
const EMPTY_STATE_CAP = 120;
const ZIP_CAP = 10;
const RADIUS_CAP = 16;
const SNAPSHOT_NODE_CAP = 4000;
const PRICE_RE = /\$|€|£|\d[\d,]*\.\d{2}/;

const DEFAULT_SELECTOR = [
  "a[href]",
  "button",
  "input:not([type=hidden])",
  "textarea",
  "select",
  '[role="button"]',
  '[role="link"]',
  '[role="tab"]',
  '[role="menuitem"]',
  '[role="option"]',
  '[role="menuitemcheckbox"]',
  '[role="menuitemradio"]',
  '[role="treeitem"]',
  '[role="checkbox"]',
  '[role="radio"]',
  '[role="textbox"]',
  '[role="combobox"]',
  '[contenteditable=""]',
  '[contenteditable="true"]',
].join(",");

const DOM_SELECTOR =
  DEFAULT_SELECTOR +
  ",[role],summary,label,[tabindex]:not([tabindex=\"-1\"])";

// ---------------------------------------------------------------- snapshot identity
// One stored snapshot record per content-script document. Only an agent-facing observe
// (`store !== false`) replaces it; internal callers ask for a read without retiring refs.

function randomU32() {
  try {
    const buf = new Uint32Array(1);
    crypto.getRandomValues(buf);
    return buf[0] >>> 0;
  } catch (_err) {
    return (Date.now() >>> 0) ^ (Math.floor(Math.random() * 0xffffffff) >>> 0);
  }
}

function hex32(n) {
  return (n >>> 0).toString(16).padStart(8, "0");
}

const DOC_NONCE = (randomU32() & 0xffff).toString(16).padStart(4, "0");
let snapCounter = randomU32();
let lastSnapshot = null;
// A fan-out reply is a candidate until the SW commits it. A late reply is never committed, so
// it cannot retire the current agent-facing snapshot.
const pendingSnapshots = new Map();

function nextSnap() {
  snapCounter = (snapCounter + 1) >>> 0;
  return hex32(snapCounter);
}

// ---------------------------------------------------------------- content revision (0130)
// `rev` is a per-document content revision over the top-frame retained walk projection. It is
// stored with the agent-facing record and read (never advanced) by `changes`. It is not an
// identity: `snap`, `sref1:` refs and `observe_id` keep their owners.
const REV_V = 1;
const CHANGES_MAX_EVENTS = 24;
const CHANGES_MAX_ENTRY_TEXT = 80;
const CHANGES_MAX_BYTES = 4096;
const CHANGES_CTX_URL_CAP = 256;
const CHANGES_CTX_TITLE_CAP = 120;
// Public token: <tab>.<doc>.<rev>.<fp16>.<shape4>. The tab is the poll's routing key; the rest is
// the content-script core. Digits and lowercase hex only, so no page text ever enters a token.
const CHANGES_TOKEN_RE =
  /^([1-9][0-9]{0,9})\.([0-9a-f]{4})\.([1-9][0-9]{0,9})\.([0-9a-f]{16})\.([0-9a-f]{4})$/;
const CHANGES_EXCLUDED = ["disabled_hidden", "closed_shadow", "frame_contents", "canvas_video"];

function fnv1a32Seeded(str, seed) {
  let hash = seed >>> 0;
  for (let i = 0; i < str.length; i += 1) {
    const code = str.charCodeAt(i);
    hash ^= code & 0xff;
    hash = Math.imul(hash, 0x01000193) >>> 0;
    hash ^= (code >>> 8) & 0xff;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

// 64-bit projection digest as two independent 32-bit FNV-1a lanes (16 hex).
function digest64(str) {
  return hex32(fnv1a32Seeded(str, 0x811c9dc5)) + hex32(fnv1a32Seeded(str, 0x9e3779b9));
}

// Exact 4-hex request-shape code: dom bit, light bit, low 14 bits of the node cap.
function revShape(detail, nodeCap, light) {
  const code =
    (detail === "dom" ? 0x8000 : 0) | (light ? 0x4000 : 0) | (Number(nodeCap) & 0x3fff);
  return (code & 0xffff).toString(16).padStart(4, "0");
}

function r2(value) {
  const n = Number(value);
  if (value == null || !Number.isFinite(n)) {
    return null;
  }
  return Math.round(n * 100) / 100;
}

function rectRound(rect) {
  if (!rect) {
    return null;
  }
  return [r2(rect.left), r2(rect.top), r2(rect.width), r2(rect.height)];
}

function viewportProjection() {
  const vv = window.visualViewport;
  return {
    scroll: [
      r2(window.scrollX),
      r2(window.scrollY),
      vv ? r2(vv.offsetLeft) : null,
      vv ? r2(vv.offsetTop) : null,
    ],
    size: [
      r2(window.innerWidth),
      r2(window.innerHeight),
      r2(window.devicePixelRatio),
      vv ? r2(vv.width) : null,
      vv ? r2(vv.height) : null,
    ],
  };
}

// Cards carry a viewport rect; project it in document coordinates so a scroll is one viewport
// entry, not a card change for every card on the page.
function cardsProjection(cards) {
  const sx = Number(window.scrollX) || 0;
  const sy = Number(window.scrollY) || 0;
  return (cards || []).map((card) => {
    const out = {};
    for (const key of Object.keys(card)) {
      if (key !== "rectCss") {
        out[key] = card[key];
      }
    }
    const r = card.rectCss;
    out.docRect = r
      ? [r2(r.left + sx), r2(r.top + sy), r2(r.width), r2(r.height)]
      : null;
    return out;
  });
}

// Canonical projection of the top-frame retained walk. Only fields listed in spec 6.1 enter it:
// never snapshot.frames[] rows, screenX/Y or Rust-side geometry.
function buildProjection(nodes, keys, elements, cards, mainTxt, frameRows) {
  const els = [];
  for (let k = 0; k < elements.length; k += 1) {
    const p = elements[k];
    const node = nodes[p.idx];
    els.push({
      idx: p.idx,
      tag: node ? String(node.tagName || "").toLowerCase() : "",
      role: p.role || "",
      text: p.text == null ? null : p.text,
      href: p.href || "",
      key: keys[p.idx] == null ? null : keys[p.idx],
      rect: rectRound(p.rectCss),
    });
  }
  return {
    url: location.href || "",
    title: document.title || "",
    vp: viewportProjection(),
    main_text: mainTxt || "",
    cards: cardsProjection(cards),
    frames: frameRows || [],
    els: els,
  };
}

function collectElements(nodes) {
  const elements = [];
  const keys = [];
  for (let i = 0; i < nodes.length; i += 1) {
    const payload = keyPayload(nodes[i], i);
    const key = snapshotKey(nodes[i], payload);
    const rect = cssRect(nodes[i]);
    if (rect) {
      payload.rectCss = rect;
      payload.idx = i;
      elements.push(payload);
      keys.push(key);
    } else {
      keys.push(null);
    }
  }
  return { elements: elements, keys: keys };
}

// Exact UTF-8 byte length without TextEncoder, so the byte cap means the same thing in every
// realm (the harness VM and the content script alike).
function utf8Length(text) {
  let bytes = 0;
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i);
    if (code < 0x80) {
      bytes += 1;
    } else if (code < 0x800) {
      bytes += 2;
    } else if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
      bytes += 4;
      i += 1;
    } else {
      bytes += 3;
    }
  }
  return bytes;
}

function capText(value, cap) {
  if (value == null) {
    return null;
  }
  return String(value).slice(0, cap);
}

function rectEqual(a, b) {
  return !!a && !!b && a[0] === b[0] && a[1] === b[1] && a[2] === b[2] && a[3] === b[3];
}

function rectOverlaps(a, b) {
  return a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3];
}

function vpSummary(vp, fields) {
  const out = {};
  if (fields.indexOf("scroll") !== -1) {
    out.scroll = { x: vp.scroll[0], y: vp.scroll[1], vv_left: vp.scroll[2], vv_top: vp.scroll[3] };
  }
  if (fields.indexOf("size") !== -1) {
    out.size = {
      w: vp.size[0],
      h: vp.size[1],
      dpr: vp.size[2],
      vv_w: vp.size[3],
      vv_h: vp.size[4],
    };
  }
  return out;
}

// Two-pass matching over the stored projection and a fresh one. Returns complete counts and,
// in the caller's cap check, the bounded arrays. Pass 1 pairs by snapshotKey (exact rect first
// inside a duplicate-key group, then walk order). Pass 2 pairs leftovers of the same tag whose
// rects overlap, so a visible label change reads as `changed`, not add+remove.
function diffProjections(oldP, newP) {
  const oldEls = oldP.els;
  const newEls = newP.els;
  const oldUsed = new Uint8Array(oldEls.length);
  const newUsed = new Uint8Array(newEls.length);
  const pairs = [];
  const byKey = new Map();
  for (let j = 0; j < oldEls.length; j += 1) {
    const key = oldEls[j].key;
    if (key == null) {
      continue;
    }
    let list = byKey.get(key);
    if (!list) {
      list = [];
      byKey.set(key, list);
    }
    list.push(j);
  }
  // Pass 1a: same key and identical rect.
  for (let i = 0; i < newEls.length; i += 1) {
    const ne = newEls[i];
    const list = ne.key == null ? null : byKey.get(ne.key);
    if (!list) {
      continue;
    }
    for (let t = 0; t < list.length; t += 1) {
      const j = list[t];
      if (!oldUsed[j] && rectEqual(oldEls[j].rect, ne.rect)) {
        oldUsed[j] = 1;
        newUsed[i] = 1;
        pairs.push([i, j]);
        break;
      }
    }
  }
  // Pass 1b: same key, remaining in walk order (a cursor keeps this linear per key).
  const cursor = new Map();
  for (let i = 0; i < newEls.length; i += 1) {
    if (newUsed[i]) {
      continue;
    }
    const key = newEls[i].key;
    const list = key == null ? null : byKey.get(key);
    if (!list) {
      continue;
    }
    let c = cursor.get(key) || 0;
    while (c < list.length && oldUsed[list[c]]) {
      c += 1;
    }
    cursor.set(key, c);
    if (c < list.length) {
      oldUsed[list[c]] = 1;
      newUsed[i] = 1;
      pairs.push([i, list[c]]);
    }
  }
  // Pass 2: leftovers of the same tag with overlapping rects, nearest top-left, ties walk order.
  const oldByTag = new Map();
  for (let j = 0; j < oldEls.length; j += 1) {
    if (oldUsed[j]) {
      continue;
    }
    const tag = oldEls[j].tag;
    let list = oldByTag.get(tag);
    if (!list) {
      list = [];
      oldByTag.set(tag, list);
    }
    list.push(j);
  }
  for (let i = 0; i < newEls.length; i += 1) {
    if (newUsed[i]) {
      continue;
    }
    const ne = newEls[i];
    const list = oldByTag.get(ne.tag) || [];
    let best = -1;
    let bestDistance = Infinity;
    for (let t = 0; t < list.length; t += 1) {
      const j = list[t];
      if (oldUsed[j] || !rectOverlaps(oldEls[j].rect, ne.rect)) {
        continue;
      }
      const distance =
        Math.abs(oldEls[j].rect[0] - ne.rect[0]) + Math.abs(oldEls[j].rect[1] - ne.rect[1]);
      if (distance < bestDistance) {
        bestDistance = distance;
        best = j;
      }
    }
    if (best !== -1) {
      oldUsed[best] = 1;
      newUsed[i] = 1;
      pairs.push([i, best]);
    }
  }

  const added = [];
  for (let i = 0; i < newEls.length; i += 1) {
    if (!newUsed[i]) {
      added.push({
        idx: newEls[i].idx,
        role: newEls[i].role,
        text: capText(newEls[i].text, CHANGES_MAX_ENTRY_TEXT),
      });
    }
  }
  const removed = [];
  for (let j = 0; j < oldEls.length; j += 1) {
    if (!oldUsed[j]) {
      removed.push({
        idx: oldEls[j].idx,
        role: oldEls[j].role,
        text: capText(oldEls[j].text, CHANGES_MAX_ENTRY_TEXT),
      });
    }
  }
  const scrollSame = JSON.stringify(oldP.vp.scroll) === JSON.stringify(newP.vp.scroll);
  const sizeSame = JSON.stringify(oldP.vp.size) === JSON.stringify(newP.vp.size);
  // rect is compared only on a stable viewport and with no add/remove: an insert/remove shifts the
  // map by construction and reporting the shift as N rect changes is the rect-storm class.
  const rectAllowed = scrollSame && sizeSame && added.length === 0 && removed.length === 0;
  const changed = [];
  for (let p = 0; p < pairs.length; p += 1) {
    const ni = pairs[p][0];
    const oj = pairs[p][1];
    const ne = newEls[ni];
    const oe = oldEls[oj];
    const fields = [];
    if (oe.role !== ne.role) fields.push("role");
    if (oe.text !== ne.text) fields.push("text");
    if (oe.href !== ne.href) fields.push("href");
    if (rectAllowed && !rectEqual(oe.rect, ne.rect)) fields.push("rect");
    // Carriers for projection inputs the visible fields do not show, so an fp change always has an
    // entry: `attrs` = an identity attribute (name/type/tag inside snapshotKey) changed with no
    // visible text/role/href change; `order` = a pure reorder (idx moved) with no add/remove to
    // explain the shift. Both are additive to the spec's text|role|href|rect vocabulary.
    if (fields.length === 0 && oe.key !== ne.key) fields.push("attrs");
    if (
      fields.length === 0 &&
      oe.idx !== ne.idx &&
      added.length === 0 &&
      removed.length === 0
    ) {
      fields.push("order");
    }
    if (fields.length) {
      changed.push({
        idx: oe.idx,
        role: ne.role,
        text: capText(ne.text, CHANGES_MAX_ENTRY_TEXT),
        fields: fields,
      });
    }
  }
  const vpFields = [];
  if (!scrollSame) vpFields.push("scroll");
  if (!sizeSame) vpFields.push("size");
  const viewport = vpFields.length
    ? { fields: vpFields, from: vpSummary(oldP.vp, vpFields), to: vpSummary(newP.vp, vpFields) }
    : null;

  const context = { changed: [] };
  if (oldP.url !== newP.url) {
    context.changed.push("url");
    context.url = {
      from: capText(oldP.url, CHANGES_CTX_URL_CAP),
      to: capText(newP.url, CHANGES_CTX_URL_CAP),
    };
  }
  if (oldP.title !== newP.title) {
    context.changed.push("title");
    context.title = {
      from: capText(oldP.title, CHANGES_CTX_TITLE_CAP),
      to: capText(newP.title, CHANGES_CTX_TITLE_CAP),
    };
  }
  if (oldP.main_text !== newP.main_text) context.changed.push("main_text");
  if (JSON.stringify(oldP.cards) !== JSON.stringify(newP.cards)) context.changed.push("cards");
  if (JSON.stringify(oldP.frames) !== JSON.stringify(newP.frames)) context.changed.push("frames");

  const counts = {
    added: added.length,
    removed: removed.length,
    changed: changed.length,
    viewport: viewport ? 1 : 0,
    context: context.changed.length,
  };
  counts.total = counts.added + counts.removed + counts.changed + counts.viewport + counts.context;
  return {
    counts: counts,
    events: added.length + removed.length + changed.length,
    changes: {
      counts: counts,
      added: added,
      removed: removed,
      changed: changed,
      viewport: viewport,
    },
    context: context,
  };
}

function changesCoverage(walked, retained) {
  return {
    scope: "retained_walk",
    page_complete: false,
    elements_walked: walked,
    elements_retained: retained,
    excluded: CHANGES_EXCLUDED.slice(),
  };
}

function changesResync(reply, reason) {
  reply.resync = { required: true, reason: reason };
  return reply;
}

// Read-only poll against the stored baseline record. It never assigns lastSnapshot, never calls
// nextSnap() and never creates a pendingSnapshots entry, so repeated polls repeat the same report.
function buildChanges(msg) {
  const token = String(msg.changes_since || "");
  const detail = msg.detail === "dom" ? "dom" : "default";
  const shape = revShape(detail, SNAPSHOT_NODE_CAP, false);
  const base = lastSnapshot;
  const hasBase = !!(base && base.doc && Number.isInteger(base.rev) && base.proj);
  const reply = {
    rev_v: REV_V,
    rev_core: hasBase ? base.doc + "." + base.rev + "." + base.fp + "." + base.shape : null,
    coverage: changesCoverage(0, 0),
  };
  const m = CHANGES_TOKEN_RE.exec(token);
  if (!m) {
    return changesResync(reply, "changed_request");
  }
  if (!hasBase) {
    return changesResync(reply, "no_baseline");
  }
  if (m[2] !== base.doc) {
    return changesResync(reply, "document_changed");
  }
  if (m[5] !== shape) {
    return changesResync(reply, "changed_request");
  }
  if (m[5] !== base.shape || m[3] !== String(base.rev) || m[4] !== base.fp) {
    return changesResync(reply, "stale_revision");
  }
  const walked = walk(detail).slice(0, SNAPSHOT_NODE_CAP);
  const collected = collectElements(walked);
  const cards = collectCards().cards;
  const fresh = buildProjection(
    walked,
    collected.keys,
    collected.elements,
    cards,
    mainText(),
    frameElementsReply()
  );
  reply.coverage = changesCoverage(walked.length, collected.elements.length);
  if (digest64(JSON.stringify(fresh)) === base.fp) {
    reply.unchanged = true;
    reply.changes = {
      counts: { added: 0, removed: 0, changed: 0, viewport: 0, context: 0, total: 0 },
      added: [],
      removed: [],
      changed: [],
      viewport: null,
    };
    reply.context = { changed: [] };
    return reply;
  }
  const diff = diffProjections(base.proj, fresh);
  const bytes = utf8Length(JSON.stringify({ changes: diff.changes, context: diff.context }));
  if (diff.events > CHANGES_MAX_EVENTS || bytes > CHANGES_MAX_BYTES) {
    // Overflow: complete counts, no arrays, no context from/to. Never a partial list.
    reply.resync = { required: true, reason: "delta_overflow", counts: diff.counts };
    return reply;
  }
  reply.unchanged = false;
  reply.changes = diff.changes;
  reply.context = diff.context;
  return reply;
}

// ---------------------------------------------------------------- frame registry
// Every injected content script (top and child) self-registers over a private port.
// A child never walks or posts until the SW asks it to; frame content is data only.

const FRAME_LINK_CAP = 12;
const FRAME_ELEMENT_CAP = 8;
const FRAME_MEASURE_DEPTH = 64;
let SELF_FRAME = null;
let FRAME_TOKEN = null;
let framePort = null;
let frameReconnectMs = 250;
const knownFrameTokens = new Map();

function connectFramePort() {
  if (!chrome.runtime || typeof chrome.runtime.connect !== "function") {
    return;
  }
  let port = null;
  try {
    port = chrome.runtime.connect({ name: "hands-frame" });
  } catch (_err) {
    scheduleFrameReconnect();
    return;
  }
  if (!port || !port.onMessage || !port.onDisconnect) {
    return;
  }
  framePort = port;
  port.onMessage.addListener((msg) => {
    const op = msg && msg.op;
    if (op === "frame_id") {
      SELF_FRAME = Number.isInteger(msg.frame) ? msg.frame : null;
      FRAME_TOKEN = typeof msg.token === "string" ? msg.token : null;
      frameReconnectMs = 250;
      try {
        port.postMessage({ op: "frame_hello", doc: DOC_NONCE });
      } catch (_err) {}
      return;
    }
    if (op === "frame_announce") {
      announceToParent();
      return;
    }
    if (op === "frame_measure") {
      measureTokens(msg.tokens, msg.point);
    }
  });
  port.onDisconnect.addListener(() => {
    if (framePort === port) {
      framePort = null;
    }
    SELF_FRAME = null;
    FRAME_TOKEN = null;
    scheduleFrameReconnect();
  });
}

function scheduleFrameReconnect() {
  if (frameReconnectMs > 8000) {
    frameReconnectMs = 8000;
  }
  const delay = frameReconnectMs + Math.floor(Math.random() * 250);
  frameReconnectMs = Math.min(frameReconnectMs * 2, 8000);
  setTimeout(connectFramePort, delay);
}

function announceToParent() {
  if (SELF_FRAME == null || SELF_FRAME === 0 || !FRAME_TOKEN) {
    return;
  }
  try {
    window.parent.postMessage({ handsFrameToken: FRAME_TOKEN }, "*");
  } catch (_err) {}
  // Ack only after the postMessage call returned: the SW may then wait for the parent.
  try {
    framePort.postMessage({ op: "frame_announce_ack", announced: true });
  } catch (_err) {}
}

function collectFrameCandidates(node, out) {
  if (!node || node.nodeType !== 1) {
    return;
  }
  const tag = String(node.tagName || "").toUpperCase();
  if (tag === "IFRAME" || tag === "FRAME" || tag === "OBJECT" || tag === "FENCEDFRAME") {
    out.push(node);
  }
  let shadow = node.shadowRoot;
  if (!shadow && chrome.dom && chrome.dom.openOrClosedShadowRoot) {
    try {
      shadow = chrome.dom.openOrClosedShadowRoot(node);
    } catch (_err) {
      shadow = null;
    }
  }
  if (shadow && shadow.children) {
    for (let i = 0; i < shadow.children.length; i += 1) {
      collectFrameCandidates(shadow.children[i], out);
    }
  }
  const kids = node.children;
  if (kids) {
    for (let i = 0; i < kids.length; i += 1) {
      collectFrameCandidates(kids[i], out);
    }
  }
}

function iframeElements() {
  const out = [];
  const root = document.documentElement;
  if (root) {
    collectFrameCandidates(root, out);
  }
  return out;
}

function frameScheme(raw) {
  const value = String(raw || "").trim().toLowerCase();
  const colon = value.indexOf(":");
  if (colon <= 0) {
    return "";
  }
  return value.slice(0, colon);
}

function sourceOrigin(raw) {
  if (!raw || typeof URL !== "function") {
    return null;
  }
  try {
    const url = new URL(raw, location.href);
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      return null;
    }
    return url.origin.toLowerCase();
  } catch (_err) {
    return null;
  }
}

// Internal iframe identity for deduping a registered-but-unlinked child when its slot report
// could not arrive (top port down). The SW compares this against the child's browser-set URL;
// it is never emitted to the agent.
function sourceUrl(raw) {
  if (!raw || typeof URL !== "function") {
    return null;
  }
  try {
    const url = new URL(raw, location.href);
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      return null;
    }
    const href = url.href;
    return href.length <= 512 ? href : null;
  } catch (_err) {
    return null;
  }
}

function classifyFrameElement(el) {
  const tag = String(el.tagName || "").toUpperCase();
  if (tag === "FENCEDFRAME") {
    return { reason: "fenced_frame", origin: null };
  }
  const sandbox = el.getAttribute("sandbox");
  if (sandbox != null) {
    const tokens = String(sandbox).toLowerCase().split(/\s+/);
    if (tokens.indexOf("allow-scripts") === -1) {
      return { reason: "sandboxed_no_scripts", origin: null };
    }
  }
  const raw = String(el.getAttribute("src") || "").trim();
  if (raw) {
    const scheme = frameScheme(raw);
    if (scheme === "about") {
      const lower = raw.toLowerCase();
      if (lower === "about:blank" || lower.indexOf("about:srcdoc") === 0) {
        // Chrome may not inject without match_origin_as_fallback; never claim a scheme refusal.
        return { reason: "no_content_script", origin: null };
      }
      return { reason: "restricted_scheme", origin: null };
    }
    if (
      scheme === "data" ||
      scheme === "blob" ||
      scheme === "filesystem" ||
      scheme === "chrome" ||
      scheme === "chrome-extension" ||
      scheme === "devtools" ||
      scheme === "view-source"
    ) {
      return { reason: "restricted_scheme", origin: null };
    }
  }
  return {
    reason: "no_content_script",
    origin: sourceOrigin(raw),
    src: sourceUrl(raw),
  };
}

function frameElementsReply() {
  const els = iframeElements();
  const out = [];
  for (let i = 0; i < els.length && i < FRAME_LINK_CAP; i += 1) {
    const verdict = classifyFrameElement(els[i]);
    const row = { slot: i, reason: verdict.reason };
    if (verdict.origin) {
      row.origin = verdict.origin;
    }
    if (verdict.src) {
      row.src = verdict.src;
    }
    out.push(row);
  }
  return out;
}

function findFrameElementBySource(source) {
  if (!source) {
    return null;
  }
  const els = iframeElements();
  for (let i = 0; i < els.length; i += 1) {
    let win = null;
    try {
      win = els[i].contentWindow;
    } catch (_err) {
      win = null;
    }
    if (win && win === source) {
      return { el: els[i], slot: i };
    }
  }
  return null;
}

function styleIsTransforming(style) {
  if (!style) {
    return false;
  }
  if (style.transform && style.transform !== "none") return true;
  if (style.scale && style.scale !== "none" && style.scale !== "1") return true;
  if (style.zoom && style.zoom !== "1" && style.zoom !== "normal") return true;
  if (style.filter && style.filter !== "none") return true;
  if (style.perspective && style.perspective !== "none") return true;
  if (style.willChange && /transform|perspective|filter/.test(String(style.willChange))) {
    return true;
  }
  return false;
}

function linkTransformOk(el) {
  let node = el;
  let depth = 0;
  while (node && depth < FRAME_MEASURE_DEPTH) {
    if (styleIsTransforming(window.getComputedStyle(node))) {
      return false;
    }
    node = parentOf(node);
    depth += 1;
  }
  return true;
}

function linkVisible(el) {
  let node = el;
  let depth = 0;
  while (node && depth < FRAME_MEASURE_DEPTH) {
    const style = window.getComputedStyle(node);
    if (style) {
      if (style.display === "none") return false;
      if (style.visibility === "hidden" || style.visibility === "collapse") return false;
      if (style.pointerEvents === "none") return false;
      if (style.opacity !== "" && parseFloat(style.opacity) === 0) return false;
    }
    if (node.hasAttribute) {
      if (node.hasAttribute("inert")) return false;
      if (node.getAttribute("aria-hidden") === "true") return false;
    }
    node = parentOf(node);
    depth += 1;
  }
  return true;
}

function linkClip(el, contentBox) {
  const out = [
    {
      left: contentBox.left,
      top: contentBox.top,
      width: contentBox.width,
      height: contentBox.height,
    },
  ];
  let node = parentOf(el);
  let depth = 0;
  while (node && depth < FRAME_MEASURE_DEPTH && out.length < 8) {
    if (clipsOverflow(node)) {
      const box = paddingBox(node);
      if (box) {
        out.push(box);
      }
    }
    node = parentOf(node);
    depth += 1;
  }
  return out;
}

function deepElementFromPoint(x, y) {
  let el = document.elementFromPoint(x, y);
  let depth = 0;
  while (el && depth < ASSESS_SHADOW_PATH) {
    const root = el.shadowRoot;
    if (!root || typeof root.elementFromPoint !== "function") {
      return el;
    }
    let inner = null;
    try {
      inner = root.elementFromPoint(x, y);
    } catch (_err) {
      inner = null;
    }
    if (!inner || inner === el) {
      return el;
    }
    el = inner;
    depth += 1;
  }
  return el;
}

function frameContentBox(el) {
  const rect = el.getBoundingClientRect();
  const style = window.getComputedStyle(el);
  const bl = style ? parseFloat(style.borderLeftWidth) || 0 : 0;
  const bt = style ? parseFloat(style.borderTopWidth) || 0 : 0;
  const br = style ? parseFloat(style.borderRightWidth) || 0 : 0;
  const bb = style ? parseFloat(style.borderBottomWidth) || 0 : 0;
  const pl = style ? parseFloat(style.paddingLeft) || 0 : 0;
  const pt = style ? parseFloat(style.paddingTop) || 0 : 0;
  const pr = style ? parseFloat(style.paddingRight) || 0 : 0;
  const pb = style ? parseFloat(style.paddingBottom) || 0 : 0;
  return {
    left: rect.left + bl + pl,
    top: rect.top + bt + pt,
    width: Math.max(0, rect.width - bl - br - pl - pr),
    height: Math.max(0, rect.height - bt - bb - pt - pb),
  };
}

function frameSlotOf(el) {
  const els = iframeElements();
  return els.indexOf(el);
}

function measureFrameElement(el, slot, point) {
  const rect = el.getBoundingClientRect();
  if (!rect) {
    return null;
  }
  const style = window.getComputedStyle(el);
  const bl = style ? parseFloat(style.borderLeftWidth) || 0 : 0;
  const bt = style ? parseFloat(style.borderTopWidth) || 0 : 0;
  const br = style ? parseFloat(style.borderRightWidth) || 0 : 0;
  const bb = style ? parseFloat(style.borderBottomWidth) || 0 : 0;
  const pl = style ? parseFloat(style.paddingLeft) || 0 : 0;
  const pt = style ? parseFloat(style.paddingTop) || 0 : 0;
  const pr = style ? parseFloat(style.paddingRight) || 0 : 0;
  const pb = style ? parseFloat(style.paddingBottom) || 0 : 0;
  const contentBox = {
    left: rect.left + bl + pl,
    top: rect.top + bt + pt,
    width: Math.max(0, rect.width - bl - br - pl - pr),
    height: Math.max(0, rect.height - bt - bb - pt - pb),
  };
  let childW = null;
  let childH = null;
  try {
    if (el.contentWindow) {
      childW = Number(el.contentWindow.innerWidth);
      childH = Number(el.contentWindow.innerHeight);
    }
  } catch (_err) {
    childW = null;
    childH = null;
  }
  const onscreen =
    contentBox.left < Number(window.innerWidth) &&
    contentBox.top < Number(window.innerHeight) &&
    contentBox.left + contentBox.width > 0 &&
    contentBox.top + contentBox.height > 0;
  let hitOk = false;
  if (contentBox.width > 0 && contentBox.height > 0) {
    // Test the composed click point when one is known; the box centre is only the fallback
    // for an initial (no-target) announce. A partial parent overlay at the click point must
    // refuse the link even when the frame centre is clear.
    let localX = contentBox.width / 2;
    let localY = contentBox.height / 2;
    if (point && Number.isFinite(point.x) && Number.isFinite(point.y)) {
      localX = point.x;
      localY = point.y;
    }
    const inside =
      localX >= 0 &&
      localY >= 0 &&
      localX < contentBox.width &&
      localY < contentBox.height;
    if (inside) {
      let hit = null;
      try {
        hit = deepElementFromPoint(contentBox.left + localX, contentBox.top + localY);
      } catch (_err) {
        hit = null;
      }
      hitOk = !!hit && composedContains(el, hit);
    }
  }
  return {
    content_left: contentBox.left,
    content_top: contentBox.top,
    content_width: contentBox.width,
    content_height: contentBox.height,
    clip: linkClip(el, contentBox),
    transform_ok: linkTransformOk(el),
    visible: linkVisible(el),
    hit_ok: hitOk,
    viewport_w: Number(window.innerWidth),
    viewport_h: Number(window.innerHeight),
    onscreen: onscreen,
    child_viewport_w: Number.isFinite(childW) ? childW : null,
    child_viewport_h: Number.isFinite(childH) ? childH : null,
    slot: slot,
  };
}

function reportFrameLink(token, origin, point) {
  const known = knownFrameTokens.get(token);
  if (!known || !framePort) {
    return;
  }
  const link = measureFrameElement(known.el, known.slot, point);
  if (!link) {
    return;
  }
  try {
    framePort.postMessage({
      op: "frame_link",
      token: token,
      origin: typeof origin === "string" ? origin : "",
      slot: known.slot,
      link: link,
    });
  } catch (_err) {}
}

function onFrameMessage(event) {
  const data = event && event.data;
  if (!data || typeof data !== "object") {
    return;
  }
  const token = data.handsFrameToken;
  if (typeof token !== "string" || token.length < 8 || token.length > 128) {
    return;
  }
  const match = findFrameElementBySource(event.source);
  if (!match) {
    return;
  }
  // Report iframe identity even if the link measurement/report later fails, so the SW can
  // dedupe the parent element row by slot without dropping a distinct same-origin sibling.
  try {
    framePort.postMessage({
      op: "frame_slot",
      token: token,
      origin: typeof event.origin === "string" ? event.origin : "",
      slot: match.slot,
    });
  } catch (_err) {}
  if (knownFrameTokens.size >= 64 && !knownFrameTokens.has(token)) {
    const oldest = knownFrameTokens.keys().next();
    if (!oldest.done) {
      knownFrameTokens.delete(oldest.value);
    }
  }
  knownFrameTokens.set(token, {
    el: match.el,
    slot: match.slot,
    origin: typeof event.origin === "string" ? event.origin : "",
  });
  reportFrameLink(token, event.origin);
}

function measureTokens(tokens, point) {
  if (!Array.isArray(tokens)) {
    return;
  }
  for (let i = 0; i < tokens.length && i < 32; i += 1) {
    const token = tokens[i];
    const known = typeof token === "string" ? knownFrameTokens.get(token) : null;
    if (!known) {
      try {
        framePort.postMessage({ op: "frame_link_missing", token: token });
      } catch (_err) {}
      continue;
    }
    reportFrameLink(token, known.origin, point);
  }
}

function frameTopReply() {
  return {
    frame: 0,
    metrics: windowMetrics(),
    url: location.href || "",
    title: document.title || "",
    visibilityState: document.visibilityState,
  };
}

function frameCommitReply(msg) {
  const snap = typeof msg.snap === "string" ? msg.snap : "";
  const pending = pendingSnapshots.get(snap);
  if (!pending) {
    return { error: "stale_snapshot" };
  }
  if (pending.commit_by != null && Date.now() > pending.commit_by) {
    // The SW's fan-out budget already expired: a late commit must not replace lastSnapshot.
    pendingSnapshots.delete(snap);
    return { error: "stale_snapshot" };
  }
  pendingSnapshots.delete(snap);
  lastSnapshot = pending;
  return {
    frame: SELF_FRAME,
    doc: pending.doc,
    snap: pending.snap,
    retained: pending.retained,
    committed: true,
  };
}

function frameBindingReply(msg) {
  const frame = Number(msg.frame);
  if (!Number.isInteger(frame) || frame <= 0) {
    return { error: "protocol_error" };
  }
  if (SELF_FRAME == null || SELF_FRAME !== frame) {
    return { error: "frame_unavailable" };
  }
  if (msg.doc && msg.doc !== DOC_NONCE) {
    return { error: "stale_document", doc: DOC_NONCE, snap: lastSnapshot ? lastSnapshot.snap : null };
  }
  const record = lastSnapshot;
  if (!record) {
    return { error: "stale_document", doc: DOC_NONCE, snap: null };
  }
  if (msg.snap && msg.snap !== record.snap) {
    return {
      error: "stale_snapshot",
      doc: DOC_NONCE,
      snap: record.snap,
    };
  }
  const idx = Number(msg.idx);
  if (
    !Number.isInteger(idx) ||
    idx < 0 ||
    idx >= record.nodes.length ||
    idx >= record.retained
  ) {
    return {
      error: "stale_element",
      doc: DOC_NONCE,
      snap: record.snap,
      idx: idx,
    };
  }
  const rectCss = cssRect(record.nodes[idx]);
  if (!rectCss) {
    // The node was removed or became unrendered between resolve and the probe.
    return {
      error: "stale_element",
      doc: DOC_NONCE,
      snap: record.snap,
      idx: idx,
    };
  }
  return {
    frame: frame,
    doc: DOC_NONCE,
    snap: record.snap,
    idx: idx,
    rectCss: rectCss,
    connected: true,
  };
}

function frameTargetAllowed(msg) {
  const selfFrame = SELF_FRAME == null ? null : SELF_FRAME;
  const frame = msg && msg.frame;
  if (msg && (msg.op === "frame_binding" || msg.op === "frame_commit")) {
    return Number.isInteger(Number(frame)) && Number(frame) > 0;
  }
  if (msg && msg.op === "frame_top") {
    return selfFrame == null || selfFrame === 0;
  }
  if (frame == null) {
    // SW `{frameId}` routing is the primary defence. A missing frame field is accepted here
    // (a direct child snapshot must still answer); a *present* mismatch is rejected below.
    return true;
  }
  if (Number(frame) === 0) {
    // Never reject frame:0 while SELF_FRAME is unset (a top op right after load).
    return selfFrame == null || selfFrame === 0;
  }
  return selfFrame != null && Number(frame) === selfFrame;
}

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  const op = msg && msg.op;
  if (
    op !== "snapshot" &&
    op !== "resolve" &&
    op !== "query" &&
    op !== "assess" &&
    op !== "postcheck" &&
    op !== "frame_top" &&
    op !== "frame_binding" &&
    op !== "frame_commit"
  ) {
    return false;
  }
  if (!frameTargetAllowed(msg)) {
    sendResponse({ error: "protocol_error" });
    return false;
  }
  try {
    if (op === "snapshot") {
      sendResponse(buildSnapshot(msg));
    } else if (op === "query") {
      sendResponse(queryScoped(msg));
    } else if (op === "assess") {
      sendResponse(buildAssess(msg));
    } else if (op === "postcheck") {
      sendResponse(buildPostcheck(msg));
    } else if (op === "frame_top") {
      sendResponse(frameTopReply());
    } else if (op === "frame_binding") {
      sendResponse(frameBindingReply(msg));
    } else if (op === "frame_commit") {
      sendResponse(frameCommitReply(msg));
    } else if (
      msg.v === 1 &&
      msg.doc &&
      msg.snap &&
      Number.isInteger(msg.idx)
    ) {
      sendResponse(resolveScoped(msg));
    } else {
      sendResponse(buildResolve(msg.id, msg.detail, msg.assess === true, msg.samples));
    }
  } catch (_err) {
    sendResponse({ error: "walk-failed" });
  }
  return false;
});

function buildSnapshot(msg) {
  if (msg && typeof msg.changes_since === "string") {
    return buildChanges(msg);
  }
  const detail = msg && msg.detail;
  const store = msg && msg.store;
  // A child snapshot is lightweight: elements + metrics + identity only. The top frame keeps
  // main_text/cards/listing.
  const light = msg && msg.light === true;
  const nodeCap = Number.isInteger(msg && msg.max_nodes)
    ? Math.max(1, Math.min(SNAPSHOT_NODE_CAP, Number(msg.max_nodes)))
    : SNAPSHOT_NODE_CAP;
  const metrics = windowMetrics();
  const nodes = walk(detail).slice(0, nodeCap);
  const collectedEls = collectElements(nodes);
  const elements = collectedEls.elements;
  const keys = collectedEls.keys;
  const pending = msg && msg.pending === true;
  const topFrame = (SELF_FRAME == null ? 0 : SELF_FRAME) === 0;
  const collected = light ? { cards: [], cards_walked: 0 } : collectCards();
  const mainTxt = light ? "" : mainText();
  // The top reply's frame rows are read once: the digest and the reply share one synchronous read.
  let frameRowsCache = null;
  const frameRowsNow = () => {
    if (frameRowsCache === null) {
      frameRowsCache = frameElementsReply();
    }
    return frameRowsCache;
  };
  const commitBy = Number.isFinite(Number(msg && msg.commit_by))
    ? Number(msg.commit_by)
    : null;
  const stored = store !== false && !pending;
  // Revision is minted only by the agent-facing top-frame store. A candidate/pending/store:false
  // reply computes nothing here and never touches the stored record.
  let rev = null;
  let fp = null;
  let shapeTok = null;
  let proj = null;
  if (stored && topFrame) {
    proj = buildProjection(nodes, keys, elements, collected.cards, mainTxt, frameRowsNow());
    fp = digest64(JSON.stringify(proj));
    shapeTok = revShape(detail, nodeCap, light);
    const prev = lastSnapshot;
    const prevRev = prev && prev.doc === DOC_NONCE && Number.isInteger(prev.rev) ? prev : null;
    rev = prevRev ? (prevRev.fp === fp ? prevRev.rev : prevRev.rev + 1) : 1;
  }
  let doc = null;
  let snap = null;
  let retained = nodes.length;
  if (stored) {
    doc = DOC_NONCE;
    snap = nextSnap();
    lastSnapshot = {
      doc: doc,
      snap: snap,
      nodes: nodes,
      keys: keys,
      retained: nodes.length,
      // Internal baseline for `url_changed`/`title_changed`; never emitted in an envelope.
      url: location.href || "",
      title: document.title || "",
      rev: rev,
      fp: fp,
      shape: shapeTok,
      proj: proj,
    };
  } else if (pending) {
    // Candidate only: the SW commits it after the fan-out deadline. A discarded late reply
    // therefore never replaces `lastSnapshot`.
    doc = DOC_NONCE;
    snap = nextSnap();
    if (pendingSnapshots.size >= 8) {
      const oldest = pendingSnapshots.keys().next();
      if (!oldest.done) {
        pendingSnapshots.delete(oldest.value);
      }
    }
    pendingSnapshots.set(snap, {
      doc: doc,
      snap: snap,
      nodes: nodes,
      keys: keys,
      retained: nodes.length,
      commit_by: commitBy,
      url: location.href || "",
      title: document.title || "",
    });
  } else {
    retained = 0;
  }
  const out = {
    url: location.href || "",
    title: document.title || "",
    elements: elements,
    cards: collected.cards,
    metrics: metrics,
    assess_v: 1,
    stored: stored,
  };
  if (!light) {
    out.main_text = mainTxt;
  }
  if (light) {
    out.elements = rankFrameElements(elements, metrics).slice(0, FRAME_ELEMENT_CAP);
  }
  if (stored || pending) {
    out.doc = doc;
    out.snap = snap;
    out.retained = retained;
  }
  if (pending) {
    out.pending = true;
  }
  if (stored && topFrame) {
    out.rev_v = REV_V;
    out.rev_core = doc + "." + rev + "." + fp + "." + shapeTok;
  }
  const selfFrame = SELF_FRAME == null ? 0 : SELF_FRAME;
  if (selfFrame !== 0) {
    out.frame = selfFrame;
    // Identity is useful even when the caller asked for no stored snapshot (solver/hittest).
    if (!stored) {
      out.doc = DOC_NONCE;
    }
  }
  if (
    msg &&
    (msg.frame_elements === true ||
      (selfFrame === 0 &&
        msg.frames === true &&
        (msg.frame == null || Number(msg.frame) === 0)))
  ) {
    out.frame_elements = frameRowsNow();
  }
  if (collected.cards_walked > 0) {
    out.cards_walked = collected.cards_walked;
  }
  if (!light) {
    const listing = collectListingMeta();
    if (listing.result_count) out.result_count = listing.result_count;
    if (listing.local_matches) out.local_matches = listing.local_matches;
    if (listing.empty_state) out.empty_state = listing.empty_state;
    if (listing.zip) out.zip = listing.zip;
    if (listing.radius) out.radius = listing.radius;
  }
  return out;
}

function rankFrameElements(elements, metrics) {
  const vw = Number(metrics && metrics.innerWidth) || 0;
  const vh = Number(metrics && metrics.innerHeight) || 0;
  const interactive = /^(Button|Hyperlink|Edit|ComboBox|CheckBox|RadioButton|TabItem|MenuItem)$/;
  const rank = (el) => {
    const r = el.rectCss;
    const inViewport =
      r && r.left < vw && r.top < vh && r.left + r.width > 0 && r.top + r.height > 0;
    const reserved = interactive.test(String(el.role || "")) ? 0 : 1;
    return (inViewport ? 0 : 2) + reserved;
  };
  return elements.slice().sort((a, b) => {
    const diff = rank(a) - rank(b);
    if (diff !== 0) return diff;
    return Number(a.idx || 0) - Number(b.idx || 0);
  });
}

function buildResolve(id, detail, assess, samples) {
  const index = parseChrId(id);
  const metrics = windowMetrics();
  if (index == null) {
    return { error: "not-found", metrics: metrics };
  }
  const nodes = walk(detail);
  if (index >= nodes.length) {
    return { error: "not-found", metrics: metrics };
  }
  const payload = nodePayload(nodes[index], index);
  if (!payload) {
    return { error: "not-found", metrics: metrics };
  }
  const reply = {
    id: payload.id,
    role: payload.role,
    text: payload.text,
    rectCss: payload.rectCss,
    href: payload.href,
    url: location.href || "",
    title: document.title || "",
    metrics: metrics,
  };
  if (assess === true) {
    reply.assessment = assessAt(nodes[index], payload.rectCss, samples);
  }
  return reply;
}

function resolveScoped(msg) {
  const selfFrame = SELF_FRAME == null ? 0 : SELF_FRAME;
  if (Number(msg.frame) !== selfFrame) {
    return { error: "frame_unsupported" };
  }
  const record = lastSnapshot;
  if (!record) {
    return { error: "stale_document", doc: DOC_NONCE, snap: null };
  }
  if (msg.doc !== record.doc) {
    return { error: "stale_document", doc: record.doc, snap: record.snap };
  }
  if (msg.snap !== record.snap) {
    return { error: "stale_snapshot", doc: record.doc, snap: record.snap };
  }
  const idx = Number(msg.idx);
  if (
    !Number.isInteger(idx) ||
    idx < 0 ||
    idx >= record.nodes.length ||
    idx >= record.retained
  ) {
    return { error: "stale_element", doc: record.doc, snap: record.snap };
  }
  const node = record.nodes[idx];
  if (!node || !node.isConnected) {
    return { error: "stale_element", doc: record.doc, snap: record.snap };
  }
  const payload = keyPayload(node, idx);
  if (snapshotKey(node, payload) !== record.keys[idx]) {
    return { error: "stale_identity", doc: record.doc, snap: record.snap };
  }
  const rect = cssRect(node);
  if (!rect) {
    return { error: "not_visible", doc: record.doc, snap: record.snap };
  }
  payload.rectCss = rect;
  const reply = {
    id: payload.id,
    role: payload.role,
    text: payload.text,
    rectCss: rect,
    url: location.href || "",
    title: document.title || "",
    metrics: windowMetrics(),
    visibilityState: document.visibilityState,
    doc: record.doc,
    snap: record.snap,
    idx: idx,
    frame: selfFrame,
  };
  if (payload.href) {
    reply.href = payload.href;
  }
  if (msg.assess === true) {
    reply.assessment = assessAt(node, rect, msg.samples);
  }
  if (msg.caps === true) {
    reply.capability = domCapability(node, payload);
  }
  return reply;
}

// Read-only list query over the stored snapshot only. Never walks, never re-snapshots, never
// assigns `lastSnapshot`. Filter semantics are pinned against `tests/fixtures/find-cases.json`.
function queryScoped(msg) {
  const selfFrame = SELF_FRAME == null ? 0 : SELF_FRAME;
  if (Number(msg.frame) !== selfFrame) {
    return { error: "frame_unsupported" };
  }
  const record = lastSnapshot;
  if (!record) {
    return { error: "no_snapshot", doc: DOC_NONCE, snap: null, frame: selfFrame };
  }
  if (msg.doc !== record.doc) {
    return {
      error: "stale_document",
      doc: record.doc,
      snap: record.snap,
      frame: selfFrame,
    };
  }
  if (msg.snap !== record.snap) {
    return {
      error: "stale_snapshot",
      doc: record.doc,
      snap: record.snap,
      frame: selfFrame,
    };
  }
  const limit = Number.isInteger(msg.limit) ? Math.max(1, Number(msg.limit)) : 20;
  const offset = Number.isInteger(msg.offset) ? Math.max(0, Number(msg.offset)) : 0;
  const budgetMs = Number.isFinite(Number(msg.budgetMs))
    ? Math.max(0, Number(msg.budgetMs))
    : 150;
  const maxNodes = Number.isInteger(msg.maxNodes)
    ? Math.max(1, Number(msg.maxNodes))
    : SNAPSHOT_NODE_CAP;
  const wantRole =
    typeof msg.role === "string" && msg.role ? msg.role.toLowerCase() : null;
  const wantText =
    typeof msg.text === "string" && msg.text ? msg.text.toLowerCase() : null;
  const wantRect =
    msg.rectCss && typeof msg.rectCss === "object" ? msg.rectCss : null;
  let root = null;
  if (msg.subtreeRoot != null) {
    const rootIdx = Number(msg.subtreeRoot);
    if (
      !Number.isInteger(rootIdx) ||
      rootIdx < 0 ||
      rootIdx >= record.nodes.length ||
      rootIdx >= record.retained
    ) {
      return {
        error: "stale_element",
        doc: record.doc,
        snap: record.snap,
        frame: selfFrame,
      };
    }
    root = record.nodes[rootIdx];
    if (!root || !root.isConnected || !cssRect(root)) {
      return {
        error: "stale_element",
        doc: record.doc,
        snap: record.snap,
        frame: selfFrame,
      };
    }
  }
  const started = Date.now();
  const scanCap = Math.min(record.nodes.length, maxNodes);
  let examined = 0;
  let matchedTotal = 0;
  let budgetHit = false;
  const matches = [];
  for (let i = 0; i < scanCap; i += 1) {
    if (Date.now() - started > budgetMs) {
      budgetHit = true;
      break;
    }
    const node = record.nodes[i];
    examined += 1;
    if (!node) {
      continue;
    }
    if (root) {
      if (i === 0 && node === root) {
        continue;
      }
      if (node === root || !nodeContains(root, node)) {
        continue;
      }
    }
    const payload = keyPayload(node, i);
    if (wantRole && String(payload.role || "").toLowerCase() !== wantRole) {
      continue;
    }
    if (wantText) {
      const haystack = String(payload.text || "").toLowerCase();
      if (haystack.indexOf(wantText) < 0) {
        continue;
      }
    }
    const rect = cssRect(node);
    if (wantRect && (!rect || !rectsIntersect(rect, wantRect))) {
      continue;
    }
    matchedTotal += 1;
    if (matchedTotal <= offset || matches.length >= limit) {
      continue;
    }
    const connected = node.isConnected !== false;
    const keyOk = connected && snapshotKey(node, payload) === record.keys[i];
    const row = {
      id: "chr:" + String(i),
      idx: i,
      role: payload.role,
      text: payload.text,
      keyOk: keyOk,
      connected: connected,
    };
    if (rect) {
      row.rectCss = rect;
    }
    if (payload.href) {
      row.href = payload.href;
    }
    matches.push(row);
  }
  return {
    doc: record.doc,
    snap: record.snap,
    frame: selfFrame,
    retained: record.retained,
    examined: examined,
    budgetHit: budgetHit,
    matchedTotal: matchedTotal,
    matches: matches,
    metrics: windowMetrics(),
    url: location.href || "",
    title: document.title || "",
    visibilityState: document.visibilityState,
  };
}

function nodeContains(root, node) {
  if (!root || !node) {
    return false;
  }
  if (typeof root.contains === "function") {
    try {
      return root.contains(node);
    } catch (_err) {
      return false;
    }
  }
  let cur = node.parentElement || null;
  for (let i = 0; i < 256 && cur; i += 1) {
    if (cur === root) {
      return true;
    }
    cur = cur.parentElement || null;
  }
  return false;
}

function rectsIntersect(a, b) {
  const left = Math.max(a.left, b.left);
  const top = Math.max(a.top, b.top);
  const right = Math.min(a.left + a.width, b.left + b.width);
  const bottom = Math.min(a.top + a.height, b.top + b.height);
  return right > left && bottom > top;
}

// Read-only DOM capability evidence. Attributes only: never `value`, never focus/click.
// Role→verb tables are mirrored by `src/chrome.rs` (`fixture_capability`); a parity test keeps
// the live and fixture vocabularies identical.
const CAP_INVOKE_ROLES = [
  "button",
  "hyperlink",
  "tabitem",
  "menuitem",
  "checkbox",
  "radiobutton",
  "treeitem",
];
const CAP_TOGGLE_ROLES = ["checkbox", "radiobutton"];
const CAP_EXPAND_ROLES = ["combobox"];
const CAP_SELECT_ROLES = ["tabitem", "menuitem", "treeitem"];
const CAP_VALUE_ROLES = ["edit", "combobox"];

function domCapability(el, payload) {
  const role = String((payload && payload.role) || "").toLowerCase();
  const tag = String(el.tagName || "").toUpperCase();
  const has = (name) => el.hasAttribute(name);
  const attr = (name) => String(el.getAttribute(name) || "").toLowerCase();
  const editable = isEditableSurface(el) || el.isContentEditable === true;
  const password = isPassword(el);
  const invoke = CAP_INVOKE_ROLES.indexOf(role) >= 0;
  const toggle =
    has("aria-checked") ||
    (tag === "INPUT" && (el.type === "checkbox" || el.type === "radio")) ||
    CAP_TOGGLE_ROLES.indexOf(role) >= 0;
  const expand = has("aria-expanded") || CAP_EXPAND_ROLES.indexOf(role) >= 0;
  const select =
    has("aria-selected") ||
    CAP_SELECT_ROLES.indexOf(role) >= 0 ||
    attr("role") === "option";
  const setValue =
    editable && !password && CAP_VALUE_ROLES.indexOf(role) >= 0;
  const action = (verb, available, evidence) => {
    const row = {
      action: verb,
      support: available ? "available" : "unknown",
      evidence: available ? evidence : "no_markup_evidence",
    };
    return row;
  };
  const actions = [
    action("invoke", invoke, role === "button" ? "role_button" : "role_markup"),
    action("toggle", toggle, has("aria-checked") ? "aria_checked" : "role_checkbox"),
    action("expand", expand, has("aria-expanded") ? "aria_expanded" : "role_expand"),
    action(
      "collapse",
      expand,
      has("aria-expanded") ? "aria_expanded" : "role_expand"
    ),
    action(
      "select",
      select,
      has("aria-selected") ? "aria_selected" : "role_selection"
    ),
    action("set_value", setValue, "editable_role"),
  ];
  const disabled =
    el.disabled === true ||
    has("disabled") ||
    attr("aria-disabled") === "true" ||
    has("inert");
  const state = { enabled: !disabled, offscreen: null, read_only: null };
  if (editable) {
    state.read_only = has("readonly") || attr("aria-readonly") === "true";
  }
  const ariaChecked = attr("aria-checked");
  if (ariaChecked === "true") {
    state.checked = "checked";
  } else if (ariaChecked === "false") {
    state.checked = "unchecked";
  } else if (ariaChecked === "mixed") {
    state.checked = "mixed";
  } else if (tag === "INPUT" && typeof el.checked === "boolean") {
    state.checked = el.checked ? "checked" : "unchecked";
  }
  const ariaExpanded = attr("aria-expanded");
  if (ariaExpanded === "true") {
    state.expanded = "expanded";
  } else if (ariaExpanded === "false") {
    state.expanded = "collapsed";
  }
  const ariaSelected = attr("aria-selected");
  if (ariaSelected === "true") {
    state.selected = true;
  } else if (ariaSelected === "false") {
    state.selected = false;
  }
  const notes = ["dom_advertised_not_proven"];
  if (disabled) {
    notes.push("disabled");
  }
  return {
    source: "dom",
    actions: actions,
    state: state,
    notes: notes.slice(0, 3),
  };
}

function isPostcheckRendered(el) {
  if (!el || !el.isConnected) {
    return false;
  }
  if (cssRect(el) == null) {
    return false;
  }
  try {
    const style = window.getComputedStyle(el);
    if (style && (style.display === "none" || style.visibility === "hidden")) {
      return false;
    }
  } catch (_err) {}
  return true;
}

function isEditableActive(el) {
  if (!el || el === document.body || el === document.documentElement) {
    return false;
  }
  const tag = String(el.tagName || "").toUpperCase();
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
    return true;
  }
  if (el.isContentEditable) {
    return true;
  }
  const role = String(el.getAttribute("role") || "").toLowerCase();
  return (
    role === "textbox" || role === "combobox" || role === "searchbox"
  );
}

// Read-only postcondition check. Returns booleans/states only: never page text, URL, title,
// value, a snapshot walk, or a DOM mutation. Stored-node kinds follow the clicked snapshot.
function buildPostcheck(msg) {
  const kind = msg && msg.kind;
  const record = lastSnapshot;
  const reply = {
    doc: DOC_NONCE,
    snap: record ? record.snap : null,
    visibilityState: document.visibilityState,
  };
  const unavailable = (reason) =>
    Object.assign(reply, { state: "unavailable", reason: reason });
  const storedNode = () => {
    if (!record) {
      return { error: "stale_document" };
    }
    if (msg.doc !== record.doc) {
      return { error: "stale_document" };
    }
    if (msg.snap !== record.snap) {
      return { error: "stale_snapshot" };
    }
    const idx = Number(msg.idx);
    if (
      !Number.isInteger(idx) ||
      idx < 0 ||
      idx >= record.nodes.length ||
      idx >= record.retained
    ) {
      return { error: "stale_element" };
    }
    const node = record.nodes[idx];
    if (!node) {
      return { error: "stale_element" };
    }
    return { node: node };
  };
  if (kind === "url_prefix" || kind === "url_equals" || kind === "title_equals") {
    if (typeof msg.value !== "string") {
      return unavailable("invalid_condition");
    }
    if (kind === "title_equals") {
      return Object.assign(reply, {
        state: document.title === msg.value ? "satisfied" : "unsatisfied",
      });
    }
    if (kind === "url_prefix") {
      return Object.assign(reply, {
        state: (location.href || "").startsWith(msg.value)
          ? "satisfied"
          : "unsatisfied",
      });
    }
    return Object.assign(reply, {
      state: location.href === msg.value ? "satisfied" : "unsatisfied",
    });
  }
  if (kind === "url_changed" || kind === "title_changed") {
    if (!record) {
      return unavailable("stale_document");
    }
    if (msg.doc !== record.doc || msg.snap !== record.snap) {
      return unavailable("baseline_mismatch");
    }
    const before = kind === "url_changed" ? record.url : record.title;
    if (typeof before !== "string") {
      return unavailable("baseline_mismatch");
    }
    const now = kind === "url_changed" ? location.href || "" : document.title || "";
    return Object.assign(reply, {
      state: now !== before ? "satisfied" : "unsatisfied",
    });
  }
  if (kind === "element_present" || kind === "element_absent") {
    const found = storedNode();
    if (found.error) {
      return unavailable(found.error);
    }
    const rendered = isPostcheckRendered(found.node);
    const satisfied = kind === "element_present" ? rendered : !rendered;
    return Object.assign(reply, {
      state: satisfied ? "satisfied" : "unsatisfied",
    });
  }
  if (kind === "focus_editable") {
    return Object.assign(reply, {
      state: isEditableActive(document.activeElement)
        ? "satisfied"
        : "unavailable",
      reason: isEditableActive(document.activeElement) ? undefined : "not_observed",
    });
  }
  if (kind === "dialog_present") {
    let found = false;
    try {
      const dialogs = document.querySelectorAll(
        'dialog[open], [role="dialog"], [role="alertdialog"]'
      );
      const cap = Math.min(dialogs.length, 10);
      for (let i = 0; i < cap; i += 1) {
        if (isPostcheckRendered(dialogs[i])) {
          found = true;
          break;
        }
      }
    } catch (_err) {}
    return Object.assign(reply, {
      state: found ? "satisfied" : "unavailable",
      reason: found ? undefined : "not_observed",
    });
  }
  if (kind === "count_at_least") {
    const role = msg.role;
    const n = Number(msg.n);
    if (
      typeof role !== "string" ||
      !/^[a-z][a-z-]*$/.test(role) ||
      !Number.isInteger(n) ||
      n < 1 ||
      n > 50
    ) {
      return unavailable("invalid_condition");
    }
    let count = 0;
    try {
      const nodes = document.querySelectorAll('[role="' + role + '"]');
      const cap = Math.min(nodes.length, 50);
      for (let i = 0; i < cap; i += 1) {
        if (isPostcheckRendered(nodes[i])) {
          count += 1;
          if (count >= n) {
            break;
          }
        }
      }
      if (count < n && nodes.length >= 50) {
        return unavailable("count_capped");
      }
    } catch (_err) {
      return unavailable("evaluation_failed");
    }
    return Object.assign(reply, {
      state: count >= n ? "satisfied" : "unavailable",
      reason: count >= n ? undefined : "not_observed",
    });
  }
  return unavailable("unsupported");
}

function parseChrId(id) {
  if (typeof id === "number" && Number.isInteger(id) && id >= 0) {
    return id;
  }
  if (typeof id !== "string") {
    return null;
  }
  const m = /^chr:(0|[1-9]\d*)$/.exec(id);
  if (!m) {
    return null;
  }
  const n = Number(m[1]);
  if (!Number.isFinite(n) || n > 4294967295) {
    return null;
  }
  return n;
}

function walk(detail) {
  const selector = detail === "dom" ? DOM_SELECTOR : DEFAULT_SELECTOR;
  const out = [];
  const root = document.documentElement;
  if (root) {
    collect(root, selector, out);
  }
  return out;
}

function collect(node, selector, out) {
  if (!node || node.nodeType !== 1) {
    return;
  }
  if (node.matches(selector) && includeNode(node) && openRowGate(node)) {
    out.push(node);
  }
  const shadow = node.shadowRoot;
  if (shadow) {
    const shadowKids = shadow.children;
    for (let i = 0; i < shadowKids.length; i += 1) {
      collect(shadowKids[i], selector, out);
    }
  }
  const kids = node.children;
  for (let i = 0; i < kids.length; i += 1) {
    collect(kids[i], selector, out);
  }
}

function includeNode(el) {
  if (!el || el.nodeType !== 1) {
    return false;
  }
  if (el.disabled || el.hasAttribute("disabled")) {
    return false;
  }
  if (el.hidden || el.hasAttribute("hidden")) {
    return false;
  }
  if (el.getAttribute("aria-hidden") === "true") {
    return false;
  }
  if (el.closest('[aria-hidden="true"], [hidden]')) {
    return false;
  }
  const rect = el.getBoundingClientRect();
  if (!rect || rect.width <= 0 || rect.height <= 0) {
    return false;
  }
  const style = window.getComputedStyle(el);
  if (!style) {
    return false;
  }
  if (style.display === "none" || style.visibility === "hidden") {
    return false;
  }
  return true;
}

function openRowGate(el) {
  const role = (el.getAttribute("role") || "").toLowerCase();
  if (
    role !== "option" &&
    role !== "menuitemcheckbox" &&
    role !== "menuitemradio" &&
    role !== "treeitem"
  ) {
    return true;
  }
  try {
    const popover = el.closest("[popover]");
    if (popover && !popover.matches(":popover-open")) {
      return false;
    }
  } catch (_err) {
    // :popover-open unsupported — do not exclude a visible row
  }
  const box = el.closest("[role=listbox],[role=menu],[role=tree]");
  if (box && box.id) {
    const controller = findPopupController(box.id);
    if (controller) {
      const expanded = controller.getAttribute("aria-expanded");
      if (expanded === "false") {
        return false;
      }
      if (expanded === "true") {
        return true;
      }
    }
  }
  return true;
}

function findPopupController(id) {
  let escaped = id;
  if (typeof CSS !== "undefined" && CSS.escape) {
    escaped = CSS.escape(id);
  }
  try {
    return document.querySelector(
      '[aria-controls="' + escaped + '"], [aria-owns~="' + escaped + '"]'
    );
  } catch (_err) {
    return document.querySelector(
      '[aria-controls="' + id + '"], [aria-owns~="' + id + '"]'
    );
  }
}

function keyPayload(el, index) {
  const password = isPassword(el);
  const href = hrefOf(el);
  const payload = {
    id: "chr:" + String(index),
    role: mapRole(el),
    text: password ? null : nodeText(el),
  };
  if (href) {
    payload.href = href;
  }
  return payload;
}

function nodePayload(el, index) {
  const rect = cssRect(el);
  if (!rect) {
    return null;
  }
  const payload = keyPayload(el, index);
  payload.rectCss = rect;
  return payload;
}

function isEditableSurface(el) {
  const tag = String(el.tagName || "");
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
    return true;
  }
  if (el.isContentEditable) {
    return true;
  }
  const role = (el.getAttribute("role") || "").toLowerCase();
  return role === "textbox" || role === "combobox";
}

function nonChurningName(el, payload) {
  if (isPassword(el)) {
    return "";
  }
  const aria = attr(el, "aria-label");
  if (aria) {
    return cap(aria, TEXT_CAP);
  }
  const placeholder = attr(el, "placeholder");
  if (placeholder) {
    return cap(placeholder, TEXT_CAP);
  }
  const alt = attr(el, "alt");
  if (alt) {
    return cap(alt, TEXT_CAP);
  }
  const name = attr(el, "name");
  if (name) {
    return cap(name, TEXT_CAP);
  }
  if (isEditableSurface(el)) {
    return "";
  }
  return payload && payload.text ? payload.text : "";
}

function fnv1a32(str) {
  let hash = 0x811c9dc5;
  for (let i = 0; i < str.length; i += 1) {
    const code = str.charCodeAt(i);
    hash ^= code & 0xff;
    hash = Math.imul(hash, 0x01000193) >>> 0;
    hash ^= (code >>> 8) & 0xff;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

function snapshotKey(el, payload) {
  const tag = String(el.tagName || "").toLowerCase();
  const role = payload && payload.role ? payload.role : "";
  const name = nonChurningName(el, payload);
  const href = payload && payload.href ? payload.href : "";
  const type =
    el.tagName === "INPUT" ? String(el.type || "").toLowerCase() : "";
  return fnv1a32([tag, role, name, href, type].join("\u0001"));
}

function cssRect(el) {
  const r = el.getBoundingClientRect();
  if (!r || r.width <= 0 || r.height <= 0) {
    return null;
  }
  return {
    left: r.left,
    top: r.top,
    width: r.width,
    height: r.height,
  };
}

function isPassword(el) {
  return el.tagName === "INPUT" && String(el.type || "").toLowerCase() === "password";
}

function nodeText(el) {
  const aria = attr(el, "aria-label");
  if (aria) {
    return cap(aria, TEXT_CAP);
  }
  const placeholder = attr(el, "placeholder");
  if (placeholder) {
    return cap(placeholder, TEXT_CAP);
  }
  const alt = attr(el, "alt");
  if (alt) {
    return cap(alt, TEXT_CAP);
  }
  const raw = (el.innerText || "").replace(/\s+/g, " ").trim();
  return cap(raw, TEXT_CAP);
}

function attr(el, name) {
  const v = el.getAttribute(name);
  if (!v) {
    return "";
  }
  return v.replace(/\s+/g, " ").trim();
}

function hrefOf(el) {
  if (el.tagName === "A") {
    const href = el.getAttribute("href");
    if (href && !isJsHref(href)) {
      return cap(el.href || href, CARD_HREF_CAP);
    }
  }
  return undefined;
}

function isJsHref(href) {
  return String(href || "")
    .trim()
    .toLowerCase()
    .indexOf("javascript:") === 0;
}

function mapRole(el) {
  const explicit = (el.getAttribute("role") || "").toLowerCase();
  const mapped = roleName(explicit);
  if (mapped) {
    return mapped;
  }
  const tag = el.tagName;
  if (tag === "A") {
    return "Hyperlink";
  }
  if (tag === "BUTTON") {
    return "Button";
  }
  if (tag === "SELECT") {
    return "ComboBox";
  }
  if (tag === "TEXTAREA") {
    return "Edit";
  }
  if (tag === "SUMMARY") {
    return "Button";
  }
  if (tag === "LABEL") {
    return "Text";
  }
  if (tag === "LI") {
    return "ListItem";
  }
  if (tag === "INPUT") {
    const t = String(el.type || "text").toLowerCase();
    if (t === "checkbox") {
      return "CheckBox";
    }
    if (t === "radio") {
      return "RadioButton";
    }
    if (t === "submit" || t === "button" || t === "reset" || t === "image") {
      return "Button";
    }
    return "Edit";
  }
  if (el.isContentEditable) {
    return "Edit";
  }
  return "Other";
}

function roleName(role) {
  switch (role) {
    case "button":
      return "Button";
    case "link":
      return "Hyperlink";
    case "tab":
      return "TabItem";
    case "menuitem":
    case "menuitemcheckbox":
    case "menuitemradio":
      return "MenuItem";
    case "checkbox":
      return "CheckBox";
    case "radio":
      return "RadioButton";
    case "textbox":
      return "Edit";
    case "combobox":
      return "ComboBox";
    case "listitem":
      return "ListItem";
    case "option":
      return "ListItem";
    case "slider":
      return "Slider";
    case "treeitem":
      return "TreeItem";
    default:
      return "";
  }
}

function windowMetrics() {
  const de = document.documentElement;
  const vv = window.visualViewport;
  const scr = window.screen;
  return {
    screenX: window.screenX,
    screenY: window.screenY,
    outerWidth: window.outerWidth,
    outerHeight: window.outerHeight,
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
    devicePixelRatio: window.devicePixelRatio,
    clientWidth: de ? de.clientWidth : null,
    clientHeight: de ? de.clientHeight : null,
    visualViewportWidth: vv ? vv.width : null,
    visualViewportHeight: vv ? vv.height : null,
    visualViewportScale: vv ? vv.scale : null,
    visualViewportOffsetLeft: vv ? vv.offsetLeft : null,
    visualViewportOffsetTop: vv ? vv.offsetTop : null,
    screenAvailLeft: scr ? scr.availLeft : null,
    screenAvailTop: scr ? scr.availTop : null,
    screenWidth: scr ? scr.width : null,
    screenHeight: scr ? scr.height : null,
  };
}

function mainText() {
  const main = document.querySelector("main");
  const raw = ((main && main.innerText) || (document.body && document.body.innerText) || "")
    .replace(/\s+\n/g, "\n")
    .trim();
  return cap(raw, MAIN_TEXT_CAP);
}

function collectCards() {
  const seen = [];
  const out = [];
  const nodes = document.querySelectorAll("article,[role=\"listitem\"],li,a[href]");
  for (let i = 0; i < nodes.length && out.length < CARD_WALK_CAP; i += 1) {
    const el = nodes[i];
    if (!includeNode(el) || isPassword(el)) {
      continue;
    }
    if (seen.indexOf(el) !== -1) {
      continue;
    }
    seen.push(el);
    const tag = el.tagName;
    const role = (el.getAttribute("role") || "").toLowerCase();
    const text = (el.innerText || "").replace(/\s+/g, " ").trim();
    const priced = PRICE_RE.test(text);
    const listing = tag === "ARTICLE" || role === "listitem";
    if (!listing && !priced) {
      continue;
    }
    if (!priced && listing && !text) {
      continue;
    }
    const href = cardHref(el);
    if (!href) {
      continue;
    }
    const rect = cssRect(el);
    if (!rect) {
      continue;
    }
    const price = cap(extractPrice(text), CARD_PRICE_CAP);
    const title = cap(cardTitle(el, text, price), CARD_TITLE_CAP);
    if (!title) {
      continue;
    }
    const card = {
      title: title,
      price: price,
      href: cap(href, CARD_HREF_CAP),
      rectCss: rect,
    };
    const miles = cardMiles(el, text);
    if (miles) card.miles = miles;
    const dealer = cardDealer(el, text, title, price);
    if (dealer) card.dealer = dealer;
    const distance = cardDistance(text);
    if (distance) card.distance = distance;
    const ofText = cardOf(text);
    if (ofText) card.of = ofText;
    const delivery = cardDelivery(text);
    if (delivery) card.delivery = delivery;
    const kind = cardKind(text, card);
    if (kind) card.kind = kind;
    out.push(card);
  }
  const cards_walked = out.length;
  const packed = preferLocalCards(out, CARD_CAP);
  return { cards: packed, cards_walked: cards_walked };
}

function preferLocalCards(cards, cap) {
  const local = [];
  const unknown = [];
  const ship = [];
  const rec = [];
  for (let i = 0; i < cards.length; i += 1) {
    const k = cards[i].kind;
    if (k === "local") {
      local.push(cards[i]);
    } else if (k === "ship") {
      ship.push(cards[i]);
    } else if (k === "recommended") {
      rec.push(cards[i]);
    } else {
      unknown.push(cards[i]);
    }
  }
  return local.concat(unknown, ship, rec).slice(0, cap);
}

function itempropText(el, name) {
  const node = el.querySelector("[itemprop=\"" + name + "\"]");
  if (!node) {
    return "";
  }
  const content = attr(node, "content");
  if (content) {
    return content;
  }
  return (node.innerText || "").replace(/\s+/g, " ").trim();
}

function cardMiles(el, text) {
  const item = itempropText(el, "mileageFromOdometer");
  if (item) {
    return cap(item, CARD_MILES_CAP);
  }
  const data = attr(el, "data-mileage");
  if (data) {
    return cap(data, CARD_MILES_CAP);
  }
  const m = text.match(/(\d{1,3}(?:,\d{3})+|\d{4,})\s*(mi|miles)\b(?!\s*away)/i);
  return m ? cap(m[0].replace(/\s+/g, " ").trim(), CARD_MILES_CAP) : "";
}

function cardDealer(el, text, title, price) {
  const item = itempropText(el, "seller");
  if (item) {
    return sanitizeDealer(item);
  }
  const data = attr(el, "data-dealer");
  if (data) {
    return sanitizeDealer(data);
  }
  let rest = String(text || "");
  if (title) {
    rest = rest.split(title).join(" ");
  }
  if (price) {
    rest = rest.split(price).join(" ");
  }
  rest = rest.replace(/(\d{1,3}(?:,\d{3})+|\d{4,})\s*(mi|miles)\b(?!\s*away)/ig, " ");
  const distance = cardDistance(rest);
  if (distance) {
    rest = rest.split(distance).join(" ");
  }
  let delivery = cardDelivery(rest);
  while (delivery) {
    rest = rest.split(delivery).join(" ");
    delivery = cardDelivery(rest);
  }
  rest = rest.replace(/\b\d+\s+of\s+\d+\b/ig, " ");
  rest = rest.replace(/\(\s*[\d,]+\s+reviews?\)/ig, " ");
  rest = rest.replace(/[$€£][\d,]+(?:\.\d{2})?/g, " ");
  const junk = [
    "american-made index",
    "american made index",
    "available for delivery",
    "no price analysis",
    "get pre-approved",
    "check availability",
    "days on cars.com",
    "you may also like",
    "contact dealer",
    "home delivery",
    "est. shipping",
    "free carfax",
    "view details",
    "get financing",
    "price drop",
    "great deal",
    "good deal",
    "fair deal",
    "high price",
    "deliver to",
    "shipping to",
    "per month",
    "see more",
    "recommended",
    "autocheck",
    "sponsored",
    "shipping",
    "reviews",
    "ratings",
    "review",
    "rating",
    "stars",
    "star",
    "est.",
    "/mo",
  ];
  for (let i = 0; i < junk.length; i += 1) {
    rest = stripJunkPhrase(rest, junk[i]);
  }
  rest = rest.replace(/\s+/g, " ").trim();
  rest = rest.replace(/^[^A-Za-z0-9]+|[^A-Za-z0-9]+$/g, "");
  return sanitizeDealer(rest);
}

function sanitizeDealer(rest) {
  rest = String(rest || "");
  rest = rest.replace(/\(\s*[\d,]+\s+reviews?\)/ig, " ");
  const junk = [
    "american-made index",
    "american made index",
    "available for delivery",
    "no price analysis",
    "get pre-approved",
    "check availability",
    "days on cars.com",
    "you may also like",
    "contact dealer",
    "home delivery",
    "est. shipping",
    "free carfax",
    "view details",
    "get financing",
    "price drop",
    "great deal",
    "good deal",
    "fair deal",
    "high price",
    "deliver to",
    "shipping to",
    "per month",
    "see more",
    "recommended",
    "autocheck",
    "sponsored",
    "shipping",
    "reviews",
    "ratings",
    "review",
    "rating",
    "stars",
    "star",
    "est.",
    "/mo",
  ];
  for (let i = 0; i < junk.length; i += 1) {
    rest = stripJunkPhrase(rest, junk[i]);
  }
  rest = rest.replace(/[$€£][\d,]+(?:\.\d{2})?/g, " ");
  rest = rest.replace(/\s+/g, " ").trim();
  rest = rest.replace(/^[^A-Za-z0-9]+|[^A-Za-z0-9]+$/g, "");
  if (rest.length < 2 || !/[A-Za-z]/.test(rest)) {
    return "";
  }
  const exact = /^(used|new|save|view|details|more)$/i;
  if (exact.test(rest)) {
    return "";
  }
  const lower = rest.toLowerCase();
  for (let i = 0; i < junk.length; i += 1) {
    if (junkHasPhrase(lower, junk[i])) {
      return "";
    }
  }
  const tokens = rest.split(/\s+/);
  if (tokens.length === 1 && !isDealerishToken(tokens[0])) {
    return "";
  }
  return cap(rest, CARD_DEALER_CAP);
}

function stripJunkPhrase(text, phrase) {
  const lower = text.toLowerCase();
  const p = phrase.toLowerCase();
  let search = 0;
  let out = "";
  let last = 0;
  while (search <= lower.length) {
    const idx = lower.indexOf(p, search);
    if (idx === -1) {
      break;
    }
    if (p === "/mo") {
      out += text.slice(last, idx) + " ";
      last = idx + p.length;
      search = idx + p.length;
      continue;
    }
    const before = idx === 0 || !/[A-Za-z0-9]/.test(lower.charAt(idx - 1));
    const afterCh = lower.charAt(idx + p.length);
    const after = !afterCh || !/[A-Za-z0-9]/.test(afterCh);
    if (before && after) {
      out += text.slice(last, idx) + " ";
      last = idx + p.length;
      search = idx + p.length;
    } else {
      search = idx + 1;
    }
  }
  return out + text.slice(last);
}

function junkHasPhrase(lower, phrase) {
  const p = phrase.toLowerCase();
  if (p === "/mo") {
    return lower.indexOf(p) !== -1;
  }
  let search = 0;
  while (search <= lower.length) {
    const idx = lower.indexOf(p, search);
    if (idx === -1) {
      return false;
    }
    const before = idx === 0 || !/[A-Za-z0-9]/.test(lower.charAt(idx - 1));
    const afterCh = lower.charAt(idx + p.length);
    const after = !afterCh || !/[A-Za-z0-9]/.test(afterCh);
    if (before && after) {
      return true;
    }
    search = idx + 1;
  }
  return false;
}

function isDealerishToken(tok) {
  const l = String(tok || "").toLowerCase();
  return l === "carmax" || l === "carvana" || l === "vroom" || l.indexOf("auto") !== -1 || l.indexOf("motor") !== -1;
}

function cardDistance(text) {
  const paren = text.match(/\(\s*\d{1,3}\s*(mi|miles)\)/i);
  if (paren) {
    const at = text.indexOf(paren[0]);
    const before = text.slice(0, at);
    const city = before.match(/([A-Za-z][A-Za-z0-9]*)\s*,\s*([A-Za-z]{2})\s*$/);
    if (city) {
      return cap((city[0] + paren[0]).replace(/\s+/g, " ").trim(), CARD_DISTANCE_CAP);
    }
    return cap(paren[0].replace(/\s+/g, " ").trim(), CARD_DISTANCE_CAP);
  }
  const away = text.match(/\d[\d,]*\s*(mi|miles)\s+away\b/i);
  if (away) {
    return cap(away[0].replace(/\s+/g, " ").trim(), CARD_DISTANCE_CAP);
  }
  const ship = text.match(/shipping from\b[^.\n]*/i);
  if (ship) {
    return cap(ship[0].replace(/\s+/g, " ").trim(), CARD_DISTANCE_CAP);
  }
  return "";
}

function cardDelivery(text) {
  const avail = text.match(/available for delivery to\b[^.\n]*/i);
  if (avail) {
    return cap(avail[0].replace(/\s+/g, " ").trim(), CARD_DELIVERY_CAP);
  }
  const est = text.match(/est\.?\s*shipping\b[^.\n]*/i);
  if (est) {
    return cap(est[0].replace(/\s+/g, " ").trim(), CARD_DELIVERY_CAP);
  }
  const shipFee = text.match(/shipping\s+[$€£][\d,]+(?:\.\d{2})?/i);
  if (shipFee && !/^shipping from\b/i.test(shipFee[0])) {
    return cap(shipFee[0].replace(/\s+/g, " ").trim(), CARD_DELIVERY_CAP);
  }
  const deliver = text.match(/deliver to\s+\d{5}(?:-\d{4})?/i);
  if (deliver) {
    return cap(deliver[0].replace(/\s+/g, " ").trim(), CARD_DELIVERY_CAP);
  }
  return "";
}

function cardKind(text, card) {
  const lower = String(text || "").toLowerCase();
  if (
    lower.indexOf("you may also like") !== -1 ||
    lower.indexOf("outside your search") !== -1 ||
    lower.indexOf("outside your area") !== -1 ||
    junkHasPhrase(lower, "recommended")
  ) {
    return cap("recommended", CARD_KIND_CAP);
  }
  const delivery = (card && card.delivery) || "";
  const distance = (card && card.distance) || "";
  if (
    /shipping from\b/i.test(distance) ||
    /shipping from\b/i.test(delivery) ||
    /shipping from\b/i.test(text) ||
    /shipping\s+[$€£]/i.test(text) ||
    /shipping\s+[$€£]/i.test(delivery) ||
    /deliver to\b/i.test(text) ||
    /deliver to\b/i.test(delivery)
  ) {
    return cap("ship", CARD_KIND_CAP);
  }
  if (/\bmi\s+away\b/i.test(text) || /\bmiles\s+away\b/i.test(text) || /\(\s*\d{1,3}\s*(mi|miles)\)/i.test(text)) {
    return cap("local", CARD_KIND_CAP);
  }
  if (/\bmi\s+away\b/i.test(distance) || /\(\s*\d{1,3}\s*(mi|miles)\)/i.test(distance)) {
    return cap("local", CARD_KIND_CAP);
  }
  return "";
}

function cardOf(text) {
  const m = text.match(/\b(\d+)\s+of\s+(\d+)\b/i);
  return m ? cap(m[1] + " of " + m[2], CARD_OF_CAP) : "";
}

function collectListingMeta() {
  const heading = document.querySelector("h1,h2,[role=\"heading\"]");
  const headingText = heading
    ? (heading.innerText || "").replace(/\s+/g, " ").trim()
    : "";
  const text = (headingText + "\n" + mainText()).trim();
  const meta = {};
  let zip = "";
  let radius = "";
  const params = new URLSearchParams(location.search || "");
  zip = (params.get("zip") || "").trim();
  const maxDist = (params.get("maximum_distance") || "").trim();
  if (/^\d+$/.test(maxDist)) {
    radius = maxDist + " mi";
  }
  const within = text.match(
    /within\s+(\d[\d,]*)\s*(mi|miles)\s+of\s+(\d{5}(?:-\d{4})?)/i
  );
  if (within) {
    if (!radius) {
      radius = within[1] + " mi";
    }
    if (!zip) {
      zip = within[3];
    }
  }
  if (!radius && /^all$/i.test(maxDist)) {
    radius = "all";
  }
  if (zip) {
    meta.zip = cap(zip, ZIP_CAP);
  }
  if (radius) {
    meta.radius = cap(radius, RADIUS_CAP);
  }
  const countSrc = headingText || text;
  const count = parseResultCount(countSrc);
  if (count) {
    meta.result_count = count;
  }
  const local = parseLocalMatches(text);
  if (local && local !== count) {
    meta.local_matches = local;
  }
  const empty = parseEmptyState(text);
  if (empty) {
    meta.empty_state = empty;
  }
  return meta;
}

function parseResultCount(text) {
  const m = text.match(/(\d[\d,]*)\+?\s+(matches|cars|results)\b/i);
  if (!m) {
    return "";
  }
  return cap(m[0].replace(/\s+/g, " ").trim(), RESULT_COUNT_CAP);
}

function parseLocalMatches(text) {
  const m = text.match(/\b(\d+)\s+local\b/i);
  return m ? cap(m[1] + " local", LOCAL_MATCHES_CAP) : "";
}

function parseEmptyState(text) {
  const phrases = [
    "nothing fits those filters",
    "we couldn't find",
    "we couldnt find",
    "we couldn\u2019t find",
    "0 matches",
    "no cars match",
    "no results",
    "try a larger radius",
    "expand your search",
  ];
  const lower = text.toLowerCase();
  for (let i = 0; i < phrases.length; i += 1) {
    const idx = indexOfEmptyPhrase(lower, phrases[i]);
    if (idx !== -1) {
      let start = 0;
      for (let j = idx - 1; j >= 0; j -= 1) {
        if (".!?\n\r".indexOf(text.charAt(j)) !== -1) {
          start = j + 1;
          break;
        }
      }
      const afterPhrase = idx + phrases[i].length;
      let end = text.length;
      for (let j = afterPhrase; j < text.length; j += 1) {
        if (".!?\n\r".indexOf(text.charAt(j)) !== -1) {
          end = j + 1;
          break;
        }
      }
      const sentence = text.slice(start, end).trim();
      return sentence ? cap(sentence, EMPTY_STATE_CAP) : "";
    }
  }
  return "";
}

function indexOfEmptyPhrase(lower, phrase) {
  let search = 0;
  while (search <= lower.length) {
    const idx = lower.indexOf(phrase, search);
    if (idx === -1) {
      return -1;
    }
    const before = idx === 0 || !/\d/.test(lower.charAt(idx - 1));
    const afterCh = lower.charAt(idx + phrase.length);
    const after = !afterCh || !/[A-Za-z0-9]/.test(afterCh);
    if (before && after) {
      return idx;
    }
    search = idx + 1;
  }
  return -1;
}

function cardHref(el) {
  if (el.tagName === "A") {
    const href = el.getAttribute("href");
    if (href && !isJsHref(href)) {
      return el.href || href;
    }
    return "";
  }
  const a = el.querySelector("a[href]");
  if (a) {
    const href = a.getAttribute("href");
    if (href && !isJsHref(href)) {
      return a.href || href;
    }
  }
  return "";
}

function extractPrice(text) {
  const skipTok = { est: 1, "est.": 1, drop: 1, save: 1, off: 1, discount: 1 };
  const hits = [];
  const re = /[$€£][\d,]+(?:\.\d{2})?/g;
  let m = re.exec(text);
  while (m) {
    const start = m.index;
    const end = start + m[0].length;
    const after = text.slice(end).replace(/^\s+/, "");
    let skip = false;
    if (after.indexOf("/mo") === 0 || /^\/\s*mo\b/i.test(after) || /^\bmo\b/i.test(after)) {
      skip = true;
    }
    const prev = prevWholeToken(text, start);
    const next1 = nextWholeToken(text, end);
    const next2 = next1 ? nextWholeToken(text, next1.end) : null;
    if (prev && skipTok[prev.toLowerCase()]) {
      skip = true;
    }
    if (next1 && skipTok[next1.tok.toLowerCase()]) {
      skip = true;
    }
    if (next2 && skipTok[next2.tok.toLowerCase()]) {
      skip = true;
    }
    if (!skip) {
      hits.push({ tok: m[0], grouped: m[0].indexOf(",") !== -1 });
    }
    m = re.exec(text);
  }
  for (let i = 0; i < hits.length; i += 1) {
    if (hits[i].grouped) {
      return hits[i].tok;
    }
  }
  if (hits.length) {
    return hits[0].tok;
  }
  const dec = text.match(/\d[\d,]*\.\d{2}/);
  return dec ? dec[0] : "";
}

function prevWholeToken(text, pos) {
  let i = pos;
  while (i > 0 && /\s/.test(text.charAt(i - 1))) {
    i -= 1;
  }
  if (i === 0) {
    return "";
  }
  if (text.charAt(i - 1) === ".") {
    let s = i - 1;
    while (s > 0 && /[A-Za-z0-9]/.test(text.charAt(s - 1))) {
      s -= 1;
    }
    return text.slice(s, i);
  }
  let s = i;
  while (s > 0 && /[A-Za-z0-9]/.test(text.charAt(s - 1))) {
    s -= 1;
  }
  return text.slice(s, i);
}

function nextWholeToken(text, pos) {
  let i = pos;
  while (i < text.length && /\s/.test(text.charAt(i))) {
    i += 1;
  }
  if (i >= text.length || !/[A-Za-z0-9]/.test(text.charAt(i))) {
    return null;
  }
  let end = i;
  while (end < text.length && /[A-Za-z0-9]/.test(text.charAt(end))) {
    end += 1;
  }
  if (text.charAt(end) === "." && text.slice(i, end + 1).toLowerCase() === "est.") {
    return { tok: text.slice(i, end + 1), end: end + 1 };
  }
  return { tok: text.slice(i, end), end: end };
}

function cardTitle(el, text, price) {
  const heading = el.querySelector("h1,h2,h3,h4,[role=\"heading\"]");
  if (heading) {
    const t = (heading.innerText || "").replace(/\s+/g, " ").trim();
    if (t) {
      return t;
    }
  }
  const aria = attr(el, "aria-label");
  if (aria) {
    return aria;
  }
  if (!text) {
    return "";
  }
  if (price && text.indexOf(price) !== -1) {
    return text.replace(price, "").replace(/\s+/g, " ").trim();
  }
  return text;
}

// ---------------------------------------------------------------- 0128 assessment
// Read-only. No synthetic input, no mutation, no timers.

const ASSESS_MIN_SAMPLES = 5;
const ASSESS_MAX_SAMPLES = 49;
const ASSESS_ANCESTORS = 64;
const ASSESS_BLOCKERS = 3;
const ASSESS_NOTES = 3;
const ASSESS_SHADOW_PATH = 8;

function clampSamples(n) {
  const v = Number(n);
  if (!Number.isFinite(v)) {
    return 25;
  }
  return Math.max(ASSESS_MIN_SAMPLES, Math.min(ASSESS_MAX_SAMPLES, Math.round(v)));
}

function assessViewport() {
  const de = document.documentElement;
  return {
    left: 0,
    top: 0,
    width: de ? de.clientWidth || window.innerWidth : window.innerWidth,
    height: de ? de.clientHeight || window.innerHeight : window.innerHeight,
  };
}

function rectArea(r) {
  if (!r) return 0;
  const w = Math.max(0, r.width || 0);
  const h = Math.max(0, r.height || 0);
  return w * h;
}

function intersectRects(a, b) {
  if (!a || !b) return null;
  const left = Math.max(a.left, b.left);
  const top = Math.max(a.top, b.top);
  const right = Math.min(a.left + a.width, b.left + b.width);
  const bottom = Math.min(a.top + a.height, b.top + b.height);
  if (right <= left || bottom <= top) {
    return { left: left, top: top, width: 0, height: 0 };
  }
  return { left: left, top: top, width: right - left, height: bottom - top };
}

function pointInRect(p, r) {
  return (
    !!r &&
    p.x >= r.left &&
    p.x < r.left + r.width &&
    p.y >= r.top &&
    p.y < r.top + r.height
  );
}

function cssRectOf(el) {
  const r = el.getBoundingClientRect();
  if (!r || r.width <= 0 || r.height <= 0) {
    return null;
  }
  return { left: r.left, top: r.top, width: r.width, height: r.height };
}

function parentOf(node) {
  if (!node) return null;
  if (node.parentElement) return node.parentElement;
  const root = node.getRootNode ? node.getRootNode() : null;
  return root && root.host ? root.host : null;
}

function ancestorsOf(el) {
  const out = [];
  let node = parentOf(el);
  while (node && out.length < ASSESS_ANCESTORS) {
    out.push(node);
    node = parentOf(node);
  }
  return out;
}

function establishesFixedContainingBlock(style) {
  if (!style) return false;
  if (style.transform && style.transform !== "none") return true;
  if (style.perspective && style.perspective !== "none") return true;
  if (style.filter && style.filter !== "none") return true;
  if (style.backdropFilter && style.backdropFilter !== "none") return true;
  if (style.translate && style.translate !== "none") return true;
  if (style.scale && style.scale !== "none") return true;
  if (style.rotate && style.rotate !== "none") return true;
  const contain = String(style.contain || "");
  if (/(paint|layout|strict|content)/.test(contain)) return true;
  const will = String(style.willChange || "");
  return /(transform|perspective|filter|translate|scale|rotate)/.test(will);
}

function clipsOverflow(el) {
  const style = window.getComputedStyle(el);
  if (!style) return false;
  const display = String(style.display || "");
  if (display === "inline" || display === "contents") return false;
  const values = [style.overflowX, style.overflowY, style.overflow];
  for (let i = 0; i < values.length; i += 1) {
    if (/(hidden|clip|scroll|auto)/.test(String(values[i] || ""))) {
      return true;
    }
  }
  return false;
}

function hasOverflowClip(el) {
  const style = window.getComputedStyle(el);
  if (!style) return false;
  return /(hidden|clip|scroll|auto)/.test(String(style.overflowX || style.overflow || ""));
}

function paddingBox(el) {
  const rect = el.getBoundingClientRect();
  const style = window.getComputedStyle(el);
  if (!rect || !style) return null;
  const pl = parseFloat(style.borderLeftWidth) || 0;
  const pt = parseFloat(style.borderTopWidth) || 0;
  const pr = parseFloat(style.borderRightWidth) || 0;
  const pb = parseFloat(style.borderBottomWidth) || 0;
  const width = rect.width - pl - pr;
  const height = rect.height - pt - pb;
  if (width <= 0 || height <= 0) return null;
  return {
    left: rect.left + pl,
    top: rect.top + pt,
    width: width,
    height: height,
  };
}

function clipRectsFor(target) {
  const ancestors = ancestorsOf(target);
  const style = window.getComputedStyle(target);
  const position = style ? String(style.position || "static") : "static";
  let limit = ancestors.length;
  if (position === "fixed") {
    for (let i = 0; i < ancestors.length; i += 1) {
      if (establishesFixedContainingBlock(window.getComputedStyle(ancestors[i]))) {
        limit = i;
        break;
      }
    }
  } else if (position === "absolute") {
    for (let i = 0; i < ancestors.length; i += 1) {
      const s = window.getComputedStyle(ancestors[i]);
      if ((s && s.position && s.position !== "static") || establishesFixedContainingBlock(s)) {
        limit = i;
        break;
      }
    }
  } else {
    limit = 0;
  }
  const rects = [];
  const elements = [];
  const notes = [];
  for (let i = 0; i < ancestors.length; i += 1) {
    const el = ancestors[i];
    const tag = String(el.tagName || "").toLowerCase();
    if (tag === "html" || tag === "body") {
      if (hasOverflowClip(el)) {
        notes.push("containing_block_skip");
      }
      continue;
    }
    if (i < limit) {
      continue;
    }
    if (!clipsOverflow(el)) {
      continue;
    }
    const box = paddingBox(el);
    if (box) {
      rects.push(box);
      elements.push(el);
    }
  }
  return { rects: rects, elements: elements, notes: notes };
}

function samplePoints(rect, samples) {
  const n = clampSamples(samples);
  const side = Math.ceil(Math.sqrt(n));
  const insetX = rect.width > 2 ? Math.min(1, rect.width / 4) : 0;
  const insetY = rect.height > 2 ? Math.min(1, rect.height / 4) : 0;
  const innerLeft = rect.left + insetX;
  const innerTop = rect.top + insetY;
  const innerWidth = Math.max(0, rect.width - insetX * 2);
  const innerHeight = Math.max(0, rect.height - insetY * 2);
  const out = [];
  for (let row = 0; row < side && out.length < n; row += 1) {
    for (let col = 0; col < side && out.length < n; col += 1) {
      out.push({
        x: innerLeft + (innerWidth * (col + 0.5)) / side,
        y: innerTop + (innerHeight * (row + 0.5)) / side,
      });
    }
  }
  return out;
}

function composedContains(target, hit) {
  let node = hit;
  let depth = 0;
  while (node && depth < ASSESS_ANCESTORS) {
    if (node === target) return true;
    node = parentOf(node);
    depth += 1;
  }
  return false;
}

function pierce(x, y) {
  const out = {
    el: null,
    shadowPath: [],
    notes: [],
    iframe: false,
    shadowUninspectable: false,
    shadowPointerEventsNone: false,
  };
  let el = document.elementFromPoint(x, y);
  if (!el) return out;
  for (let depth = 0; depth < ASSESS_SHADOW_PATH; depth += 1) {
    out.el = el;
    if (el.tagName === "IFRAME" || el.tagName === "FRAME") {
      out.iframe = true;
      return out;
    }
    let root = null;
    let closed = false;
    try {
      root = el.shadowRoot || null;
      if (!root && chrome.dom && chrome.dom.openOrClosedShadowRoot) {
        root = chrome.dom.openOrClosedShadowRoot(el);
        closed = !!root;
      }
    } catch (_err) {
      root = null;
    }
    if (!root) {
      return out;
    }
    if (typeof root.elementFromPoint !== "function") {
      out.shadowUninspectable = true;
      return out;
    }
    let inner = null;
    try {
      inner = root.elementFromPoint(x, y);
    } catch (_err) {
      inner = null;
    }
    if (!inner || inner === el) {
      return out;
    }
    const innerStyle = window.getComputedStyle(inner);
    if (innerStyle && innerStyle.pointerEvents === "none") {
      // `ShadowRoot.elementFromPoint` ignores pointer-events; do not accept a shadow inner
      // hit that cannot receive the press. Report unknown rather than a false negative.
      out.shadowPointerEventsNone = true;
      out.el = el;
      return out;
    }
    if (out.shadowPath.length < ASSESS_SHADOW_PATH) {
      out.shadowPath.push(cap(String(el.tagName || "").toLowerCase(), 16));
    }
    if (closed && out.notes.indexOf("shadow_pierced_closed") === -1) {
      out.notes.push("shadow_pierced_closed");
    }
    el = inner;
  }
  if (out.shadowPath.length >= ASSESS_SHADOW_PATH) {
    out.shadowUninspectable = true;
  }
  return out;
}

function describeElement(el, reason) {
  if (!el || el.nodeType !== 1) return null;
  const style = window.getComputedStyle(el);
  if (style && style.pointerEvents === "none") {
    return null;
  }
  const tag = String(el.tagName || "").toLowerCase();
  const role = attr(el, "role");
  const name = attr(el, "aria-label") || (el.innerText || "").replace(/\s+/g, " ").trim();
  const out = {
    kind: tag === "iframe" ? "iframe" : "element",
    tag: cap(tag, 16),
  };
  if (role) out.role = cap(role, 32);
  if (name) out.name = cap(name, 80);
  const rect = cssRectOf(el);
  if (rect) out.rect_css = rect;
  if (reason) out.reason = reason;
  return out;
}

function describeBlockers(misses) {
  const counts = {};
  const rows = {};
  for (let i = 0; i < misses.length; i += 1) {
    const miss = misses[i];
    let blocker = null;
    if (!miss || !miss.el) {
      blocker = { kind: "viewport", reason: "outside_viewport" };
    } else if (miss.iframe) {
      const named = describeElement(miss.el, "cross_frame");
      blocker = named || { kind: "iframe", reason: "cross_frame" };
      blocker.kind = "iframe";
      blocker.reason = "cross_frame";
    } else if (miss.shadowUninspectable) {
      blocker = { kind: "shadow", tag: cap(String(miss.el.tagName || "").toLowerCase(), 16), reason: "shadow_closed" };
    } else {
      const tag = String(miss.el.tagName || "").toLowerCase();
      if (tag === "html" || tag === "body") {
        blocker = { kind: "background", tag: cap(tag, 16), reason: "not_target" };
      } else {
        if (miss.shadowPath && miss.shadowPath.length > 0) {
          blocker = describeElement(miss.el, "overlay");
        }
        let candidates = [];
        if (!blocker) {
          try {
            candidates = document.elementsFromPoint(miss.x, miss.y) || [];
          } catch (_err) {
            candidates = [];
          }
          const top = candidates.slice(0, 3);
          for (let j = 0; j < top.length; j += 1) {
            const named = describeElement(top[j], "overlay");
            if (named) {
              blocker = named;
              break;
            }
          }
        }
        if (!blocker) {
          blocker = describeElement(miss.el, "overlay");
        }
        if (!blocker) {
          blocker = { kind: "element", tag: cap(tag, 16), reason: "not_target" };
        }
      }
    }
    const key = [
      blocker.kind || "",
      blocker.tag || "",
      blocker.role || "",
      blocker.rect_css ? Math.round(blocker.rect_css.left) + ":" + Math.round(blocker.rect_css.top) : "",
    ].join("|");
    rows[key] = blocker;
    counts[key] = (counts[key] || 0) + 1;
  }
  const keys = Object.keys(rows).sort((a, b) => counts[b] - counts[a]);
  return keys.slice(0, ASSESS_BLOCKERS).map((k) => rows[k]);
}

function isRendered(el) {
  if (!el) return false;
  if (typeof el.checkVisibility === "function") {
    const visible = el.checkVisibility({
      contentVisibilityAuto: true,
      opacityProperty: false,
    });
    if (visible === false) return false;
  }
  const style = window.getComputedStyle(el);
  if (!style) return true;
  return style.display !== "none" && style.visibility !== "hidden";
}

function notesFor(target, clipNotes, center) {
  const out = [];
  const push = (token) => {
    if (token && out.indexOf(token) === -1 && out.length < ASSESS_NOTES) {
      out.push(token);
    }
  };
  if (
    (target.getAttribute && target.getAttribute("aria-disabled") === "true") ||
    (target.closest && target.closest('[aria-disabled="true"]'))
  ) {
    push("aria_disabled");
  }
  if (
    (target.hasAttribute && target.hasAttribute("inert")) ||
    (target.closest && target.closest("[inert]"))
  ) {
    push("inert");
  }
  for (let i = 0; i < clipNotes.length; i += 1) {
    push(clipNotes[i]);
  }
  for (let i = 0; i < center.notes.length; i += 1) {
    push(center.notes[i]);
  }
  const style = window.getComputedStyle(target);
  if (style) {
    if (style.clipPath && style.clipPath !== "none") push("clip_path");
    if (style.maskImage && style.maskImage !== "none") push("mask");
    if (style.filter && style.filter !== "none") push("filter");
  }
  return out;
}

function confidenceFor(verdict, samples, samplesHit, area, notes, center) {
  if (verdict === null) {
    return "none";
  }
  if (samples < 9 || center.shadowUninspectable) {
    return "low";
  }
  const caveat = notes.length > 0;
  const allAgree =
    verdict === true ? samplesHit === samples : samplesHit === 0;
  if (samples >= 25 && allAgree && area >= 100 && !caveat) {
    return "high";
  }
  return "medium";
}

function clippedByFor(candidate, targetRect, clip, viewport) {
  if (!candidate) return null;
  const shrunk =
    candidate.width + 0.5 < targetRect.width ||
    candidate.height + 0.5 < targetRect.height;
  if (!shrunk) return null;
  for (let i = 0; i < clip.elements.length; i += 1) {
    const box = clip.rects[i];
    if (!box) continue;
    const edgeMatches =
      Math.abs(box.left - candidate.left) <= 0.5 ||
      Math.abs(box.top - candidate.top) <= 0.5 ||
      Math.abs(box.left + box.width - (candidate.left + candidate.width)) <= 0.5 ||
      Math.abs(box.top + box.height - (candidate.top + candidate.height)) <= 0.5;
    if (edgeMatches) {
      const named = describeElement(clip.elements[i], "clipped");
      if (named) return named;
    }
  }
  if (
    Math.abs(viewport.left - candidate.left) <= 0.5 ||
    Math.abs(viewport.top - candidate.top) <= 0.5 ||
    Math.abs(viewport.left + viewport.width - (candidate.left + candidate.width)) <= 0.5 ||
    Math.abs(viewport.top + viewport.height - (candidate.top + candidate.height)) <= 0.5
  ) {
    return { kind: "viewport", reason: "clipped" };
  }
  return { kind: "viewport", reason: "clipped" };
}

function assessAt(target, rectCss, samples) {
  const started = performance.now();
  const startSamples = clampSamples(samples);
  const targetRect = rectCss;
  const viewport = assessViewport();
  const clip = clipRectsFor(target);
  let candidate = intersectRects(targetRect, viewport);
  for (let i = 0; i < clip.rects.length; i += 1) {
    candidate = intersectRects(candidate, clip.rects[i]);
  }
  const area = rectArea(targetRect);
  const onscreen =
    area > 0 ? rectArea(intersectRects(targetRect, viewport)) / area : null;
  const selected = {
    x: targetRect.left + targetRect.width / 2,
    y: targetRect.top + targetRect.height / 2,
  };
  const points = samplePoints(targetRect, startSamples);
  let samplesHit = 0;
  const misses = [];
  for (let i = 0; i < points.length; i += 1) {
    const p = points[i];
    if (!pointInRect(p, viewport)) {
      misses.push(null);
      continue;
    }
    const hit = pierce(p.x, p.y);
    if (hit.el && !hit.iframe && !hit.shadowUninspectable && composedContains(target, hit.el)) {
      samplesHit += 1;
    } else {
      misses.push({
        el: hit.el,
        x: p.x,
        y: p.y,
        iframe: hit.iframe,
        shadowUninspectable: hit.shadowUninspectable,
        shadowPointerEventsNone: hit.shadowPointerEventsNone,
        shadowPath: hit.shadowPath,
      });
    }
  }
  const visibleBase = points.length > 0 ? samplesHit / points.length : null;
  const visible =
    visibleBase != null && onscreen != null
      ? Math.min(visibleBase, onscreen)
      : visibleBase;
  const center = pierce(selected.x, selected.y);
  const notes = notesFor(target, clip.notes, center);
  let interactable = null;
  let reason = null;
  if (!pointInRect(selected, viewport)) {
    interactable = false;
    reason = "center_outside_viewport";
  } else if (center.el && !center.iframe && !center.shadowUninspectable && composedContains(target, center.el)) {
    interactable = true;
    const style = window.getComputedStyle(target);
    if (style && style.pointerEvents === "none") {
      const inner = describeElement(center.el, null);
      if (inner && center.el !== target) {
        notes.push("pointer_events_none");
      }
    }
  } else if (center.shadowPointerEventsNone) {
    // The document-level hit is the shadow host; a pointer-events:none inner cannot
    // receive the press. Unknown rather than a false negative.
    interactable = null;
    reason = "shadow_pierce_unavailable";
  } else {
    interactable = false;
    if (!isRendered(target)) {
      reason = "not_rendered";
    } else {
      const style = window.getComputedStyle(target);
      if (style && style.pointerEvents === "none") {
        reason = "pointer_events_none";
      } else if (!pointInRect(selected, candidate)) {
        reason = "clipped_out";
      } else if (center.shadowUninspectable) {
        reason = "shadow_closed";
      } else if (center.iframe) {
        reason = "cross_frame";
      } else {
        reason = "blocked";
      }
    }
  }
  const blocked = describeBlockers(misses);
  const clipped = clippedByFor(candidate, targetRect, clip, viewport);
  const confidence = confidenceFor(
    interactable,
    points.length,
    samplesHit,
    area,
    notes,
    center
  );
  return {
    interactable: interactable,
    selected_point: selected,
    target_rect_css: {
      left: targetRect.left,
      top: targetRect.top,
      width: targetRect.width,
      height: targetRect.height,
    },
    visible_fraction: visible,
    onscreen_fraction: onscreen,
    samples: points.length,
    samples_hit: samplesHit,
    blocked_by: blocked,
    clipped_by: clipped,
    notes: notes.slice(0, ASSESS_NOTES),
    reason: reason,
    confidence: confidence,
    elapsed_ms: Math.max(0, performance.now() - started),
  };
}

function isInteractiveHit(el) {
  if (!el || el.nodeType !== 1) return false;
  const tag = String(el.tagName || "").toLowerCase();
  if (
    ["a", "button", "input", "select", "textarea", "summary", "label", "option"].indexOf(tag) !==
    -1
  ) {
    return true;
  }
  const role = (el.getAttribute("role") || "").toLowerCase();
  if (
    [
      "button",
      "link",
      "checkbox",
      "radio",
      "tab",
      "menuitem",
      "menuitemcheckbox",
      "menuitemradio",
      "option",
      "textbox",
      "combobox",
      "treeitem",
      "slider",
      "switch",
    ].indexOf(role) !== -1
  ) {
    return true;
  }
  if (el.hasAttribute("tabindex") && el.getAttribute("tabindex") !== "-1") {
    return true;
  }
  return !!el.isContentEditable;
}

function buildAssess(msg) {
  const started = performance.now();
  const metrics = windowMetrics();
  const css = msg && msg.css ? { x: Number(msg.css.x), y: Number(msg.css.y) } : null;
  const base = {
    url: location.href || "",
    title: document.title || "",
    metrics: metrics,
  };
  if (SELF_FRAME != null && SELF_FRAME !== 0) {
    base.frame = SELF_FRAME;
  }
  if (!css || !Number.isFinite(css.x) || !Number.isFinite(css.y)) {
    base.css = null;
    base.at_point = unknownAtPoint("point_mismatch");
    return base;
  }
  base.css = css;
  const viewport = assessViewport();
  if (!pointInRect(css, viewport)) {
    base.at_point = unknownAtPoint("outside_viewport");
    return base;
  }
  const hit = pierce(css.x, css.y);
  if (hit.shadowUninspectable) {
    base.at_point = unknownAtPoint("shadow_closed", hit);
    return base;
  }
  if (hit.iframe) {
    base.at_point = unknownAtPoint("cross_frame", hit);
    base.iframe_slot = frameSlotOf(hit.el);
    const box = frameContentBox(hit.el);
    if (box && box.width > 0 && box.height > 0) {
      base.local_css = { x: css.x - box.left, y: css.y - box.top };
    }
    return base;
  }
  if (!hit.el) {
    base.at_point = unknownAtPoint("outside_viewport", hit);
    return base;
  }
  if (!isInteractiveHit(hit.el)) {
    base.at_point = unknownAtPoint("not_interactive", hit);
    return base;
  }
  const element = describeElement(hit.el, null);
  base.at_point = {
    element: element || undefined,
    shadow_path: hit.shadowPath.slice(0, ASSESS_SHADOW_PATH),
    interactable: true,
    reason: null,
    notes: hit.notes.slice(0, ASSESS_NOTES),
    confidence: hit.shadowPath.length > 0 ? "medium" : "high",
    elapsed_ms: Math.max(0, performance.now() - started),
  };
  return base;
}

function unknownAtPoint(reason, hit) {
  return {
    shadow_path: hit ? hit.shadowPath.slice(0, ASSESS_SHADOW_PATH) : [],
    interactable: null,
    reason: reason,
    notes: hit ? hit.notes.slice(0, ASSESS_NOTES) : [],
    confidence: "none",
  };
}

function cap(s, n) {
  const t = String(s || "");
  if (t.length <= n) {
    return t;
  }
  return t.slice(0, n);
}

// A page can flood `message`; every event is shape/source checked and page data is never logged.
if (window.addEventListener) {
  window.addEventListener("message", onFrameMessage);
}
connectFramePort();
