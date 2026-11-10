const HOST_NAME = "com.helpinghands.host";
const FRAME_PORT_NAME = "hands-frame";
const FRAME_PORT_CAP = 32;
const FRAME_ROW_CAP = 12; // including frame 0
const ANNOUNCE_STEP_MS = 40;
const LINK_SETTLE_MS = 40;
const MEASURE_TIMEOUT_MS = 100;
const CHILD_SNAPSHOT_TIMEOUT_MS = 70;
const FANOUT_BUDGET_MS = 250;
const CLIENT_BUDGET_MS = 400;
const FANOUT_MARGIN_MS = 40;
const MIN_FANOUT_MS = 60;
const CHAIN_BUDGET_MS = 250;
const FRAME_ELEMENT_CAP = 8;

let hostPort = null;
let reconnectTimer = null;

// Per-tab frame registry: tab id -> Map(frameId -> entry).
// Entry: {tab, frameId, documentId, documentLifecycle, origin, url, port, token,
//         doc, link, linkState, linkReason, parent, announced, late}
const frameRegistry = new Map();
// Rejected connected ports per tab: a frame-less `frame_cap_exceeded` row must still appear.
const rejectedPorts = new Map();
// Token -> entry. Tokens are issued over a private port and never broadcast.
const tokenIndex = new Map();
// Token -> pending measurement callbacks (fresh frame_measure).
const measureWaiters = new Map();

function connectHost() {
  if (hostPort) {
    return;
  }
  let port;
  try {
    port = chrome.runtime.connectNative(HOST_NAME);
  } catch (_err) {
    scheduleReconnect();
    return;
  }
  hostPort = port;
  port.onMessage.addListener((msg) => {
    forward(port, msg);
  });
  port.onDisconnect.addListener(() => {
    // Must read lastError or Chrome logs Unchecked runtime.lastError:
    // "Native host has exited" (host gone / FIRST_PIPE_INSTANCE race).
    void chrome.runtime.lastError;
    hostPort = null;
    scheduleReconnect();
  });
}

function scheduleReconnect() {
  if (reconnectTimer != null) {
    return;
  }
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectHost();
  }, 1000);
}

function post(port, message) {
  try {
    port.postMessage(message);
  } catch (_err) {}
}

function copyReply(reply) {
  const out = {};
  for (const key in reply) {
    if (Object.prototype.hasOwnProperty.call(reply, key)) {
      out[key] = reply[key];
    }
  }
  return out;
}

// Every content-script message carries an explicit frame id. A bare broadcast is never sent.
function frameIdFor(msg) {
  const frame = Number(msg && msg.frame);
  if (Number.isInteger(frame) && frame > 0) {
    return frame;
  }
  return 0;
}

function b36(n) {
  return (Number(n) >>> 0).toString(36);
}

function sendToFrame(tabId, frameId, msg, callback) {
  chrome.tabs.sendMessage(tabId, msg, { frameId: frameId }, callback);
}

function withTabEvidence(port, tab, fallback, msg, enrich) {
  chrome.windows.get(tab.windowId, (win) => {
    if (chrome.runtime.lastError || !win) {
      post(port, { error: "window_unreadable", tab: tab.id });
      return;
    }
    const evidence = {
      tab: tab.id,
      window: tab.windowId,
      active: !!tab.active,
      focused: !!win.focused,
      fallback: fallback,
    };
    sendToFrame(tab.id, frameIdFor(msg), msg, (reply) => {
      if (chrome.runtime.lastError) {
        post(port, Object.assign({ error: "no_content" }, evidence));
        return;
      }
      if (!reply) {
        post(port, Object.assign({ error: "empty" }, evidence));
        return;
      }
      const out = copyReply(reply);
      Object.assign(out, evidence);
      if (enrich) {
        enrich(out, tab, port, msg);
        return;
      }
      const frame = frameIdFor(msg);
      if ((msg.op === "resolve" || msg.op === "assess") && frame > 0) {
        enrichFrameReply(tab, frame, out, port);
        return;
      }
      if (msg.op === "assess" && frame === 0 && Number.isInteger(reply.iframe_slot)) {
        // A top point-assess hit an iframe. Name the owning frame so Rust can issue the
        // explicit `{tab, frameId}` assess instead of leaving `cross_frame` forever.
        const entry = frameBySlot(tab.id, reply.iframe_slot);
        if (entry) {
          out.iframe_frame = entry.frameId;
        }
      }
      post(port, out);
    });
  });
}

// ------------------------------------------------------------------ frame ports

function randomToken() {
  try {
    const buf = new Uint8Array(16);
    crypto.getRandomValues(buf);
    let out = "";
    for (let i = 0; i < buf.length; i += 1) {
      out += buf[i].toString(16).padStart(2, "0");
    }
    return out;
  } catch (_err) {
    let out = "";
    for (let i = 0; i < 8; i += 1) {
      out += Math.floor(Math.random() * 0x10000)
        .toString(16)
        .padStart(4, "0");
    }
    return out;
  }
}

function frameTable(tabId) {
  let frames = frameRegistry.get(tabId);
  if (!frames) {
    frames = new Map();
    frameRegistry.set(tabId, frames);
  }
  return frames;
}

function revokeEntry(entry) {
  if (entry && entry.token) {
    tokenIndex.delete(entry.token);
  }
}

function pruneTab(tabId) {
  rejectedPorts.delete(tabId);
  const frames = frameRegistry.get(tabId);
  if (!frames) {
    return;
  }
  for (const entry of frames.values()) {
    revokeEntry(entry);
  }
  frameRegistry.delete(tabId);
}

