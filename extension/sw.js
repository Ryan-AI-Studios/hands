const HOST_NAME = "com.helpinghands.host";
const FRAME_PORT_NAME = "hands-frame";
const FRAME_PORT_CAP = 32;
const FRAME_ROW_CAP = 12;
const ANNOUNCE_STEP_MS = 60;
const LINK_SETTLE_MS = 60;
const MEASURE_TIMEOUT_MS = 100;

let hostPort = null;
let reconnectTimer = null;

// Per-tab frame registry: tab id -> Map(frameId -> entry).
// Entry: {tab, frameId, documentId, documentLifecycle, origin, url, port, token,
//         doc, link, linkState, linkReason, parent, announced, late}
const frameRegistry = new Map();
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
  const frame = msg && msg.frame;
  if (Number.isInteger(frame) && frame > 0) {
    return frame;
  }
  return 0;
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
        enrich(out, tab, port);
        return;
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
    if (op === "frame_link") {
      acceptFrameLink(entry, msg);
    }
  });
  port.onDisconnect.addListener(() => {
    if (frames.get(frameId) === entry) {
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

function waitMeasure(token, callback) {
  let waiters = measureWaiters.get(token);
  if (!waiters) {
    waiters = [];
    measureWaiters.set(token, waiters);
  }
  waiters.push(callback);
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
  }, MEASURE_TIMEOUT_MS);
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

function collectFrameRows(tabId, frameElements) {
  const frames = frameRegistry.get(tabId) || new Map();
  const rows = [];
  const linkedSlots = new Set();
  for (const entry of frames.values()) {
    if (entry.frameId === 0) {
      continue;
    }
    const origin = clampOrigin(entry.origin);
    if (entry.link && entry.linkState === "ok") {
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
      if (Number.isInteger(entry.link.slot)) {
        linkedSlots.add(entry.link.slot);
      }
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
  const seen = Array.isArray(frameElements) ? frameElements : [];
  for (const item of seen) {
    if (!item || typeof item.reason !== "string") {
      continue;
    }
    if (Number.isInteger(item.slot) && linkedSlots.has(item.slot)) {
      continue;
    }
    const row = { status: "unavailable", reason: item.reason };
    const origin = clampOrigin(item.origin);
    if (origin) row.origin = origin;
    rows.push(row);
  }
  // Rank linked/registered frames first; keep unavailable reasons behind them.
  rows.sort((a, b) => {
    const rank = (row) => (row.status === "ok" ? 0 : 1);
    return rank(a) - rank(b);
  });
  const total = rows.length;
  const kept = rows.slice(0, FRAME_ROW_CAP);
  return {
    frames: kept,
    frames_total: total,
    frames_truncated: total > kept.length,
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

function finishFrameSnapshot(out, tab, port) {
  const frameElements = Array.isArray(out.frame_elements) ? out.frame_elements : [];
  delete out.frame_elements;
  announceAndMeasure(tab.id, () => {
    const rows = collectFrameRows(tab.id, frameElements);
    out.frames = rows.frames;
    out.frames_total = rows.frames_total;
    out.frames_truncated = rows.frames_truncated;
    post(port, out);
  });
}

function refreshChain(tabId, frameId, callback) {
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
  const next = () => {
    if (index >= ancestors.length) {
      const chain = [];
      for (const level of ancestors) {
        if (!level.link || level.linkState !== "ok") {
          callback(null);
          return;
        }
        chain.push(level.link);
      }
      callback(chain);
      return;
    }
    const level = ancestors[index];
    index += 1;
    const parent = frames.get(level.link.parent);
    if (!parent || !parent.port) {
      callback(null);
      return;
    }
    const token = level.token;
    // Register the waiter before posting: a fast parent replies in the same turn.
    waitMeasure(token, (link) => {
      if (!link) {
        callback(null);
        return;
      }
      next();
    });
    post(parent.port, { op: "frame_measure", tokens: [token] });
  };
  next();
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
      { op: "frame_binding", frame: frame, doc: msg.doc, snap: msg.snap },
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
        refreshChain(tab.id, frame, (chain) => {
          const out = copyReply(reply);
          Object.assign(out, evidence);
          out.chain = chain;
          if (!chain) {
            out.error = "frame_geometry_unavailable";
          }
          post(port, out);
        });
      }
    );
  });
}

// ------------------------------------------------------------------ host forward

function forward(port, msg) {
  const op = msg && msg.op;
  if (
    op !== "snapshot" &&
    op !== "resolve" &&
    op !== "assess" &&
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