function registerFramePort(port) {
  const sender = port.sender || {};
  const tab = sender.tab;
  const frameId = sender.frameId;
  if (!tab || tab.id == null || !Number.isInteger(frameId) || frameId < 0) {
    return;
  }
  const tabId = tab.id;
  const frames = frameTable(tabId);
  const existing = frames.get(frameId);
  if (!existing && frames.size >= FRAME_PORT_CAP) {
    rejectedPorts.set(tabId, (rejectedPorts.get(tabId) || 0) + 1);
    post(port, { op: "frame_reject", error: "frame_cap_exceeded" });
    try {
      port.disconnect();
    } catch (_err) {}
    return;
  }
  const token = randomToken();
  const entry = {
    tab: tabId,
    frameId: frameId,
    documentId: sender.documentId || null,
    documentLifecycle: sender.documentLifecycle || null,
    origin: sender.origin || "",
    url: sender.url || "",
    port: port,
    token: token,
    doc: null,
    link: null,
    linkState: "none",
    linkReason: null,
    parent: null,
    announced: false,
    late: false,
  };
  // Install the replacement before disconnecting the old port: the old port's onDisconnect
  // must not prune the entry that just replaced it.
  frames.set(frameId, entry);
  tokenIndex.set(entry.token, entry);
  if (existing) {
    // Duplicate frame id: last registration wins, and the old token is revoked.
    console.warn("hands: duplicate frame port replaced frame", frameId);
    revokeEntry(existing);
    try {
      existing.port.disconnect();
    } catch (_err) {}
  }
  port.onMessage.addListener((msg) => {
    const op = msg && msg.op;
    if (op === "frame_hello") {
      entry.doc = typeof msg.doc === "string" ? msg.doc : entry.doc;
      return;
    }
    if (op === "frame_announce_ack") {
      if (msg.announced === true) {
        entry.announced = true;
        entry.late = false;
      }
      return;
    }
    if (op === "frame_slot") {
      const token = typeof msg.token === "string" ? msg.token : "";
      const target = tokenIndex.get(token);
      if (
        target &&
        target.tab === entry.tab &&
        target.frameId !== entry.frameId &&
        Number.isInteger(msg.slot) &&
        msg.slot >= 0
      ) {
        const origin = normalizeOrigin(msg.origin);
        const expected = normalizeOrigin(target.origin);
        if (
          origin &&
          expected &&
          origin === expected &&
          (target.slotParent == null || target.slotParent === entry.frameId) &&
          (target.parent == null || target.parent === entry.frameId)
        ) {
          target.slot = msg.slot;
          // A slot is an index in the reporting parent's iframe list; record the owner.
          target.slotParent = entry.frameId;
        }
      }
      return;
    }
    if (op === "frame_link") {
      acceptFrameLink(entry, msg);
    }
  });
  port.onDisconnect.addListener(() => {
    if (frames.get(frameId) === entry) {
      // Children whose only chain level was this frame can no longer compose; mark them
      // stale so an observe does not keep advertising an `ok` link the click cannot use.
      for (const child of frames.values()) {
        if (child.link && child.link.parent === frameId) {
          child.link = null;
          child.linkState = "stale";
          child.linkReason = "frame_geometry_unavailable";
        }
      }
      frames.delete(frameId);
      revokeEntry(entry);
      if (!frames.size) {
        frameRegistry.delete(tabId);
      }
    }
  });
  post(port, { op: "frame_id", frame: frameId, token: entry.token });
}

function normalizeOrigin(value) {
  if (typeof value !== "string") {
    return null;
  }
  const origin = value.trim().toLowerCase();
  if (!origin || origin === "null" || origin.length > 80) {
    return null;
  }
  if (!/^[a-z][a-z0-9+.-]*:\/\/[^/\s?#]+$/.test(origin)) {
    return null;
  }
  return origin;
}

function sanitizeLink(raw) {
  if (!raw || typeof raw !== "object") {
    return null;
  }
  const numbers = [
    "content_left",
    "content_top",
    "content_width",
    "content_height",
    "viewport_w",
    "viewport_h",
  ];
  for (const key of numbers) {
    if (typeof raw[key] !== "number" || !Number.isFinite(raw[key])) {
      return null;
    }
  }
  if (raw.content_width < 0 || raw.content_height < 0) {
    return null;
  }
  if (raw.viewport_w < 1 || raw.viewport_h < 1) {
    return null;
  }
  const clip = [];
  const rawClip = Array.isArray(raw.clip) ? raw.clip : [];
  for (let i = 0; i < rawClip.length && i < 8; i += 1) {
    const rect = rawClip[i];
    if (
      !rect ||
      typeof rect.left !== "number" ||
      typeof rect.top !== "number" ||
      typeof rect.width !== "number" ||
      typeof rect.height !== "number"
    ) {
      return null;
    }
    if (
      !Number.isFinite(rect.left) ||
      !Number.isFinite(rect.top) ||
      !Number.isFinite(rect.width) ||
      !Number.isFinite(rect.height)
    ) {
      return null;
    }
    clip.push({
      left: rect.left,
      top: rect.top,
      width: rect.width,
      height: rect.height,
    });
  }
  return {
    parent: null,
    content_left: raw.content_left,
    content_top: raw.content_top,
    content_width: raw.content_width,
    content_height: raw.content_height,
    clip: clip,
    transform_ok: raw.transform_ok === true,
    visible: raw.visible === true,
    hit_ok: raw.hit_ok === true,
    viewport_w: raw.viewport_w,
    viewport_h: raw.viewport_h,
    onscreen: raw.onscreen === true,
    child_viewport_w:
      typeof raw.child_viewport_w === "number" && Number.isFinite(raw.child_viewport_w)
        ? raw.child_viewport_w
        : null,
    child_viewport_h:
      typeof raw.child_viewport_h === "number" && Number.isFinite(raw.child_viewport_h)
        ? raw.child_viewport_h
        : null,
    slot: Number.isInteger(raw.slot) ? raw.slot : null,
  };
}

function resolveMeasure(token, link) {
  const waiters = measureWaiters.get(token);
  if (!waiters) {
    return;
  }
  measureWaiters.delete(token);
  for (const callback of waiters) {
    callback(link);
  }
}

function waitMeasure(token, callback, timeoutMs) {
  let waiters = measureWaiters.get(token);
  if (!waiters) {
    waiters = [];
    measureWaiters.set(token, waiters);
  }
  waiters.push(callback);
  const timeout = Number.isFinite(timeoutMs)
    ? Math.max(1, Math.min(MEASURE_TIMEOUT_MS, timeoutMs))
    : MEASURE_TIMEOUT_MS;
  setTimeout(() => {
    const current = measureWaiters.get(token);
    if (!current) {
      return;
    }
    const index = current.indexOf(callback);
    if (index === -1) {
      return;
    }
    current.splice(index, 1);
    if (!current.length) {
      measureWaiters.delete(token);
    }
    callback(null);
  }, timeout);
}

function acceptFrameLink(parentEntry, msg) {
  const token = typeof msg.token === "string" ? msg.token : "";
  if (!token || token.length > 128) {
    return;
  }
  const target = tokenIndex.get(token);
  if (!target || target.tab !== parentEntry.tab) {
    return;
  }
  if (target.frameId === parentEntry.frameId) {
    return;
  }
  const origin = normalizeOrigin(msg.origin);
  const expected = normalizeOrigin(target.origin);
  if (!origin || !expected || origin !== expected) {
    if (!target.link) {
      target.linkState = "ambiguous";
      target.linkReason = "frame_link_ambiguous";
    }
    resolveMeasure(token, null);
    return;
  }
  if (target.parent != null && target.parent !== parentEntry.frameId) {
    // A second parent may not move an existing link; refuse without invalidating the first.
    if (!target.link) {
      target.linkState = "ambiguous";
      target.linkReason = "frame_link_ambiguous";
    }
    resolveMeasure(token, null);
    return;
  }
  // Slot identity is trusted only after the origin and single-parent guards pass. Record it
  // with its owning parent even if the link itself is later rejected.
  if (Number.isInteger(msg.slot) && msg.slot >= 0) {
    target.slot = msg.slot;
    target.slotParent = parentEntry.frameId;
  }
  const link = sanitizeLink(msg.link);
  if (!link) {
    if (!target.link) {
      target.linkState = "ambiguous";
      target.linkReason = "frame_link_ambiguous";
    }
    resolveMeasure(token, null);
    return;
  }
  link.parent = parentEntry.frameId;
  target.link = link;
  target.parent = parentEntry.frameId;
  target.linkState = "ok";
  target.linkReason = null;
  target.announced = true;
  resolveMeasure(token, link);
}

function announceAll(tabId, callback) {
  const frames = frameRegistry.get(tabId);
  const pending = [];
  if (frames) {
    for (const entry of frames.values()) {
      if (entry.frameId === 0 || !entry.port) {
        continue;
      }
      entry.announced = false;
      post(entry.port, { op: "frame_announce" });
      pending.push(entry);
    }
  }
  const missing = () => pending.filter((entry) => !entry.announced && entry.port);
  if (!missing().length) {
    callback();
    return;
  }
  // One bounded re-announce: a parent that is not yet listening loses the first post.
  setTimeout(() => {
    const again = missing();
    if (!again.length) {
      callback();
      return;
    }
    for (const entry of again) {
      entry.announced = false;
      post(entry.port, { op: "frame_announce" });
    }
    setTimeout(() => {
      for (const entry of again) {
        if (!entry.announced) {
          entry.late = true;
        }
      }
      callback();
    }, ANNOUNCE_STEP_MS);
  }, ANNOUNCE_STEP_MS);
}

function clampOrigin(value) {
  const origin = normalizeOrigin(value);
  return origin;
}

function clampUrl(value) {
  if (typeof value !== "string") {
    return null;
  }
  const url = value.trim();
  if (!url || url.length > 512 || !/^https?:\/\//i.test(url)) {
    return null;
  }
  return url;
}

function collectFrameRows(tabId, frameElements, childReplies) {
  const frames = frameRegistry.get(tabId) || new Map();
  const rows = [];
  // A row may only claim `ok` when its whole parent chain is still live and linked (P7-3): a
  // re-announce can re-link a grandchild through an ancestor that has already gone stale.
  const chainOk = (entry) => {
    const seen = new Set();
    let cur = entry;
    for (let depth = 0; depth <= 5; depth += 1) {
      if (!cur || cur.linkState !== "ok" || !cur.link) {
        return false;
      }
      if (seen.has(cur.frameId)) {
        return false;
      }
      seen.add(cur.frameId);
      const parent = cur.link.parent;
      if (parent === 0) {
        return frames.has(0);
      }
      const next = frames.get(parent);
      if (!next) {
        return false;
      }
      cur = next;
    }
    return false;
  };
  // Slot identity is scoped by its owning parent frame: `${parent}:${slot}`.
  const knownSlots = new Set();
  for (const entry of frames.values()) {
    if (entry.frameId === 0) {
      continue;
    }
    if (Number.isInteger(entry.slot) && Number.isInteger(entry.slotParent)) {
      knownSlots.add(entry.slotParent + ":" + entry.slot);
    }
    if (entry.link && Number.isInteger(entry.link.parent) && Number.isInteger(entry.link.slot)) {
      knownSlots.add(entry.link.parent + ":" + entry.link.slot);
    }
    const origin = clampOrigin(entry.origin);
    if (entry.budget && entry.linkState === "ok") {
      const row = {
        frame: entry.frameId,
        status: "unavailable",
        reason: "frame_budget_exceeded",
      };
      if (origin) row.origin = origin;
      if (entry.link) row.link = entry.link;
      rows.push(row);
    } else if (entry.link && entry.linkState === "ok" && chainOk(entry)) {
      const row = { frame: entry.frameId, status: "ok" };
      if (entry.link.visible !== true) {
        row.status = "unavailable";
        row.reason = "frame_not_visible";
      } else if (entry.link.transform_ok !== true) {
        row.status = "unavailable";
        row.reason = "frame_geometry_unavailable";
      } else if (entry.link.hit_ok !== true) {
        row.status = "unavailable";
        row.reason =
          entry.link.onscreen === false ? "frame_not_visible" : "frame_occluded";
      }
      if (origin) row.origin = origin;
      row.link = entry.link;
      rows.push(row);
    } else if (entry.linkState === "ok") {
      // Linked locally, but an ancestor level is no longer valid; never advertise `ok`.
      const row = {
        frame: entry.frameId,
        status: "unavailable",
        reason: "frame_geometry_unavailable",
      };
      if (origin) row.origin = origin;
      rows.push(row);
    } else if (entry.linkState === "stale") {
      const row = {
        frame: entry.frameId,
        status: "unavailable",
        reason: "frame_geometry_unavailable",
      };
      if (origin) row.origin = origin;
      rows.push(row);
    } else if (entry.linkState === "ambiguous") {
      const row = {
        frame: entry.frameId,
        status: "unavailable",
        reason: "frame_link_ambiguous",
      };
      if (origin) row.origin = origin;
      rows.push(row);
    } else if (entry.announced) {
      const row = {
        frame: entry.frameId,
        status: "unavailable",
        reason: "frame_link_missing",
      };
      if (origin) row.origin = origin;
      rows.push(row);
    } else {
      const row = {
        frame: entry.frameId,
        status: "unavailable",
        reason: "frame_late",
      };
      if (origin) row.origin = origin;
      rows.push(row);
    }
  }
  // Full frame URL is internal only (Rust vendor classification); never serialized as an
  // origin and never merged into page text.
  for (const row of rows) {
    const entry = frames.get(row.frame);
    if (entry && entry.url) {
      row.url = entry.url;
    }
  }
  // Every injected frame enumerates its own iframes. The top reply is owner 0; each child
  // snapshot carries its own `frame_elements` list, owner = that child frame id.
  const topRegistered = [...frames.values()].some((entry) => entry.frameId === 0);
  // An entry whose iframe is already identified by a top-level slot is deduped by `knownSlots`
  // and must not also consume a same-origin item (P7-1): a linked-then-stale child is still the
  // same iframe, and its distinct un-injected sibling must keep its row.
  const topSlotKnown = (entry) =>
    (entry.slotParent === 0 && Number.isInteger(entry.slot)) ||
    (entry.link &&
      entry.link.parent === 0 &&
      Number.isInteger(entry.link.slot));
  // Without a slot report (top port down), a top iframe item can be *related* to a
  // registered-unlinked child by its exact browser-set URL. It is never dropped on that match:
  // a distinct iframe may legitimately share the URL, so every top item gets its own row and a
  // matching item is labelled with the registered child's honest link reason (P8-1/P9-1).
  const unlinkedByUrl = new Map();
  for (const entry of frames.values()) {
    if (entry.frameId === 0 || (entry.link && entry.linkState === "ok")) {
      continue;
    }
    if (topSlotKnown(entry)) {
      continue;
    }
    const url = clampUrl(entry.url);
    if (url && !unlinkedByUrl.has(url)) {
      unlinkedByUrl.set(
        url,
        entry.linkState === "ambiguous"
          ? "frame_link_ambiguous"
          : entry.linkState === "stale"
            ? "frame_geometry_unavailable"
            : "frame_link_missing"
      );
    }
  }
  const ownerLists = [{ owner: 0, items: Array.isArray(frameElements) ? frameElements : [] }];
  if (childReplies) {
    for (const [frameId, reply] of childReplies) {
      if (reply && Array.isArray(reply.frame_elements)) {
        ownerLists.push({ owner: frameId, items: reply.frame_elements });
      }
    }
  }
  for (const { owner, items } of ownerLists) {
    for (const item of items) {
      if (!item || typeof item.reason !== "string") {
        continue;
      }
      const origin = clampOrigin(item.origin);
      if (Number.isInteger(item.slot) && knownSlots.has(owner + ":" + item.slot)) {
        continue;
      }
      const src = clampUrl(item.src);
      const related =
        owner === 0 &&
        !topRegistered &&
        item.reason === "no_content_script" &&
        src
          ? unlinkedByUrl.get(src) || null
          : null;
      const row = { status: "unavailable", reason: related || item.reason };
      if (origin) row.origin = origin;
      if (owner !== 0) row.parent = owner;
      rows.push(row);
    }
  }
  const rejected = rejectedPorts.get(tabId) || 0;
  if (rejected > 0) {
    // A refused 33rd port must not vanish: one frame-less cap row carries the token.
    rows.push({ status: "unavailable", reason: "frame_cap_exceeded" });
  }
  // Rank visible/on-screen linked frames first, then nearest to the top-left, then DOM order.
  const rank = (row) => {
    if (row.link && row.status === "ok" && row.link.onscreen === true) return 0;
    if (row.link) return 1;
    return 2;
  };
  rows.sort((a, b) => {
    const byRank = rank(a) - rank(b);
    if (byRank !== 0) return byRank;
    const da = a.link ? Math.hypot(a.link.content_left, a.link.content_top) : Infinity;
    const db = b.link ? Math.hypot(b.link.content_left, b.link.content_top) : Infinity;
    if (da !== db) return da - db;
    const sa = a.link && Number.isInteger(a.link.slot) ? a.link.slot : 1e9;
    const sb = b.link && Number.isInteger(b.link.slot) ? b.link.slot : 1e9;
    return sa - sb;
  });
  const childCap = FRAME_ROW_CAP - 1; // frame 0 counts against the cap
  const totalChildren = rows.length;
  let kept = rows.slice(0, childCap);
  const truncated = totalChildren > childCap;
  const capRow = { status: "unavailable", reason: "frame_cap_exceeded" };
  const hasCapRow = kept.some((row) => row.reason === "frame_cap_exceeded");
  if (rejected > 0 && !hasCapRow) {
    kept = kept.slice(0, Math.max(0, childCap - 1));
    kept.push(capRow);
  } else if (truncated && !hasCapRow) {
    kept = kept.slice(0, Math.max(0, childCap - 1));
    kept.push(capRow);
  }
  const hasChildren = totalChildren > 0 || rejected > 0;
  return {
    frames: kept,
    // Omit the field entirely on a top-only page so a new-client `frames:true` envelope keeps
    // the 0123 top-only shape. When present, frame 0 counts against the cap.
    frames_total: hasChildren ? totalChildren + 1 + Math.max(0, rejected - 1) : 0,
    frames_truncated: hasChildren ? truncated || rejected > 0 : false,
  };
}

function unlinkedAnnounced(tabId) {
  const frames = frameRegistry.get(tabId);
  const out = [];
  if (!frames) {
    return out;
  }
  for (const entry of frames.values()) {
    if (
      entry.frameId !== 0 &&
      entry.announced &&
      !entry.link &&
      entry.linkState !== "ambiguous"
    ) {
      out.push(entry);
    }
  }
  return out;
}

// Announce, settle, and re-announce once for an announced-but-unlinked child. A delayed
// parent `message` therefore links instead of being reported as missing.
function announceAndMeasure(tabId, callback) {
  announceAll(tabId, () => {
    setTimeout(() => {
      const retry = unlinkedAnnounced(tabId);
      if (!retry.length) {
        callback();
        return;
      }
      for (const entry of retry) {
        entry.announced = false;
        post(entry.port, { op: "frame_announce" });
      }
      setTimeout(callback, LINK_SETTLE_MS);
    }, LINK_SETTLE_MS);
  });
}

function frameBySlot(tabId, slot) {
  const frames = frameRegistry.get(tabId);
  if (!frames) {
    return null;
  }
  for (const entry of frames.values()) {
    const ownsTopSlot =
      (entry.link && entry.link.parent === 0) || entry.slotParent === 0;
    if (
      entry.frameId !== 0 &&
      entry.linkState === "ok" &&
      ownsTopSlot &&
      ((entry.link && entry.link.slot === slot) || entry.slot === slot)
    ) {
      return entry;
    }
  }
  return null;
}

function childSnapshot(entry, stored, detail, maxNodes, commitBy, callback) {
  sendToFrame(
    entry.tab,
    entry.frameId,
    {
      op: "snapshot",
      detail: detail || "default",
      store: false,
      // A fan-out reply is a candidate; only an accepted reply is committed by the SW.
      pending: stored === true,
      commit_by: commitBy,
      light: true,
      max_nodes: maxNodes,
      frames: false,
      frame_elements: true,
      frame: entry.frameId,
    },
    (reply) => {
      if (chrome.runtime.lastError || !reply || reply.error) {
        callback(null);
        return;
      }
      callback(reply);
    }
  );
}

function commitChildSnapshot(entry, reply, callback) {
  if (!reply || reply.pending !== true || typeof reply.snap !== "string") {
    callback(reply);
    return;
  }
  sendToFrame(
    entry.tab,
    entry.frameId,
    { op: "frame_commit", frame: entry.frameId, snap: reply.snap },
    (commit) => {
      if (
        chrome.runtime.lastError ||
        !commit ||
        commit.error ||
        commit.committed !== true
      ) {
        callback(null);
        return;
      }
      callback(
        Object.assign({}, reply, {
          doc: commit.doc,
          snap: commit.snap,
          retained: commit.retained,
          committed: true,
          pending: false,
        })
      );
    }
  );
}

function collectChildSnapshots(
  tabId,
  entries,
  stored,
  detail,
  maxNodes,
  commitBy,
  callback
) {
  const replies = new Map();
  if (!entries.length) {
    callback(replies);
    return;
  }
  let pending = entries.length;
  let committing = 0;
  let wantFinish = false;
  let cut = false;
  let finished = false;
  const finish = () => {
    if (finished) {
      return;
    }
    finished = true;
    callback(replies);
  };
  const maybeFinish = () => {
    if (finished || !wantFinish) {
      return;
    }
    // After the straggler cutoff, finish with the replies received so far; a pending reply is
    // abandoned so one hung child cannot discard the healthy children's elements (P7-2).
    if (committing <= 0 && (cut || pending <= 0)) {
      finish();
    }
  };
  const done = () => {
    if (finished) {
      return;
    }
    pending -= 1;
    maybeFinish();
  };
  setTimeout(() => {
    wantFinish = true;
    cut = true;
    maybeFinish();
  }, CHILD_SNAPSHOT_TIMEOUT_MS);
  for (const entry of entries) {
    childSnapshot(entry, stored, detail, maxNodes, commitBy, (reply) => {
      if (finished) {
        // Late/discarded: never commit, so the child's `lastSnapshot` is untouched.
        return;
      }
      if (!reply) {
        done();
        return;
      }
      if (!stored) {
        replies.set(entry.frameId, reply);
        done();
        return;
      }
      committing += 1;
      let settled = false;
      const settle = (committed) => {
        if (settled) {
          return;
        }
        settled = true;
        committing -= 1;
        if (committed) {
          replies.set(entry.frameId, committed);
        }
        done();
        maybeFinish();
      };
      commitChildSnapshot(entry, reply, settle);
      // A commit that never answers must not hold the envelope open, but a slow commit can
      // still finish before the fan-out deadline; wait through `commit_by` plus the delivery
      // margin, settling just before the outer finalize.
      const settleWait = Math.max(
        30,
        commitBy + FANOUT_MARGIN_MS - 5 - Date.now()
      );
      setTimeout(() => settle(null), settleWait);
    });
  }
}

function mergeFrameSnapshot(out, tab, msg, replies, frameElements) {
  const stored = out.stored !== false;
  for (const [frameId, child] of replies) {
    const doc = typeof child.doc === "string" ? child.doc : null;
    const snap = typeof child.snap === "string" ? child.snap : null;
    const valid =
      stored &&
      child.committed === true &&
      doc &&
      snap &&
      /^[0-9a-f]{4}$/.test(doc) &&
      /^[0-9a-f]{8}$/.test(snap);
    if (stored && !valid) {
      // An agent-facing observe must not expose a third, ref-less id shape.
      continue;
    }
    const elements = Array.isArray(child.elements) ? child.elements : [];
    for (const el of elements) {
      if (!el || typeof el !== "object") {
        continue;
      }
      el.frame = frameId;
      const idx = Number.isInteger(el.idx) ? el.idx : 0;
      if (valid) {
        const token =
          "sref1:c." +
          b36(tab.id) +
          "." +
          b36(frameId) +
          "." +
          doc +
          "." +
          snap +
          "." +
          b36(idx);
        el.id = token;
        el.ref = token;
      } else {
        // `chr:` is top-frame-only. Internal (store:false) reads still carry frame elements
        // for detection but cannot mint a resolvable ref without a stored doc/snap pair.
        el.id = "frame:" + frameId + ":" + idx;
        delete el.ref;
      }
      out.elements.push(el);
    }
  }
  const rows = collectFrameRows(tab.id, frameElements, replies);
  for (const row of rows.frames) {
    const child = replies.get(row.frame);
    if (!child) {
      continue;
    }
    if (typeof child.doc === "string") row.doc = child.doc;
    if (typeof child.snap === "string") row.snap = child.snap;
    if (Number.isInteger(child.retained)) row.retained = child.retained;
  }
  if (rows.frames_total > 0) {
    out.frames = rows.frames;
    out.frames_total = rows.frames_total;
    out.frames_truncated = rows.frames_truncated;
  } else {
    delete out.frames;
    delete out.frames_total;
    delete out.frames_truncated;
  }
  for (const child of replies.values()) {
    if (child && typeof child === "object") {
      delete child.frame_elements;
    }
  }
}

function finishFrameSnapshot(out, tab, port, msg) {
  const frameElements = Array.isArray(out.frame_elements) ? out.frame_elements : [];
  delete out.frame_elements;
  const state = { entries: [], replies: new Map(), done: false };
  const finalize = () => {
    if (state.done) {
      return;
    }
    state.done = true;
    const entries = state.entries.length
      ? state.entries
      : [...(frameRegistry.get(tab.id) || new Map()).values()].filter(
          (entry) => entry.frameId !== 0 && entry.linkState === "ok"
        );
    for (const entry of entries) {
      if (!state.replies.has(entry.frameId)) {
        entry.budget = true;
      }
    }
    mergeFrameSnapshot(out, tab, msg, state.replies, frameElements);
    post(port, out);
  };
  const started = Number.isFinite(msg && msg.__handsStarted)
    ? msg.__handsStarted
    : Date.now();
  const elapsed = Math.max(0, Date.now() - started);
  const remaining = CLIENT_BUDGET_MS - elapsed - FANOUT_MARGIN_MS;
  if (remaining < MIN_FANOUT_MS) {
    // The top walk already consumed the client budget: skip fan-out honestly.
    const entries = [...(frameRegistry.get(tab.id) || new Map()).values()].filter(
      (entry) => entry.frameId !== 0 && entry.linkState === "ok"
    );
    for (const entry of entries) {
      entry.budget = true;
    }
    state.entries = entries;
    finalize();
    return;
  }
  const budget = Math.min(FANOUT_BUDGET_MS, remaining);
  // A child may commit up to `commit_by`; leave one margin so an accepted commit can reach the
  // SW before the outer finalize instead of being committed child-side and dropped (P9-2).
  const finalizeAt = Date.now() + budget;
  const commitBy = finalizeAt - FANOUT_MARGIN_MS;
  setTimeout(finalize, budget);
  const known = frameRegistry.get(tab.id);
  const hasChild = known
    ? [...known.values()].some((entry) => entry.frameId !== 0)
    : false;
  if (!hasChild) {
    // A top-only page needs no announce/measure settle. A pruned top port plus a registered
    // child still counts as a child.
    finalize();
    return;
  }
  announceAndMeasure(tab.id, () => {
    const frames = frameRegistry.get(tab.id) || new Map();
    const entries = [...frames.values()].filter(
      (entry) => entry.frameId !== 0 && entry.linkState === "ok"
    );
    for (const entry of entries) {
      entry.budget = false;
    }
    state.entries = entries;
    // Top walk keeps its 4000-node cap; the child budget keeps the total under 12000.
    const maxNodes = Math.max(100, Math.floor(8000 / Math.max(1, entries.length)));
    collectChildSnapshots(
      tab.id,
      entries,
      out.stored !== false,
      msg && msg.detail,
      maxNodes,
      commitBy,
      (replies) => {
        for (const [frameId, reply] of replies) {
          state.replies.set(frameId, reply);
        }
        finalize();
      }
    );
  });
}

function pointFromRect(rect) {
  if (
    !rect ||
    !Number.isFinite(rect.left) ||
    !Number.isFinite(rect.top) ||
    !Number.isFinite(rect.width) ||
    !Number.isFinite(rect.height) ||
    rect.width <= 0 ||
    rect.height <= 0
  ) {
    return null;
  }
  return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
}

function refreshChain(tabId, frameId, point, callback) {
  const frames = frameRegistry.get(tabId);
  if (!frames) {
    callback(null);
    return;
  }
  const entry = frames.get(frameId);
  if (!entry || !entry.link || entry.linkState !== "ok") {
    callback(null);
    return;
  }
  const ancestors = [];
  let current = entry;
  for (let depth = 0; depth < 6; depth += 1) {
    if (!current.link || current.linkState !== "ok") {
      callback(null);
      return;
    }
    ancestors.push(current);
    if (current.link.parent === 0) {
      break;
    }
    const parent = frames.get(current.link.parent);
    if (!parent) {
      callback(null);
      return;
    }
    current = parent;
  }
  let index = 0;
  const deadline = Date.now() + CHAIN_BUDGET_MS;
  // The point starts in the target frame's local viewport. After each level it is translated
  // into that parent's viewport, so the next level tests the same click point.
  let local = point && Number.isFinite(point.x) && Number.isFinite(point.y) ? point : null;
  const next = () => {
    if (index >= ancestors.length) {
      const chain = [];
      for (const level of ancestors) {
        if (!level.link || level.linkState !== "ok") {
          callback(null, "frame_geometry_unavailable");
          return;
        }
        chain.push(level.link);
      }
      callback(chain);
      return;
    }
    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      callback(null, "frame_budget_exceeded");
      return;
    }
    const level = ancestors[index];
    index += 1;
    const parent = frames.get(level.link.parent);
    if (!parent || !parent.port) {
      callback(null, "frame_geometry_unavailable");
      return;
    }
    const token = level.token;
    const at = local ? { x: local.x, y: local.y } : null;
    // Register the waiter before posting: a fast parent replies in the same turn.
    waitMeasure(
      token,
      (link) => {
        if (!link) {
          callback(
            null,
            Date.now() >= deadline
              ? "frame_budget_exceeded"
              : "frame_geometry_unavailable"
          );
          return;
        }
        if (local) {
          local = {
            x: link.content_left + local.x,
            y: link.content_top + local.y,
          };
        }
        next();
      },
      remaining
    );
    post(parent.port, { op: "frame_measure", tokens: [token], point: at });
  };
  next();
}

function enrichFrameReply(tab, frame, out, port) {
  // Fresh top metrics (no walk/store) + fresh chain, in the same exchange as the child reply.
  sendToFrame(tab.id, 0, { op: "frame_top", frame: 0 }, (top) => {
    if (chrome.runtime.lastError || !top || top.error || top.frame !== 0) {
      out.error = "frame_geometry_unavailable";
      post(port, out);
      return;
    }
    const point =
      pointFromRect(out.rectCss) ||
      (out.css && Number.isFinite(out.css.x) && Number.isFinite(out.css.y)
        ? { x: out.css.x, y: out.css.y }
        : null);
    refreshChain(tab.id, frame, point, (chain, reason) => {
      if (!chain) {
        out.error = reason || "frame_geometry_unavailable";
        post(port, out);
        return;
      }
      out.top = { frame: 0, metrics: top.metrics, url: top.url, title: top.title };
      out.chain = chain;
      post(port, out);
    });
  });
}

function frameBindingRequest(port, tab, msg) {
  const frame = Number(msg.frame);
  chrome.windows.get(tab.windowId, (win) => {
    if (chrome.runtime.lastError || !win) {
      post(port, { error: "window_unreadable", tab: tab.id });
      return;
    }
    const evidence = {
      tab: tab.id,
      window: tab.windowId,
      active: !!tab.active,
      focused: !!win.focused,
      fallback: false,
      frame: frame,
    };
    sendToFrame(
      tab.id,
      frame,
      { op: "frame_binding", frame: frame, doc: msg.doc, snap: msg.snap, idx: msg.idx },
      (reply) => {
        if (chrome.runtime.lastError) {
          post(port, Object.assign({ error: "frame_unavailable" }, evidence));
          return;
        }
        if (!reply) {
          post(port, Object.assign({ error: "empty" }, evidence));
          return;
        }
        if (reply.error) {
          post(port, Object.assign({ error: reply.error }, evidence));
          return;
        }
        const point = pointFromRect(reply.rectCss);
        refreshChain(tab.id, frame, point, (chain, reason) => {
          const out = copyReply(reply);
          Object.assign(out, evidence);
          out.chain = chain;
          if (!chain) {
            out.error = reason || "frame_geometry_unavailable";
          }
          post(port, out);
        });
      }
    );
  });
}

// ------------------------------------------------------------------ host forward

function forward(port, msg) {
  if (msg && typeof msg === "object" && !Number.isFinite(msg.__handsStarted)) {
    // The top walk runs before the SW sees this reply; the frame fan-out must fit the
    // remainder of the Rust client's 400 ms deadline.
    msg.__handsStarted = Date.now();
  }
  const op = msg && msg.op;
  if (
    op !== "snapshot" &&
    op !== "resolve" &&
    op !== "assess" &&
    op !== "postcheck" &&
    op !== "binding" &&
    op !== "frame_binding"
  ) {
    post(port, { error: "unsupported-op" });
    return;
  }
  if (op === "binding" && msg.tab == null) {
    post(port, { error: "protocol_error" });
    return;
  }
  if (op === "postcheck" && msg.tab == null) {
    // A postcondition is never evaluated against whatever tab is in front.
    post(port, { error: "protocol_error" });
    return;
  }
  if (op === "frame_binding") {
    const frame = Number(msg.frame);
    if (msg.tab == null || !Number.isInteger(frame) || frame <= 0) {
      post(port, { error: "protocol_error" });
      return;
    }
  }
  if (msg.tab != null) {
    // Named/scoped request: route by tab id and never fall back to the active tab.
    const want = Number(msg.tab);
    if (!Number.isInteger(want) || want <= 0) {
      post(port, { error: "protocol_error" });
      return;
    }
    chrome.tabs.get(want, (tab) => {
      if (chrome.runtime.lastError || !tab || tab.id == null) {
        post(port, { error: "tab_unavailable", tab: want });
        return;
      }
      if (tab.id !== want) {
        post(port, { error: "protocol_error", tab: want });
        return;
      }
      if (op === "binding") {
        chrome.windows.get(tab.windowId, (win) => {
          if (chrome.runtime.lastError || !win) {
            post(port, { error: "window_unreadable", tab: want });
            return;
          }
          post(port, {
            tab: want,
            window: tab.windowId,
            active: !!tab.active,
            focused: !!win.focused,
            fallback: false,
          });
        });
        return;
      }
      if (op === "frame_binding") {
        frameBindingRequest(port, tab, msg);
        return;
      }
      if (op === "snapshot" && msg.frames === true) {
        withTabEvidence(port, tab, false, msg, finishFrameSnapshot);
        return;
      }
      withTabEvidence(port, tab, false, msg);
    });
    return;
  }
  // Unscoped observe keeps the existing last-focused → normal-window fallback, but labels
  // the answering tab and marks the fallback so refs are withheld.
  chrome.tabs.query({ active: true, lastFocusedWindow: true }, (tabs) => {
    void chrome.runtime.lastError;
    const tab = tabs && tabs[0];
    if (tab && tab.id != null) {
      if (op === "snapshot" && msg.frames === true) {
        withTabEvidence(port, tab, false, msg, finishFrameSnapshot);
        return;
      }
      withTabEvidence(port, tab, false, msg);
      return;
    }
    chrome.tabs.query({ active: true, windowType: "normal" }, (rest) => {
      void chrome.runtime.lastError;
      const fallback = rest && rest[0];
      if (!fallback || fallback.id == null) {
        post(port, { error: "no-tab" });
        return;
      }
      if (op === "snapshot" && msg.frames === true) {
        withTabEvidence(port, fallback, true, msg, finishFrameSnapshot);
        return;
      }
      withTabEvidence(port, fallback, true, msg);
    });
  });
}

if (chrome.runtime.onConnect) {
  chrome.runtime.onConnect.addListener((port) => {
    if (port && port.name === FRAME_PORT_NAME) {
      registerFramePort(port);
    }
  });
}
if (chrome.tabs) {
  if (chrome.tabs.onRemoved) {
    chrome.tabs.onRemoved.addListener((tabId) => pruneTab(tabId));
  }
  if (chrome.tabs.onUpdated) {
    chrome.tabs.onUpdated.addListener((tabId, info) => {
      if (info && info.status === "loading") {
        pruneTab(tabId);
      }
    });
  }
}

connectHost();
chrome.runtime.onStartup.addListener(connectHost);
chrome.runtime.onInstalled.addListener(connectHost);

function keepWorker() {
  chrome.runtime.getPlatformInfo(() => {
    void chrome.runtime.lastError;
    setTimeout(keepWorker, 20000);
  });
}
keepWorker();
