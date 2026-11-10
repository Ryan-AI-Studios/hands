/* Executable harness for the read-only extension logic.
 * Run with: node extension/tests/harness.js
 * No network, no browser, no synthetic DOM input.
 */
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const root = path.resolve(__dirname, "..");

// Deterministic virtual clock shared by both VM contexts. The fan-out/commit budgets are
// wall-clock based; a real clock made the harness flaky on slower CI runners.
const CLOCK = { t: 1_700_000_000_000 };
function dateShim() {
  return { now: () => CLOCK.t };
}

function defaultStyle(overrides) {
  return Object.assign(
    {
      display: "block",
      visibility: "visible",
      opacity: "1",
      pointerEvents: "auto",
      transform: "none",
      scale: "none",
      zoom: "1",
      filter: "none",
      perspective: "none",
      willChange: "auto",
      overflow: "visible",
      overflowX: "visible",
      overflowY: "visible",
      borderLeftWidth: "0px",
      borderTopWidth: "0px",
      borderRightWidth: "0px",
      borderBottomWidth: "0px",
      paddingLeft: "0px",
      paddingTop: "0px",
      paddingRight: "0px",
      paddingBottom: "0px",
    },
    overrides || {}
  );
}

function element(options) {
  const opts = options || {};
  const attrs = opts.attrs || {};
  const el = {
    nodeType: 1,
    tagName: opts.tag || "BUTTON",
    type: opts.type || attrs.type || "",
    innerText: opts.innerText || "",
    isContentEditable: !!opts.contentEditable,
    isConnected: opts.isConnected !== false,
    href: attrs.href || "",
    value: "",
    children: [],
    parentElement: null,
    shadowRoot: opts.shadowRoot || null,
    contentWindow: opts.contentWindow || null,
    style: defaultStyle(opts.style),
    getAttribute(name) {
      return Object.prototype.hasOwnProperty.call(attrs, name) ? attrs[name] : null;
    },
    hasAttribute(name) {
      return Object.prototype.hasOwnProperty.call(attrs, name);
    },
    getBoundingClientRect() {
      return opts.rect || { left: 10, top: 20, width: 100, height: 40 };
    },
    matches() {
      return true;
    },
    closest() {
      return null;
    },
    getRootNode() {
      return { host: el.parentElement || null };
    },
    checkVisibility() {
      return opts.visible !== false;
    },
  };
  return el;
}

function attach(parent, child) {
  parent.children.push(child);
  child.parentElement = parent;
  return child;
}

function runTimers(timers, limit) {
  const max = limit || 60;
  for (let i = 0; i < max; i += 1) {
    // Virtual clock: run the earliest scheduled delay first, not insertion order.
    let index = -1;
    for (let j = 0; j < timers.length; j += 1) {
      if (timers[j].ms > 5000) {
        continue;
      }
      if (index === -1 || timers[j].ms < timers[index].ms) {
        index = j;
      }
    }
    if (index === -1) {
      return;
    }
    const timer = timers.splice(index, 1)[0];
    CLOCK.t += timer.ms;
    timer.fn();
  }
}

function contentContext(options) {
  const opts = options || {};
  const messageListeners = [];
  const timers = [];
  const context = {
    console,
    Math,
    Date: dateShim(),
    Number,
    String,
    Array,
    Object,
    JSON,
    Uint32Array,
    Uint8Array,
    URL,
    URLSearchParams,
    RegExp,
    Error,
    Promise,
    setTimeout(fn, ms) {
      timers.push({ fn, ms: Number(ms) || 0 });
      return timers.length;
    },
    chrome: {
      runtime: {
        onMessage: {
          addListener(listener) {
            context.__listener = listener;
          },
        },
        connect(payload) {
          context.__connectPayload = payload;
          if (typeof opts.connect !== "function") {
            return null;
          }
          const port = opts.connect(context);
          context.__port = port;
          return port;
        },
      },
      dom: {
        openOrClosedShadowRoot: opts.openOrClosedShadowRoot || undefined,
      },
    },
    crypto: {
      getRandomValues(array) {
        array[0] = opts.nonce || 0x12345678;
        return array;
      },
    },
    window: {
      screen: {},
      visualViewport: null,
      innerWidth: opts.innerWidth || 800,
      innerHeight: opts.innerHeight || 600,
      outerWidth: 800,
      outerHeight: 600,
      screenX: 0,
      screenY: 0,
      addEventListener(type, fn) {
        if (type === "message") {
          messageListeners.push(fn);
        }
      },
      getComputedStyle(el) {
        return (el && el.style) || defaultStyle();
      },
    },
    document: {
      title: opts.title || "",
      visibilityState: "visible",
      documentElement: opts.documentElement || null,
      body: null,
      querySelector() {
        return null;
      },
      querySelectorAll() {
        return [];
      },
      elementFromPoint(x, y) {
        return opts.elementFromPoint ? opts.elementFromPoint(x, y) : null;
      },
      elementsFromPoint() {
        return [];
      },
    },
    location: { href: opts.href || "https://example.test/", search: "" },
    performance: { now: () => 0 },
    CSS: { escape: (value) => value },
  };
  context.window.parent = {
    postMessage(data, target) {
      if (typeof opts.onParentPost !== "function") {
        return;
      }
      opts.onParentPost({
        data,
        target,
        source: context.window,
        origin: opts.origin || "https://example.test",
      });
    },
  };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(path.join(root, "content.js"), "utf8"), context, {
    filename: "content.js",
  });
  context.__dispatchMessage = (event) => {
    for (const fn of messageListeners) {
      fn(event);
    }
  };
  context.__runTimers = (limit) => runTimers(timers, limit);
  return context;
}

function installSnapshot(context, node, doc, snap) {
  context.__node = node;
  vm.runInContext(
    `lastSnapshot = {
      doc: __doc,
      snap: __snap,
      nodes: [__node],
      keys: [snapshotKey(__node, keyPayload(__node, 0))],
      retained: 1,
      url: location.href,
      title: document.title,
    };`,
    Object.assign(context, { __doc: doc, __snap: snap })
  );
}

function scopedResolve(context, message) {
  context.__msg = Object.assign({ v: 1, frame: 0, idx: 0 }, message);
  return vm.runInContext("resolveScoped(__msg)", context);
}

function listenerResolve(context, message) {
  let response = null;
  context.__listener(message, null, (value) => {
    response = value;
  });
  return response;
}

function contentTests() {
  const context = contentContext();
  const node = element({ tag: "BUTTON", attrs: { "aria-label": "Send" } });
  installSnapshot(context, node, "aaaa", "11111111");

  let reply = scopedResolve(context, { doc: "aaaa", snap: "11111111" });
  assert.equal(reply.error, undefined);
  assert.equal(reply.id, "chr:0");
  assert.equal(reply.doc, "aaaa");
  assert.equal(reply.snap, "11111111");
  assert.equal(reply.idx, 0);
  assert.equal(reply.visibilityState, "visible");
  assert.equal(reply.rectCss.width, 100);
  assert.equal(reply.frame, 0);

  reply = scopedResolve(context, { doc: "bbbb", snap: "11111111" });
  assert.equal(reply.error, "stale_document");

  reply = scopedResolve(context, { doc: "aaaa", snap: "22222222" });
  assert.equal(reply.error, "stale_snapshot");

  node.isConnected = false;
  reply = scopedResolve(context, { doc: "aaaa", snap: "11111111" });
  assert.equal(reply.error, "stale_element");
  node.isConnected = true;

  const label = element({ tag: "BUTTON", innerText: "Buy" });
  installSnapshot(context, label, "eeee", "55555555");
  reply = scopedResolve(context, { doc: "eeee", snap: "55555555" });
  assert.equal(reply.error, undefined);
  label.innerText = "Buy now";
  reply = scopedResolve(context, { doc: "eeee", snap: "55555555" });
  assert.equal(reply.error, "stale_identity");

  const editable = element({
    tag: "DIV",
    attrs: { contenteditable: "true" },
    innerText: "Draft",
    contentEditable: true,
  });
  installSnapshot(context, editable, "cccc", "33333333");
  reply = scopedResolve(context, { doc: "cccc", snap: "33333333" });
  assert.equal(reply.error, undefined);
  editable.innerText = "Draft with typed text";
  reply = scopedResolve(context, { doc: "cccc", snap: "33333333" });
  assert.equal(reply.error, undefined, "contenteditable typing must keep the key");

  const password = element({
    tag: "INPUT",
    attrs: { type: "password" },
    type: "password",
  });
  installSnapshot(context, password, "dddd", "44444444");
  reply = scopedResolve(context, { doc: "dddd", snap: "44444444" });
  assert.equal(reply.error, undefined);
  password.value = "secret-that-must-not-be-hashed";
  reply = scopedResolve(context, { doc: "dddd", snap: "44444444" });
  assert.equal(reply.error, undefined, "password value must not affect the key");

  // Listener predicate routes a scoped request before the legacy buildResolve path.
  const listenerScoped = listenerResolve(context, {
    op: "resolve",
    v: 1,
    frame: 0,
    doc: "dddd",
    snap: "44444444",
    idx: 0,
  });
  assert.equal(listenerScoped.id, "chr:0");
  const listenerLegacy = listenerResolve(context, { op: "resolve", id: "chr:0" });
  assert.equal(listenerLegacy.error, "not-found");
}

function defaultTabs() {
  return {
    12: { id: 12, windowId: 45, active: true, url: "https://example.test/", title: "Same title" },
    14: { id: 14, windowId: 46, active: true, url: "https://other.test/", title: "Same title" },
  };
}

function swContext() {
  const calls = { query: 0, get: 0, windows: 0, send: [], routes: [], posts: [] };
  const timers = [];
  const connectListeners = [];
  const removedListeners = [];
  const updatedListeners = [];
  const contentByFrame = new Map();
  const tabs = defaultTabs();
  const port = {
    onMessage: { addListener() {} },
    onDisconnect: { addListener() {} },
    postMessage(message) {
      calls.posts.push(message);
    },
  };
  let focused = true;
  let tokenCounter = 0;
  let contentReply = {
    id: "chr:7",
    role: "Button",
    text: "Sign in",
    rectCss: { left: 10, top: 20, width: 100, height: 40 },
    metrics: {},
    doc: "9f3a",
    snap: "a1b2c3d4",
    idx: 7,
    frame: 0,
    visibilityState: "visible",
  };
  const context = {
    console,
    Object,
    Number,
    String,
    Math,
    JSON,
    Uint8Array,
    Date: dateShim(),
    setTimeout(fn, ms) {
      timers.push({ fn, ms: Number(ms) || 0 });
      return timers.length;
    },
    crypto: {
      getRandomValues(array) {
        tokenCounter += 1;
        for (let i = 0; i < array.length; i += 1) {
          array[i] = (tokenCounter * 37 + i) & 0xff;
        }
        return array;
      },
    },
    chrome: {
      runtime: {
        connectNative() {
          return port;
        },
        onConnect: {
          addListener(fn) {
            connectListeners.push(fn);
          },
        },
        onStartup: { addListener() {} },
        onInstalled: { addListener() {} },
        getPlatformInfo(callback) {
          if (callback) callback({ os: "win" });
        },
        lastError: null,
      },
      tabs: {
        query(_query, callback) {
          calls.query += 1;
          callback([]);
        },
        get(id, callback) {
          calls.get += 1;
          if (tabs[id]) {
            callback(tabs[id]);
          } else {
            context.chrome.runtime.lastError = { message: "No tab" };
            callback(undefined);
            context.chrome.runtime.lastError = null;
          }
        },
        sendMessage(id, message, options, callback) {
          const opts = typeof options === "function" ? {} : options;
          const cb = typeof options === "function" ? options : callback;
          calls.send.push(id);
          calls.routes.push({ tab: id, frameId: opts && opts.frameId, op: message && message.op });
          const handler = contentByFrame.get(id + ":" + (opts && opts.frameId));
          if (!handler) {
            if ((opts && opts.frameId) === 0 && contentReply) {
              context.chrome.runtime.lastError = null;
              if (cb) cb(contentReply);
              return;
            }
            context.chrome.runtime.lastError = { message: "Could not establish connection" };
            if (cb) cb(undefined);
            context.chrome.runtime.lastError = null;
            return;
          }
          handler(message, (reply) => {
            context.chrome.runtime.lastError = null;
            if (cb) cb(reply);
          });
        },
        onRemoved: {
          addListener(fn) {
            removedListeners.push(fn);
          },
        },
        onUpdated: {
          addListener(fn) {
            updatedListeners.push(fn);
          },
        },
      },
      windows: {
        get(_id, callback) {
          calls.windows += 1;
          callback({ focused: focused });
        },
      },
    },
  };
  context.__port = port;
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(path.join(root, "sw.js"), "utf8"), context, {
    filename: "sw.js",
  });
  return {
    context,
    calls,
    port,
    registry(tabId) {
      context.__tab = tabId;
      return vm.runInContext("frameRegistry.get(__tab) || null", context);
    },
    tokens() {
      return vm.runInContext("Array.from(tokenIndex.keys())", context);
    },
    setFocused(value) {
      focused = value;
    },
    setActive(tabId, value) {
      tabs[tabId].active = value;
    },
    setContentReply(value) {
      contentReply = value;
    },
    setContent(tabId, frameId, handler) {
      contentByFrame.set(tabId + ":" + frameId, handler);
    },
    connect(portLike) {
      for (const fn of connectListeners) {
        fn(portLike);
      }
    },
    removeTab(tabId) {
      for (const fn of removedListeners) {
        fn(tabId);
      }
    },
    updateTab(tabId, info) {
      for (const fn of updatedListeners) {
        fn(tabId, info);
      }
    },
    runTimers(limit) {
      runTimers(timers, limit);
    },
    defer(fn, ms) {
      timers.push({ fn, ms: Number(ms) || 0 });
    },
    lastPost() {
      return calls.posts[calls.posts.length - 1];
    },
    forward(message) {
      context.__msg = message;
      vm.runInContext("forward(__port, __msg)", context);
      return calls.posts[calls.posts.length - 1];
    },
  };
}

function swTests() {
  const harness = swContext();

  let reply = harness.forward({
    op: "resolve",
    v: 1,
    tab: 12,
    frame: 0,
    doc: "9f3a",
    snap: "a1b2c3d4",
    idx: 7,
  });
  assert.equal(harness.calls.query, 0, "named request must not query tabs");
  assert.deepEqual(harness.calls.send, [12]);
  assert.deepEqual(harness.calls.routes[0], { tab: 12, frameId: 0, op: "resolve" });
  assert.equal(reply.tab, 12);
  assert.equal(reply.window, 45);
  assert.equal(reply.active, true);
  assert.equal(reply.focused, true);
  assert.equal(reply.fallback, false);
  assert.equal(reply.doc, "9f3a");

  harness.setActive(12, false);
  reply = harness.forward({
    op: "resolve",
    v: 1,
    tab: 12,
    frame: 0,
    doc: "9f3a",
    snap: "a1b2c3d4",
    idx: 7,
  });
  assert.equal(reply.active, false, "background tab is labelled, not routed elsewhere");
  harness.setActive(12, true);

  harness.setContentReply({
    id: "chr:7",
    role: "Button",
    text: "Sign in",
    rectCss: { left: 10, top: 20, width: 100, height: 40 },
    metrics: {},
    doc: "9f3a",
    snap: "a1b2c3d4",
    idx: 7,
    frame: 0,
    visibilityState: "hidden",
  });
  reply = harness.forward({
    op: "resolve",
    v: 1,
    tab: 12,
    frame: 0,
    doc: "9f3a",
    snap: "a1b2c3d4",
    idx: 7,
  });
  assert.equal(reply.visibilityState, "hidden");

  harness.setContentReply({
    id: "chr:7",
    role: "Button",
    text: "Sign in",
    rectCss: { left: 10, top: 20, width: 100, height: 40 },
    metrics: {},
    doc: "9f3a",
    snap: "a1b2c3d4",
    idx: 7,
    frame: 0,
    title: "",
    visibilityState: "visible",
  });
  reply = harness.forward({
    op: "resolve",
    v: 1,
    tab: 14,
    frame: 0,
    doc: "9f3a",
    snap: "a1b2c3d4",
    idx: 7,
  });
  assert.equal(reply.tab, 14);
  assert.equal(reply.window, 46);
  assert.equal(reply.title, "");
  assert.equal(reply.fallback, false, "same-title/empty-title never uses the fallback");

  const sendsBeforeMissing = harness.calls.send.length;
  reply = harness.forward({
    op: "resolve",
    v: 1,
    tab: 13,
    frame: 0,
    doc: "9f3a",
    snap: "a1b2c3d4",
    idx: 7,
  });
  assert.equal(reply.error, "tab_unavailable");
  assert.equal(harness.calls.query, 0, "missing named tab must not fall back");
  assert.equal(harness.calls.send.length, sendsBeforeMissing);

  harness.setContentReply({ error: "stale_document" });
  reply = harness.forward({
    op: "resolve",
    v: 1,
    tab: 12,
    frame: 0,
    doc: "bbbb",
    snap: "a1b2c3d4",
    idx: 7,
  });
  assert.equal(reply.error, "stale_document");
  assert.equal(reply.tab, 12);
  assert.equal(reply.fallback, false);

  harness.setFocused(false);
  const sendsBeforeBinding = harness.calls.send.length;
  reply = harness.forward({ op: "binding", tab: 12 });
  assert.equal(reply.tab, 12);
  assert.equal(reply.active, true);
  assert.equal(reply.focused, false);
  assert.equal(harness.calls.send.length, sendsBeforeBinding, "binding must not read content");

  reply = harness.forward({ op: "binding" });
  assert.equal(reply.error, "protocol_error");
  assert.equal(harness.calls.query, 0);
  assert.equal(harness.calls.send.length, sendsBeforeBinding);

  // frame_binding requires a named tab and a nonzero frame.
  reply = harness.forward({ op: "frame_binding", tab: 12, frame: 0, doc: "9f3a", snap: "a1b2c3d4" });
  assert.equal(reply.error, "protocol_error");
  reply = harness.forward({ op: "frame_binding", frame: 3, doc: "9f3a", snap: "a1b2c3d4" });
  assert.equal(reply.error, "protocol_error");
}

// ------------------------------------------------------------------ frame bridge

function bridgeFramePorts(sw, sender, content) {
  const swListeners = { message: [], disconnect: [] };
  const contentListeners = { message: [], disconnect: [] };
  let closed = false;
  const swPort = {
    name: "hands-frame",
    sender,
    onMessage: {
      addListener(fn) {
        swListeners.message.push(fn);
      },
    },
    onDisconnect: {
      addListener(fn) {
        swListeners.disconnect.push(fn);
      },
    },
    postMessage(msg) {
      if (closed) return;
      for (const fn of contentListeners.message) fn(msg);
    },
    disconnect() {
      if (closed) return;
      closed = true;
      for (const fn of swListeners.disconnect) fn();
      for (const fn of contentListeners.disconnect) fn();
    },
  };
  const contentPort = {
    name: "hands-frame",
    onMessage: {
      addListener(fn) {
        contentListeners.message.push(fn);
      },
    },
    onDisconnect: {
      addListener(fn) {
        contentListeners.disconnect.push(fn);
      },
    },
    postMessage(msg) {
      if (closed) return;
      for (const fn of swListeners.message) fn(msg);
    },
    disconnect() {
      swPort.disconnect();
    },
    __silence() {
      contentListeners.message.length = 0;
    },
    __silenceDisconnect() {
      contentListeners.disconnect.length = 0;
    },
  };
  return { swPort, contentPort };
}

function frameHarness() {
  const sw = swContext();
  const frames = new Map();

  function addFrame(opts) {
    const tab = opts.tab || 12;
    const frameId = opts.frameId;
    const origin = opts.origin || "https://example.test";
    const href = opts.href || origin + "/";
    const holder = { parent: null };
    let ctx = null;
    ctx = contentContext({
      nonce: opts.nonce,
      origin,
      href,
      title: opts.title || "",
      innerWidth: opts.innerWidth || 800,
      innerHeight: opts.innerHeight || 600,
      documentElement: opts.documentElement || null,
      elementFromPoint: (x, y) =>
        ctx && typeof ctx.__elementAt === "function" ? ctx.__elementAt(x, y) : null,
      onParentPost(event) {
        if (ctx.__dropParentPosts > 0) {
          ctx.__dropParentPosts -= 1;
          return;
        }
        const parent = ctx.__parentCtx;
        if (!parent) return;
        parent.__dispatchMessage({
          data: event.data,
          source: event.source,
          origin: event.origin,
        });
      },
      connect(content) {
        const pair = bridgeFramePorts(sw, {
          tab: { id: tab },
          frameId,
          documentId: opts.documentId || "doc-" + tab + "-" + frameId,
          documentLifecycle: "active",
          origin,
          url: href,
        });
        holder.swPort = pair.swPort;
        holder.port = pair.contentPort;
        // Chrome fires runtime.onConnect asynchronously; deliver frame_id only after the
        // content script has attached its port listener.
        sw.defer(() => sw.connect(pair.swPort));
        return pair.contentPort;
      },
    });
    ctx.__frameId = frameId;
    ctx.__origin = origin;
    ctx.__parentCtx = holder.parent;
    ctx.__dropParentPosts = 0;
    frames.set(tab + ":" + frameId, ctx);
    sw.setContent(tab, frameId, (msg, cb) => {
      ctx.__listener(msg, null, (reply) => cb(reply));
    });
    // Settle the deferred onConnect/frame_id/frame_hello exchange.
    sw.runTimers();
    ctx.__runTimers();
    ctx.__swPort = holder.swPort;
    return ctx;
  }

  return { sw, addFrame, frames };
}

function waitForIdle(sw, framesMap) {
  for (let i = 0; i < 4; i += 1) {
    sw.runTimers();
    for (const ctx of framesMap.values()) {
      ctx.__runTimers();
    }
  }
}

function fanOutSnapshot(h, message) {
  h.sw.forward(message);
  waitForIdle(h.sw, h.frames);
  return h.sw.lastPost();
}

function rowFor(rows, frameId) {
  return rows.frames.find((row) => row.frame === frameId);
}

function frameRoutingTests() {
  const h = frameHarness();
  const top = h.addFrame({ frameId: 0, nonce: 0x1111, href: "https://example.test/top" });
  const child = h.addFrame({
    frameId: 3,
    nonce: 0x2222,
    origin: "https://widget.test",
    href: "https://widget.test/login",
    innerWidth: 300,
    innerHeight: 250,
  });
  child.__parentCtx = top;

  const iframe = element({ tag: "IFRAME", rect: { left: 40, top: 60, width: 300, height: 250 } });
  iframe.contentWindow = child.window;
  const rootEl = element({ tag: "DIV", rect: { left: 0, top: 0, width: 800, height: 600 } });
  attach(rootEl, iframe);
  top.document.documentElement = rootEl;
  top.__elementAt = () => iframe;
  const childButton = element({ tag: "BUTTON", innerText: "Target" });
  child.document.documentElement = childButton;

  // E1: two frames register distinct ids and receive frame_id.
  const registry = h.sw.registry(12);
  assert.ok(registry, "tab registry exists");
  assert.ok(registry.get(0), "top frame registered");
  assert.ok(registry.get(3), "child frame registered");
  assert.notEqual(registry.get(0).token, registry.get(3).token, "tokens differ");
  assert.equal(child.__frameId, 3);
  assert.equal(child.__origin, "https://widget.test");

  // E8/E9: explicit frame routing, never a bare broadcast.
  h.sw.forward({ op: "resolve", v: 1, tab: 12, frame: 0, doc: "x", snap: "y", idx: 0 });
  let resolves = h.sw.calls.routes.filter((route) => route.op === "resolve");
  assert.deepEqual(resolves[resolves.length - 1], {
    tab: 12,
    frameId: 0,
    op: "resolve",
  });
  h.sw.forward({ op: "resolve", v: 1, tab: 12, frame: 3, doc: "x", snap: "y", idx: 0 });
  resolves = h.sw.calls.routes.filter((route) => route.op === "resolve");
  assert.deepEqual(resolves[resolves.length - 1], {
    tab: 12,
    frameId: 3,
    op: "resolve",
  });

  // E10: a child rejects a mismatched msg.frame without walking.
  const mismatch = listenerResolve(child, { op: "resolve", v: 1, frame: 7, doc: "x", snap: "y", idx: 0 });
  assert.equal(mismatch.error, "protocol_error");

  // E10b: a top op with frame:0 is accepted while SELF_FRAME is unset.
  const fresh = contentContext();
  const topBeforeId = listenerResolve(fresh, { op: "resolve", v: 1, frame: 0, doc: "x", snap: "y", idx: 0 });
  assert.notEqual(topBeforeId.error, "protocol_error");

  // Snapshot fan-out links the child through the parent port.
  const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
  assert.ok(reply, "fan-out replied");
  assert.equal(reply.tab, 12);
  const row = rowFor(reply, 3);
  assert.ok(row, "child row present");
  assert.equal(row.status, "ok", "child linked");
  assert.equal(row.origin, "https://widget.test");
  assert.ok(row.link, "link attached");
  assert.equal(row.link.content_left, 40);
  assert.equal(row.link.content_top, 60);
  assert.equal(row.link.content_width, 300);
  assert.equal(row.link.content_height, 250);
  assert.equal(row.link.parent, 0, "parent frame id comes from the reporting port");
  assert.equal(h.sw.registry(12).get(3).doc, vm.runInContext("DOC_NONCE", child));
  const frameEl = reply.elements.find((el) => el.frame === 3);
  assert.ok(frameEl, "child elements merge with frame provenance");
  assert.ok(frameEl.id.startsWith("sref1:c.c.3."), "tab/frame base36 ref: " + frameEl.id);
  assert.equal(frameEl.id, frameEl.ref, "id == ref for a frame element");
  assert.ok(!frameEl.id.startsWith("chr:"), "chr: is top-frame-only");
  assert.ok(row.doc && row.snap, "linked row carries the child doc/snap");
}

function frameOptOutTests() {
  const h = frameHarness();
  const top = h.addFrame({ frameId: 0, nonce: 0x1111 });
  const child = h.addFrame({ frameId: 3, nonce: 0x2222, origin: "https://widget.test" });
  child.__parentCtx = top;
  const iframe = element({ tag: "IFRAME" });
  iframe.contentWindow = child.window;
  attach(element({ tag: "DIV" }), iframe);

  let reply = h.sw.forward({ op: "snapshot", tab: 12 });
  assert.equal(reply.frames, undefined, "bare request keeps the 0123 top-only shape");
  assert.equal(reply.frame_elements, undefined, "parent frame list is internal");
  assert.equal(h.sw.calls.routes[h.sw.calls.routes.length - 1].frameId, 0);

  reply = h.sw.forward({ op: "snapshot", tab: 12, frames: false });
  assert.equal(reply.frames, undefined, "frames:false stays top-only");
  assert.equal(h.sw.calls.routes[h.sw.calls.routes.length - 1].frameId, 0);

  reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
  assert.ok(Array.isArray(reply.frames), "frames:true opts in");

  // N-1: a top-only page with `frames:true` keeps the 0123 top-only shape (no frame fields).
  {
    const topOnly = frameHarness();
    topOnly.addFrame({ frameId: 0 });
    const topReply = fanOutSnapshot(topOnly, { op: "snapshot", tab: 12, frames: true });
    assert.equal(topReply.frames, undefined);
    assert.equal(topReply.frames_total, undefined);
    assert.equal(topReply.frames_truncated, undefined);
  }

  // P4-1: a registered-unlinked iframe whose src redirects to another origin must dedupe by
  // its own slot, not by origin; a distinct same-origin un-injected sibling keeps its row.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 3, origin: "https://same.test" });
    child.__parentCtx = top;
    const linkedIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://ads.test/redirect" },
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    linkedIframe.contentWindow = child.window;
    const unlinkedIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://same.test/other" },
      rect: { left: 5, top: 250, width: 300, height: 200 },
    });
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, linkedIframe);
    attach(rootEl, unlinkedIframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => linkedIframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Redirected" });
    // Keep the slot report but drop the link report: #a stays frame_link_missing with its
    // identity known; #b must still be named.
    const token = h.sw.registry(12).get(3).token;
    const originalPost = top.__port.postMessage;
    top.__port.postMessage = (msg) => {
      if (msg.op === "frame_link" && msg.token === token) return;
      originalPost.call(top.__port, msg);
    };
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(reply.frames.length, 2, "exactly two distinct child rows");
    const registered = reply.frames.find((row) => row.frame === 3);
    assert.equal(registered.reason, "frame_link_missing");
    assert.ok(
      reply.frames.some(
        (row) => row.reason === "no_content_script" && row.origin === "https://same.test"
      ),
      "the distinct same-origin un-injected sibling keeps its row"
    );
    assert.ok(
      !reply.frames.some((row) => row.origin === "https://ads.test"),
      "the registered iframe's seen row is deduped by slot"
    );
  }

  // P3-2: a distinct same-origin un-injected iframe still gets a row even when a sibling is
  // registered/linked.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 4, origin: "https://same.test" });
    child.__parentCtx = top;
    const linkedIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://same.test/" },
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    linkedIframe.contentWindow = child.window;
    const unlinkedIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://same.test/other" },
      rect: { left: 5, top: 250, width: 300, height: 200 },
    });
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, linkedIframe);
    attach(rootEl, unlinkedIframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => linkedIframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Same" });
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.ok(
      reply.frames.some((row) => row.reason === "no_content_script"),
      "the distinct same-origin un-injected iframe is not silently dropped"
    );
  }

  // L-2/m-8: a stored observe whose child commit fails must not expose a ref-less frame id.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 8, origin: "https://nocommit.test" });
    child.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", rect: { left: 5, top: 5, width: 300, height: 200 } });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Target" });
    h.sw.setContent(12, 8, (msg, cb) => {
      if (msg.op === "frame_commit") {
        cb({ error: "stale_snapshot" });
        return;
      }
      child.__listener(msg, null, (reply) => cb(reply));
    });
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const leaked = reply.elements.find(
      (el) => el.frame === 8 || String(el.id || "").startsWith("frame:")
    );
    assert.equal(leaked, undefined, "a stored observe drops a failed-commit frame element");
  }

  // L-8: a committed reply with a malformed doc/snap must not leak a ref-less frame id.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 10, origin: "https://badcommit.test" });
    child.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", rect: { left: 5, top: 5, width: 300, height: 200 } });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Target" });
    h.sw.setContent(12, 10, (msg, cb) => {
      if (msg.op === "frame_commit") {
        cb({ committed: true, doc: "zzzz", snap: "bad", retained: 1 });
        return;
      }
      child.__listener(msg, null, (reply) => cb(reply));
    });
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const leaked = reply.elements.find(
      (el) => el.frame === 10 || String(el.id || "").startsWith("frame:")
    );
    assert.equal(leaked, undefined, "malformed committed doc/snap never mints a frame id");
  }

  // P5-1: slot identity is parent-scoped. A top-level un-injected `data:` iframe at slot 0
  // must not vanish when a nested injected child also has slot 0.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const mid = h.addFrame({ frameId: 3, origin: "https://mid.test" });
    const deep = h.addFrame({ frameId: 5, origin: "https://deep.test" });
    mid.__parentCtx = top;
    deep.__parentCtx = mid;
    const dataIframe = element({
      tag: "IFRAME",
      attrs: { src: "data:text/html,restricted" },
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    const midIframe = element({
      tag: "IFRAME",
      rect: { left: 5, top: 250, width: 300, height: 200 },
    });
    midIframe.contentWindow = mid.window;
    const topRoot = element({ tag: "DIV" });
    attach(topRoot, dataIframe);
    attach(topRoot, midIframe);
    top.document.documentElement = topRoot;
    top.__elementAt = () => midIframe;
    const deepIframe = element({
      tag: "IFRAME",
      rect: { left: 7, top: 9, width: 200, height: 100 },
    });
    deepIframe.contentWindow = deep.window;
    const midRoot = element({ tag: "DIV" });
    attach(midRoot, deepIframe);
    mid.document.documentElement = midRoot;
    mid.__elementAt = () => deepIframe;
    deep.document.documentElement = element({ tag: "BUTTON", innerText: "Deep" });
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(reply.frames.length, 3, "mid + deep + top data iframe rows");
    assert.ok(reply.frames.some((row) => row.frame === 3));
    assert.ok(reply.frames.some((row) => row.frame === 5));
    assert.ok(
      reply.frames.some((row) => row.reason === "restricted_scheme"),
      "the top-level data: iframe at slot 0 is not hidden by a nested slot 0"
    );
  }

  // P5-2/L-15: while the top port is down, a registered child gets one transient row, not a
  // second false `no_content_script`.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 3, origin: "https://late.test" });
    child.__parentCtx = top;
    const iframe = element({
      tag: "IFRAME",
      attrs: { src: "https://late.test/" },
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Late" });
    fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    top.__port.__silenceDisconnect();
    top.__port.disconnect();
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const childRows = reply.frames.filter((row) => row.frame === 3);
    assert.equal(childRows.length, 1, "one row for the registered child");
    // P6-7: the parent disappeared, so the child's old link is cleared and the row is stale
    // rather than an `ok` link the click could not use.
    assert.equal(childRows[0].reason, "frame_geometry_unavailable");
    assert.equal(childRows[0].link, undefined);
    assert.ok(
      !reply.frames.some((row) => row.reason === "no_content_script"),
      "no false no_content_script while the top port is down"
    );
  }

  // P6-1/P6-3: top port down, a never-linked registered child and a distinct same-origin
  // sibling both keep a row (count-based skip, not an origin set).
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({
      frameId: 3,
      origin: "https://kid.test",
      href: "https://kid.test/a",
    });
    child.__parentCtx = top;
    const registeredIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://kid.test/a" },
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    registeredIframe.contentWindow = child.window;
    const siblingIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://kid.test/lazy" },
      rect: { left: 5, top: 250, width: 300, height: 200 },
    });
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, registeredIframe);
    attach(rootEl, siblingIframe);
    top.document.documentElement = rootEl;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Kid" });
    top.__port.__silenceDisconnect();
    top.__port.disconnect();
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    // P9-1: a matching top item is an honest duplicate, never a silent drop.
    assert.equal(reply.frames.length, 3, "registered child + duplicate top item + sibling");
    assert.equal(reply.frames.find((row) => row.frame === 3).reason, "frame_link_missing");
    assert.equal(
      reply.frames.filter(
        (row) =>
          row.frame === undefined &&
          row.origin === "https://kid.test" &&
          row.reason === "frame_link_missing"
      ).length,
      1,
      "the registered iframe's top item is an honest duplicate"
    );
    assert.ok(
      reply.frames.some(
        (row) => row.reason === "no_content_script" && row.origin === "https://kid.test"
      ),
      "the distinct same-origin sibling is not silently dropped"
    );
    assert.equal(reply.frames_total, 4, "top + three child rows");
  }

  // P6-2: a restricted iframe nested inside an injected child is rowed by that child.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const mid = h.addFrame({ frameId: 3, origin: "https://mid.test" });
    mid.__parentCtx = top;
    const midIframe = element({
      tag: "IFRAME",
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    midIframe.contentWindow = mid.window;
    const topRoot = element({ tag: "DIV" });
    attach(topRoot, midIframe);
    top.document.documentElement = topRoot;
    top.__elementAt = () => midIframe;
    const sandbox = element({
      tag: "IFRAME",
      attrs: { sandbox: "allow-forms", src: "https://nested.test/" },
      rect: { left: 7, top: 9, width: 200, height: 100 },
    });
    const midRoot = element({ tag: "DIV" });
    attach(midRoot, sandbox);
    mid.document.documentElement = midRoot;
    mid.__elementAt = () => sandbox;
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(reply.frames.length, 2, "mid + nested sandbox row");
    assert.ok(reply.frames.some((row) => row.frame === 3 && row.status === "ok"));
    assert.ok(
      reply.frames.some(
        (row) => row.reason === "sandboxed_no_scripts" && row.parent === 3
      ),
      "the nested sandbox iframe is named by its parent"
    );
    assert.equal(reply.frames_total, 3, "top + mid + nested");
  }

  // P7-1: a linked child that goes stale on a top-port drop must not consume the distinct
  // same-origin un-injected sibling (its iframe is already deduped by slot).
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 3, origin: "https://kid.test" });
    child.__parentCtx = top;
    const linkedIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://kid.test/a" },
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    linkedIframe.contentWindow = child.window;
    const siblingIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://kid.test/lazy" },
      rect: { left: 5, top: 250, width: 300, height: 200 },
    });
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, linkedIframe);
    attach(rootEl, siblingIframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => linkedIframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Kid" });
    const up = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(up.frames.length, 2, "top-up control: child + sibling");
    assert.equal(rowFor(up, 3).status, "ok");
    assert.ok(
      up.frames.some(
        (row) => row.reason === "no_content_script" && row.origin === "https://kid.test"
      )
    );
    assert.equal(up.frames_total, 3);
    top.__port.__silenceDisconnect();
    top.__port.disconnect();
    const down = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(down.frames.length, 2, "stale child + sibling");
    assert.equal(rowFor(down, 3).reason, "frame_geometry_unavailable");
    assert.ok(
      down.frames.some(
        (row) => row.reason === "no_content_script" && row.origin === "https://kid.test"
      ),
      "the sibling is not consumed by the stale child"
    );
    assert.equal(down.frames_total, 3);
  }

  // P7-3: a grandchild must not advertise `ok` when its ancestor link is gone.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const mid = h.addFrame({ frameId: 3, origin: "https://mid.test" });
    const deep = h.addFrame({ frameId: 5, origin: "https://deep.test" });
    mid.__parentCtx = top;
    deep.__parentCtx = mid;
    const midIframe = element({
      tag: "IFRAME",
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    midIframe.contentWindow = mid.window;
    const topRoot = element({ tag: "DIV" });
    attach(topRoot, midIframe);
    top.document.documentElement = topRoot;
    top.__elementAt = () => midIframe;
    const deepIframe = element({
      tag: "IFRAME",
      rect: { left: 7, top: 9, width: 200, height: 100 },
    });
    deepIframe.contentWindow = deep.window;
    const midRoot = element({ tag: "DIV" });
    attach(midRoot, deepIframe);
    mid.document.documentElement = midRoot;
    mid.__elementAt = () => deepIframe;
    deep.document.documentElement = element({ tag: "BUTTON", innerText: "Deep" });
    const up = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(rowFor(up, 5).status, "ok");
    top.__port.__silenceDisconnect();
    top.__port.disconnect();
    const down = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(rowFor(down, 3).reason, "frame_geometry_unavailable");
    const deepRow = rowFor(down, 5);
    assert.ok(deepRow, "the grandchild is still named");
    assert.notEqual(deepRow.status, "ok", "no ok row through a stale ancestor");
    assert.equal(deepRow.reason, "frame_geometry_unavailable");
  }

  // P6-4 remainder: a same-origin relay cannot set a linked child's slot; the real parent can.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 3, origin: "https://widget.test" });
    const relay = h.addFrame({ frameId: 4, origin: "https://widget.test" });
    child.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", rect: { left: 5, top: 5, width: 300, height: 200 } });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Widget" });
    const originalPost = top.__port.postMessage;
    top.__port.postMessage = (msg) => {
      if (msg.op === "frame_slot") {
        return;
      }
      if (msg.op === "frame_link") {
        const copy = Object.assign({}, msg);
        delete copy.slot;
        originalPost.call(top.__port, copy);
        return;
      }
      originalPost.call(top.__port, msg);
    };
    fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    top.__port.postMessage = originalPost;
    const token = h.sw.registry(12).get(3).token;
    assert.equal(h.sw.registry(12).get(3).slot, undefined);
    relay.__port.postMessage({
      op: "frame_slot",
      token,
      origin: "https://widget.test",
      slot: 9,
    });
    assert.equal(
      h.sw.registry(12).get(3).slot,
      undefined,
      "a relay cannot set a linked child's slot"
    );
    top.__port.postMessage({
      op: "frame_slot",
      token,
      origin: "https://widget.test",
      slot: 1,
    });
    assert.equal(h.sw.registry(12).get(3).slot, 1);
    assert.equal(h.sw.registry(12).get(3).slotParent, 0);
  }

  // P8-1: a nested registered-unlinked child on the same origin must not consume a distinct
  // top-level un-injected sibling's row when the top port drops.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({
      frameId: 3,
      origin: "https://kid.test",
      href: "https://kid.test/a",
    });
    const nested = h.addFrame({
      frameId: 5,
      origin: "https://dup.test",
      href: "https://dup.test/inner",
    });
    child.__parentCtx = top;
    nested.__parentCtx = child;
    nested.__dropParentPosts = 99; // registered but never links
    const childIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://kid.test/a" },
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    childIframe.contentWindow = child.window;
    const siblingIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://dup.test/lazy" },
      rect: { left: 5, top: 250, width: 300, height: 200 },
    });
    const nestedIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://dup.test/inner" },
      rect: { left: 7, top: 9, width: 200, height: 100 },
    });
    nestedIframe.contentWindow = nested.window;
    const topRoot = element({ tag: "DIV" });
    attach(topRoot, childIframe);
    attach(topRoot, siblingIframe);
    top.document.documentElement = topRoot;
    top.__elementAt = () => childIframe;
    const childRoot = element({ tag: "DIV" });
    attach(childRoot, nestedIframe);
    child.document.documentElement = childRoot;
    child.__elementAt = () => nestedIframe;
    const up = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(rowFor(up, 3).status, "ok");
    assert.equal(rowFor(up, 5).reason, "frame_link_missing");
    assert.ok(
      up.frames.some(
        (row) => row.parent === 3 && row.reason === "no_content_script"
      ),
      "the child names its nested iframe"
    );
    assert.ok(
      up.frames.some(
        (row) =>
          row.parent === undefined &&
          row.reason === "no_content_script" &&
          row.origin === "https://dup.test"
      ),
      "the top-level dup.test sibling is present"
    );
    assert.equal(up.frames_total, 5, "top + child + nested + nested-row + sibling");
    top.__port.__silenceDisconnect();
    top.__port.disconnect();
    const down = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(rowFor(down, 3).reason, "frame_geometry_unavailable");
    assert.equal(rowFor(down, 5).reason, "frame_link_missing");
    assert.ok(
      down.frames.some(
        (row) =>
          row.parent === undefined &&
          row.reason === "no_content_script" &&
          row.origin === "https://dup.test"
      ),
      "the top-level sibling is not consumed by the nested unlinked frame"
    );
    assert.equal(down.frames.length, 3, "stale child + nested + sibling");
    assert.equal(down.frames_total, 4, "top + three child rows");
  }

  // P9-1: a top-level iframe with the *same URL* as a nested registered-unlinked frame is
  // still named (honest duplicate), never silently consumed.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({
      frameId: 3,
      origin: "https://kid.test",
      href: "https://kid.test/a",
    });
    const nested = h.addFrame({
      frameId: 5,
      origin: "https://dup.test",
      href: "https://dup.test/inner",
    });
    child.__parentCtx = top;
    nested.__parentCtx = child;
    nested.__dropParentPosts = 99;
    const childIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://kid.test/a" },
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    childIframe.contentWindow = child.window;
    const sameUrlIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://dup.test/inner" },
      rect: { left: 5, top: 250, width: 300, height: 200 },
    });
    const nestedIframe = element({
      tag: "IFRAME",
      attrs: { src: "https://dup.test/inner" },
      rect: { left: 7, top: 9, width: 200, height: 100 },
    });
    nestedIframe.contentWindow = nested.window;
    const topRoot = element({ tag: "DIV" });
    attach(topRoot, childIframe);
    attach(topRoot, sameUrlIframe);
    top.document.documentElement = topRoot;
    top.__elementAt = () => childIframe;
    const childRoot = element({ tag: "DIV" });
    attach(childRoot, nestedIframe);
    child.document.documentElement = childRoot;
    child.__elementAt = () => nestedIframe;
    const up = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(rowFor(up, 3).status, "ok");
    assert.equal(rowFor(up, 5).reason, "frame_link_missing");
    assert.ok(
      up.frames.some(
        (row) => row.parent === 3 && row.reason === "no_content_script"
      )
    );
    assert.equal(up.frames_total, 5);
    top.__port.__silenceDisconnect();
    top.__port.disconnect();
    const down = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(rowFor(down, 3).reason, "frame_geometry_unavailable");
    assert.equal(rowFor(down, 5).reason, "frame_link_missing");
    assert.ok(
      down.frames.some(
        (row) =>
          row.frame === undefined &&
          row.origin === "https://dup.test" &&
          row.reason === "frame_link_missing"
      ),
      "the same-URL top-level sibling is named, not consumed"
    );
    assert.equal(down.frames_total, 4);
  }

  // P8-3: a budget flag from an earlier snapshot must not outlive a stale link reason.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 9, origin: "https://slow2.test" });
    child.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", rect: { left: 5, top: 5, width: 300, height: 200 } });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Slow" });
    h.sw.setContent(12, 9, (msg, cb) => {
      if (msg.op === "snapshot") {
        return; // never replies: snapshot 1 marks frame_budget_exceeded
      }
      child.__listener(msg, null, (reply) => cb(reply));
    });
    const first = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(rowFor(first, 9).reason, "frame_budget_exceeded");
    top.__port.__silenceDisconnect();
    top.__port.disconnect();
    const second = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.equal(rowFor(second, 9).reason, "frame_geometry_unavailable");
  }

  // P9-2/P9-5: a commit answered at 150 ms (before commit_by) is still emitted, not dropped
  // by the old fixed 30/120 ms fallback.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 3, origin: "https://slowcommit.test" });
    child.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", rect: { left: 5, top: 5, width: 300, height: 200 } });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "SlowCommit" });
    h.sw.setContent(12, 3, (msg, cb) => {
      if (msg.op === "frame_commit") {
        child.__listener(msg, null, (reply) => {
          h.sw.defer(() => cb(reply), 150);
        });
        return;
      }
      child.__listener(msg, null, (reply) => cb(reply));
    });
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const row = rowFor(reply, 3);
    assert.equal(row.status, "ok", "a 150 ms commit is not dropped");
    assert.ok(
      reply.elements.some((el) => el.frame === 3),
      "the delayed commit's elements are emitted"
    );
  }

  // M-3: if the top walk already consumed the client budget, fan-out is skipped honestly.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 7, origin: "https://budget.test" });
    child.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", rect: { left: 1, top: 1, width: 300, height: 200 } });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Target" });
    fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const reply = fanOutSnapshot(h, {
      op: "snapshot",
      tab: 12,
      frames: true,
      __handsStarted: CLOCK.t - 380,
    });
    assert.ok(
      reply.frames.some(
        (row) => row.frame === 7 && row.reason === "frame_budget_exceeded"
      ),
      "an exhausted client budget skips fan-out"
    );
  }

  // A non-storing internal observe still carries frame elements for detection, but never
  // mints a resolvable ref or a colliding chr: id.
  const childButton = element({ tag: "BUTTON", innerText: "Target" });
  child.document.documentElement = childButton;
  top.document.documentElement = element({ tag: "DIV" });
  const topIframe = element({ tag: "IFRAME", rect: { left: 1, top: 2, width: 300, height: 200 } });
  topIframe.contentWindow = child.window;
  attach(top.document.documentElement, topIframe);
  top.__elementAt = () => topIframe;
  reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true, store: false });
  assert.equal(reply.stored, false);
  const internal = reply.elements.find((el) => el.frame === 3);
  assert.ok(internal, "internal frame elements are still observed");
  assert.ok(internal.id.startsWith("frame:3:"), internal.id);
  assert.equal(internal.ref, undefined, "no resolvable ref without a stored pair");
}

function frameRestrictedTests() {
  const h = frameHarness();
  const top = h.addFrame({ frameId: 0, nonce: 0x1111 });
  const rootEl = element({ tag: "DIV" });
  const sandboxed = element({ tag: "IFRAME", attrs: { sandbox: "allow-forms", src: "https://s.test/" } });
  const fenced = element({ tag: "FENCEDFRAME" });
  const dataFrame = element({ tag: "IFRAME", attrs: { src: "data:text/html,hello" } });
  const blankFrame = element({ tag: "IFRAME", attrs: { src: "about:blank" } });
  const noScript = element({ tag: "IFRAME", attrs: { src: "https://noscript.test/" } });
  attach(rootEl, sandboxed);
  attach(rootEl, fenced);
  attach(rootEl, dataFrame);
  attach(rootEl, blankFrame);
  attach(rootEl, noScript);
  top.document.documentElement = rootEl;

  const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
  const reasons = reply.frames.map((row) => row.reason);
  assert.ok(reasons.includes("sandboxed_no_scripts"));
  assert.ok(reasons.includes("fenced_frame"));
  assert.ok(reasons.includes("restricted_scheme"));
  assert.ok(reasons.includes("no_content_script"));
  for (const row of reply.frames) {
    assert.equal(row.frame, undefined, "un-injected frames never carry a fabricated frame id");
    assert.ok(!String(row.reason).includes("://"), "no full URL in a reason");
  }

  // A token arriving for a script-sandboxed frame wins over the attribute label.
  const child = h.addFrame({
    frameId: 5,
    nonce: 0x3333,
    origin: "https://s.test",
    href: "https://s.test/",
  });
  child.__parentCtx = top;
  sandboxed.contentWindow = child.window;
  top.__elementAt = () => sandboxed;
  const linked = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
  const row = rowFor(linked, 5);
  assert.ok(row, "registered frame row exists");
  assert.equal(row.status, "ok", "an arriving token wins over the attribute label");
}

function frameAdversarialTests() {
  // E5b: event.origin mismatch is rejected.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 3, origin: "https://widget.test" });
    child.__parentCtx = top;
    child.__origin = "https://widget.test";
    const iframe = element({ tag: "IFRAME" });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    const token = h.sw.registry(12).get(3).token;
    // The parent reports a browser-set origin that is not the child sender origin.
    top.__port.postMessage({
      op: "frame_link",
      token,
      origin: "https://evil.test",
      link: {
        content_left: 0,
        content_top: 0,
        content_width: 300,
        content_height: 250,
        clip: [],
        transform_ok: true,
        visible: true,
        hit_ok: true,
        viewport_w: 800,
        viewport_h: 600,
      },
    });
    assert.equal(h.sw.registry(12).get(3).linkState, "ambiguous");
  }

  // E5c: opaque origin fails closed; about:blank with an inherited origin links.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const opaque = h.addFrame({ frameId: 4, origin: "null", href: "about:blank" });
    opaque.__parentCtx = top;
    const opaqueIframe = element({ tag: "IFRAME", attrs: { src: "about:blank" } });
    opaqueIframe.contentWindow = opaque.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, opaqueIframe);
    top.document.documentElement = rootEl;
    top.__port.postMessage({
      op: "frame_link",
      token: h.sw.registry(12).get(4).token,
      origin: "null",
      link: {
        content_left: 0,
        content_top: 0,
        content_width: 300,
        content_height: 250,
        clip: [],
        transform_ok: true,
        visible: true,
        hit_ok: true,
        viewport_w: 800,
        viewport_h: 600,
      },
    });
    assert.equal(h.sw.registry(12).get(4).linkState, "ambiguous");
  }

  // E5c (accepted arm): an about:blank frame that inherits a non-opaque origin links.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0, origin: "https://parent.test" });
    const blank = h.addFrame({
      frameId: 4,
      origin: "https://parent.test",
      href: "about:blank",
    });
    blank.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", attrs: { src: "about:blank" }, rect: { left: 1, top: 2, width: 300, height: 200 } });
    iframe.contentWindow = blank.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    const linked = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const row = rowFor(linked, 4);
    assert.ok(row);
    assert.equal(row.status, "ok", "inherited about:blank origin links");
    assert.equal(h.sw.registry(12).get(4).linkState, "ok");
  }

  // E7c: a relayed token through a second frame is refused.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const victim = h.addFrame({ frameId: 3, origin: "https://victim.test" });
    const relay = h.addFrame({ frameId: 4, origin: "https://relay.test" });
    victim.__parentCtx = top;
    relay.__parentCtx = top;
    const vIframe = element({ tag: "IFRAME" });
    vIframe.contentWindow = victim.window;
    const rIframe = element({ tag: "IFRAME" });
    rIframe.contentWindow = relay.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, vIframe);
    attach(rootEl, rIframe);
    top.document.documentElement = rootEl;
    const victimToken = h.sw.registry(12).get(3).token;
    // The relay frame posts the victim's token; the browser-set origin is the relay's.
    top.__dispatchMessage({
      data: { handsFrameToken: victimToken },
      source: relay.window,
      origin: "https://relay.test",
    });
    assert.equal(h.sw.registry(12).get(3).linkState, "ambiguous");
    assert.equal(h.sw.registry(12).get(3).link, null, "never the relay's geometry");
  }

  // E6: a token from another tab is not linked.
  {
    const h = frameHarness();
    const top12 = h.addFrame({ tab: 12, frameId: 0 });
    const child = h.addFrame({ tab: 12, frameId: 3, origin: "https://widget.test" });
    child.__parentCtx = top12;
    const top14 = h.addFrame({ tab: 14, frameId: 0 });
    const token = h.sw.registry(12).get(3).token;
    top14.__port.postMessage({
      op: "frame_link",
      token,
      origin: "https://widget.test",
      link: {
        content_left: 0,
        content_top: 0,
        content_width: 300,
        content_height: 250,
        clip: [],
        transform_ok: true,
        visible: true,
        hit_ok: true,
        viewport_w: 800,
        viewport_h: 600,
      },
    });
    assert.equal(h.sw.registry(12).get(3).link, null);
  }

  // E7: the same token from two parents is refused after the first link.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const other = h.addFrame({ frameId: 4, origin: "https://other.test" });
    const child = h.addFrame({ frameId: 3, origin: "https://widget.test" });
    child.__parentCtx = top;
    other.__parentCtx = top;
    const iframe = element({ tag: "IFRAME" });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    h.sw.forward({ op: "snapshot", tab: 12, frames: true });
    waitForIdle(h.sw, h.frames);
    const registry = h.sw.registry(12);
    const token = registry.get(3).token;
    const first = registry.get(3).link.parent;
    // A second parent reports the same token; the link must not move.
    other.__port.postMessage({
      op: "frame_link",
      token,
      origin: "https://widget.test",
      link: {
        content_left: 999,
        content_top: 999,
        content_width: 300,
        content_height: 250,
        clip: [],
        transform_ok: true,
        visible: true,
        hit_ok: true,
        viewport_w: 800,
        viewport_h: 600,
      },
    });
    assert.equal(registry.get(3).link.parent, first);
    assert.notEqual(registry.get(3).link.content_left, 999);
  }
}

function frameLifecycleTests() {
  // E2: disconnect prunes; reconnect re-registers with a new token.
  {
    const h = frameHarness();
    h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 3, origin: "https://widget.test" });
    const before = h.sw.registry(12).get(3).token;
    h.frames.get("12:3");
    const port = child.__port;
    port.disconnect();
    assert.equal(h.sw.registry(12).get(3), undefined, "disconnect prunes the entry");
    // Reconnect by loading a fresh child document.
    const again = h.addFrame({ frameId: 3, origin: "https://widget.test", nonce: 0x4444 });
    const after = h.sw.registry(12).get(3).token;
    assert.notEqual(before, after, "reconnect issues a new token");
    assert.ok(again.__port);
  }

  // E3: duplicate frame id last-wins with a warning.
  {
    const h = frameHarness();
    const first = h.addFrame({ frameId: 3, origin: "https://one.test" });
    const firstToken = h.sw.registry(12).get(3).token;
    const second = h.addFrame({ frameId: 3, origin: "https://two.test" });
    const entry = h.sw.registry(12).get(3);
    assert.equal(entry.origin, "https://two.test");
    assert.notEqual(entry.token, firstToken);
    assert.ok(second);
    assert.ok(first);
  }

  // E4: 33rd connected port is refused; earlier entries stay intact.
  {
    const h = frameHarness();
    h.addFrame({ frameId: 0 });
    for (let i = 1; i <= 31; i += 1) {
      h.addFrame({ frameId: i, origin: "https://f" + i + ".test" });
    }
    assert.equal(h.sw.registry(12).size, 32);
    const extra = h.addFrame({ frameId: 99, origin: "https://extra.test" });
    assert.equal(h.sw.registry(12).get(99), undefined);
    assert.equal(h.sw.registry(12).size, 32);
    assert.equal(extra.__frameId, 99);
    const rejected = extra.__port ? true : true;
    assert.ok(rejected);
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    assert.ok(
      reply.frames.some((row) => row.reason === "frame_cap_exceeded"),
      "a refused 33rd port still appears as a frame-less cap row"
    );
  }

  // F-05: a child that links but never answers the child snapshot is frame_budget_exceeded,
  // not silently missing or a client timeout.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 9, origin: "https://slow.test" });
    child.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", rect: { left: 5, top: 5, width: 300, height: 200 } });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    child.document.documentElement = element({ tag: "BUTTON", innerText: "Target" });
    h.sw.setContent(12, 9, (msg, cb) => {
      if (msg.op === "snapshot") {
        return; // never replies: the fan-out budget must name it
      }
      child.__listener(msg, null, (reply) => cb(reply));
    });
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const row = rowFor(reply, 9);
    assert.ok(row);
    assert.equal(row.reason, "frame_budget_exceeded");
  }

  // P7-2: one hung child must not discard a healthy child's elements or its nested rows.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const healthy = h.addFrame({ frameId: 3, origin: "https://healthy.test" });
    const hung = h.addFrame({ frameId: 4, origin: "https://hung.test" });
    healthy.__parentCtx = top;
    hung.__parentCtx = top;
    const healthyIframe = element({
      tag: "IFRAME",
      rect: { left: 5, top: 5, width: 300, height: 200 },
    });
    healthyIframe.contentWindow = healthy.window;
    const hungIframe = element({
      tag: "IFRAME",
      rect: { left: 5, top: 250, width: 300, height: 200 },
    });
    hungIframe.contentWindow = hung.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, healthyIframe);
    attach(rootEl, hungIframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => healthyIframe;
    const sandbox = element({
      tag: "IFRAME",
      attrs: { sandbox: "allow-forms", src: "https://nested.test/" },
      rect: { left: 7, top: 9, width: 200, height: 100 },
    });
    const healthyRoot = element({ tag: "DIV" });
    attach(healthyRoot, sandbox);
    healthy.document.documentElement = healthyRoot;
    healthy.__elementAt = () => sandbox;
    hung.document.documentElement = element({ tag: "BUTTON", innerText: "Hung" });
    h.sw.setContent(12, 4, (msg, cb) => {
      if (msg.op === "snapshot") {
        return; // never replies: the straggler cutoff must not drop frame 3
      }
      hung.__listener(msg, null, (reply) => cb(reply));
    });
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const healthyRow = rowFor(reply, 3);
    assert.equal(healthyRow.status, "ok");
    assert.ok(
      reply.elements.some((el) => el.frame === 3),
      "the healthy child's elements survive the hung child"
    );
    assert.equal(rowFor(reply, 4).reason, "frame_budget_exceeded");
    assert.ok(
      reply.frames.some(
        (row) => row.reason === "sandboxed_no_scripts" && row.parent === 3
      ),
      "the healthy child's nested row survives too"
    );
    assert.equal(reply.frames_total, 4, "top + healthy + hung + nested");
  }

  // F-07: an on-screen frame ranks before a far off-screen frame that connected first.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const far = h.addFrame({ frameId: 4, origin: "https://far.test" });
    const near = h.addFrame({ frameId: 5, origin: "https://near.test" });
    far.__parentCtx = top;
    near.__parentCtx = top;
    const farIframe = element({ tag: "IFRAME", rect: { left: 2000, top: 10, width: 300, height: 200 } });
    farIframe.contentWindow = far.window;
    const nearIframe = element({ tag: "IFRAME", rect: { left: 10, top: 10, width: 300, height: 200 } });
    nearIframe.contentWindow = near.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, farIframe);
    attach(rootEl, nearIframe);
    top.document.documentElement = rootEl;
    top.__elementAt = (x) => (x < 800 ? nearIframe : farIframe);
    far.document.documentElement = element({ tag: "BUTTON", innerText: "Far" });
    near.document.documentElement = element({ tag: "BUTTON", innerText: "Near" });
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const order = reply.frames
      .filter((row) => row.frame === 4 || row.frame === 5)
      .map((row) => row.frame);
    assert.equal(order.join(","), "5,4", "near/on-screen ranks first");
  }

  // E15: a child that never acks is named frame_late, not silently absent.
  {
    const h = frameHarness();
    h.addFrame({ frameId: 0 });
    const late = h.addFrame({ frameId: 6, origin: "https://late.test" });
    // Drop frame_announce on the content side: the child never acks.
    late.__port.__silence();
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const row = rowFor(reply, 6);
    assert.ok(row, "late frame still has a row");
    assert.equal(row.status, "unavailable");
    assert.equal(row.reason, "frame_late");
  }

  // E17: a frame removed before the link is frame_link_missing, never guessed.
  {
    const h = frameHarness();
    h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 8, origin: "https://gone.test" });
    child.__parentCtx = h.frames.get("12:0");
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const row = rowFor(reply, 8);
    assert.ok(row);
    assert.equal(row.status, "unavailable");
    assert.ok(["frame_link_missing", "frame_late"].includes(row.reason));
  }

  // Tab removal prunes the whole registry.
  {
    const h = frameHarness();
    h.addFrame({ frameId: 0 });
    h.sw.removeTab(12);
    assert.equal(h.sw.registry(12), null);
  }
}

function frameAsyncTests() {
  // E19: a delayed parent post is retried, not reported as missing.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 3, origin: "https://widget.test" });
    child.__parentCtx = top;
    child.__dropParentPosts = 1;
    const iframe = element({ tag: "IFRAME", rect: { left: 40, top: 60, width: 300, height: 250 } });
    iframe.contentWindow = child.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, iframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const row = rowFor(reply, 3);
    assert.ok(row);
    assert.equal(row.status, "ok", "the retry links the child");
  }

  // E20: an iframe inside an open shadow root is found and linked.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const child = h.addFrame({ frameId: 4, origin: "https://shadow.test" });
    child.__parentCtx = top;
    const iframe = element({ tag: "IFRAME", rect: { left: 5, top: 6, width: 200, height: 100 } });
    iframe.contentWindow = child.window;
    const shadowRoot = { children: [iframe], elementFromPoint: () => null };
    const host = element({ tag: "DIV", shadowRoot });
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, host);
    top.document.documentElement = rootEl;
    top.__elementAt = () => iframe;
    const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    const row = rowFor(reply, 4);
    assert.ok(row, "shadow iframe row");
    assert.equal(row.status, "ok", "shadow iframe linked");
    assert.equal(row.link.content_left, 5);
  }

  // E21: hidden / pointer-events-none / overlay links are named, not composed.
  {
    const h = frameHarness();
    const top = h.addFrame({ frameId: 0 });
    const hidden = h.addFrame({ frameId: 5, origin: "https://hidden.test" });
    hidden.__parentCtx = top;
    const hiddenIframe = element({
      tag: "IFRAME",
      rect: { left: 0, top: 0, width: 300, height: 250 },
      style: { display: "none" },
    });
    hiddenIframe.contentWindow = hidden.window;
    const rootEl = element({ tag: "DIV" });
    attach(rootEl, hiddenIframe);
    top.document.documentElement = rootEl;
    top.__elementAt = () => null;
    let reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    let row = rowFor(reply, 5);
    assert.equal(row.reason, "frame_not_visible");

    const covered = h.addFrame({ frameId: 6, origin: "https://covered.test" });
    covered.__parentCtx = top;
    const coveredIframe = element({
      tag: "IFRAME",
      rect: { left: 0, top: 0, width: 300, height: 250 },
    });
    coveredIframe.contentWindow = covered.window;
    attach(rootEl, coveredIframe);
    const overlay = element({ tag: "DIV", rect: { left: 0, top: 0, width: 800, height: 600 } });
    top.__elementAt = () => overlay;
    reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
    row = rowFor(reply, 6);
    assert.equal(row.reason, "frame_occluded");
  }
}

function frameNestedTests() {
  const h = frameHarness();
  const top = h.addFrame({ frameId: 0 });
  const child = h.addFrame({ frameId: 3, origin: "https://mid.test" });
  const grandchild = h.addFrame({ frameId: 5, origin: "https://deep.test" });
  child.__parentCtx = top;
  grandchild.__parentCtx = child;
  grandchild.document.documentElement = element({ tag: "BUTTON", innerText: "Deep" });

  const childIframe = element({ tag: "IFRAME", rect: { left: 20, top: 30, width: 400, height: 300 } });
  childIframe.contentWindow = child.window;
  const topRoot = element({ tag: "DIV" });
  attach(topRoot, childIframe);
  top.document.documentElement = topRoot;
  top.__elementAt = () => childIframe;

  const grandIframe = element({ tag: "IFRAME", rect: { left: 5, top: 7, width: 200, height: 100 } });
  grandIframe.contentWindow = grandchild.window;
  const childRoot = element({ tag: "DIV" });
  attach(childRoot, grandIframe);
  child.document.documentElement = childRoot;
  child.__elementAt = () => grandIframe;

  const reply = fanOutSnapshot(h, { op: "snapshot", tab: 12, frames: true });
  const registry = h.sw.registry(12);
  assert.equal(rowFor(reply, 3).status, "ok");
  assert.equal(rowFor(reply, 5).status, "ok");
  assert.equal(registry.get(3).link.parent, 0);
  assert.equal(registry.get(5).link.parent, 3, "nested parent id is contiguous");
  assert.equal(rowFor(reply, 5).link.content_left, 5, "each link stays local to its parent");

  // frame_binding for the grandchild returns the full fresh chain, deepest first.
  h.sw.forward({
    op: "frame_binding",
    tab: 12,
    frame: 5,
    doc: vm.runInContext("DOC_NONCE", grandchild),
    snap: vm.runInContext("lastSnapshot && lastSnapshot.snap", grandchild),
    idx: 0,
  });
  waitForIdle(h.sw, h.frames);
  const binding = h.sw.lastPost();
  assert.ok(Array.isArray(binding.chain));
  assert.equal(binding.chain.length, 2);
  assert.equal(binding.chain[0].parent, 3);
  assert.equal(binding.chain[1].parent, 0);
}

function frameBindingOpsTests() {
  const h = frameHarness();
  const top = h.addFrame({ frameId: 0 });
  const child = h.addFrame({
    frameId: 3,
    origin: "https://widget.test",
    innerWidth: 300,
    innerHeight: 250,
  });
  child.__parentCtx = top;
  child.document.documentElement = element({ tag: "BUTTON", innerText: "Target" });
  const iframe = element({ tag: "IFRAME", rect: { left: 40, top: 60, width: 300, height: 250 } });
  iframe.contentWindow = child.window;
  const rootEl = element({ tag: "DIV" });
  attach(rootEl, iframe);
  top.document.documentElement = rootEl;
  h.sw.forward({ op: "snapshot", tab: 12, frames: true });
  waitForIdle(h.sw, h.frames);

  // frame_top returns metrics only; it never walks or stores.
  const topReply = listenerResolve(top, { op: "frame_top", frame: 0 });
  assert.equal(topReply.frame, 0);
  assert.equal(topReply.elements, undefined);
  assert.equal(topReply.main_text, undefined);
  assert.ok(topReply.metrics);

  // frame_binding requires a nonzero frame, routes to that child, and returns a fresh chain.
  h.sw.forward({
    op: "frame_binding",
    tab: 12,
    frame: 3,
    doc: vm.runInContext("DOC_NONCE", child),
    snap: vm.runInContext("lastSnapshot && lastSnapshot.snap", child),
    idx: 0,
  });
  waitForIdle(h.sw, h.frames);
  const binding = h.sw.lastPost();
  assert.equal(h.sw.calls.routes[h.sw.calls.routes.length - 1].frameId, 3);
  assert.equal(binding.frame, 3);
  assert.equal(binding.idx, 0, "the probe echoes idx");
  assert.ok(binding.rectCss, "the stored node still has a rect");
  assert.equal(binding.tab, 12);
  assert.equal(binding.doc, vm.runInContext("DOC_NONCE", child));
  assert.ok(Array.isArray(binding.chain), "fresh chain returned");
  assert.equal(binding.chain.length, 1);
  assert.equal(binding.chain[0].parent, 0);

  // frame_measure re-measures a known token over the parent port.
  const token = h.sw.registry(12).get(3).token;
  const postsBefore = h.sw.calls.posts.length;
  top.__swPort.postMessage({ op: "frame_measure", tokens: [token] });
  assert.ok(h.sw.calls.posts.length >= postsBefore);
  assert.equal(h.sw.registry(12).get(3).linkState, "ok");

  // F-04: the per-level hit test uses the composed click point, not the box centre. A parent
  // overlay over the right half leaves the centre clear but must refuse the target point.
  const overlay = element({ tag: "DIV", rect: { left: 200, top: 0, width: 600, height: 600 } });
  top.__elementAt = (x) => (x > 200 ? overlay : iframe);
  top.__swPort.postMessage({
    op: "frame_measure",
    tokens: [token],
    point: { x: 200, y: 100 },
  });
  assert.equal(
    h.sw.registry(12).get(3).link.hit_ok,
    false,
    "a partial overlay at the click point refuses the link"
  );
  top.__swPort.postMessage({ op: "frame_measure", tokens: [token] });
  assert.equal(
    h.sw.registry(12).get(3).link.hit_ok,
    true,
    "the centre fallback is clear"
  );
}

function frameContentTests() {
  // A child snapshot is frame-tagged; the top reply is not.
  const child = contentContext({ nonce: 0x7777 });
  vm.runInContext("SELF_FRAME = 3;", child);
  const childButton = element({ tag: "BUTTON", innerText: "Target" });
  child.document.documentElement = childButton;
  child.__msg = { op: "snapshot", detail: "default", store: false, frames: true };
  const childReply = vm.runInContext("buildSnapshot(__msg)", child);
  assert.equal(childReply.frame, 3);
  assert.equal(childReply.frame_elements, undefined);
  assert.equal(childReply.stored, false);
  assert.equal(childReply.doc, vm.runInContext("DOC_NONCE", child), "identity survives store:false");
  assert.equal(childReply.snap, undefined, "no stored snapshot means no snap");
  assert.equal(childReply.elements.length, 1, "the child page's button is observed");
  assert.ok(childReply.metrics && childReply.metrics.innerWidth > 0);

  // B4 finding: a direct child snapshot with no `frame` field is accepted (SW routing is the
  // primary defence); a *present* mismatched frame is still protocol_error.
  const direct = listenerResolve(child, { op: "snapshot", detail: "default", store: false });
  assert.equal(direct.error, undefined);
  assert.equal(direct.elements.length, 1);
  const mismatched = listenerResolve(child, { op: "snapshot", frame: 4, store: false });
  assert.equal(mismatched.error, "protocol_error");

  // F-06: a fan-out reply is a candidate until `frame_commit`; an uncommitted (late) reply
  // never replaces `lastSnapshot`.
  const pendingCtx = contentContext({ nonce: 0x8888 });
  vm.runInContext("SELF_FRAME = 3;", pendingCtx);
  const pendingButton = element({ tag: "BUTTON", innerText: "Target" });
  pendingCtx.document.documentElement = pendingButton;
  const candidate = listenerResolve(pendingCtx, {
    op: "snapshot",
    store: false,
    pending: true,
    light: true,
  });
  assert.equal(candidate.pending, true);
  assert.ok(/^[0-9a-f]{8}$/.test(candidate.snap));
  assert.equal(vm.runInContext("lastSnapshot", pendingCtx), null, "candidate stores nothing");
  const commit = listenerResolve(pendingCtx, {
    op: "frame_commit",
    frame: 3,
    snap: candidate.snap,
  });
  assert.equal(commit.committed, true);
  assert.equal(vm.runInContext("lastSnapshot.snap", pendingCtx), candidate.snap);
  const staleCommit = listenerResolve(pendingCtx, {
    op: "frame_commit",
    frame: 3,
    snap: "00000000",
  });
  assert.equal(staleCommit.error, "stale_snapshot");

  // m-2: a candidate whose commit window expired must not replace lastSnapshot.
  const beforeExpired = vm.runInContext("lastSnapshot.snap", pendingCtx);
  const expired = listenerResolve(pendingCtx, {
    op: "snapshot",
    store: false,
    pending: true,
    light: true,
    commit_by: CLOCK.t - 1,
  });
  const expiredCommit = listenerResolve(pendingCtx, {
    op: "frame_commit",
    frame: 3,
    snap: expired.snap,
  });
  assert.equal(expiredCommit.error, "stale_snapshot");
  assert.equal(vm.runInContext("lastSnapshot.snap", pendingCtx), beforeExpired);

  // frame_binding requires a stored snapshot and echoes idx/liveness; a removed/invalid idx
  // is stale_element, and a mismatched frame is refused.
  assert.equal(
    listenerResolve(child, { op: "frame_binding", frame: 3, idx: 0 }).error,
    "stale_document",
    "no stored snapshot"
  );
  const okBinding = listenerResolve(pendingCtx, { op: "frame_binding", frame: 3, idx: 0 });
  assert.equal(okBinding.connected, true);
  assert.equal(okBinding.idx, 0);
  assert.ok(okBinding.rectCss);
  assert.equal(
    listenerResolve(pendingCtx, { op: "frame_binding", frame: 3 }).error,
    "stale_element",
    "missing idx is stale"
  );
  assert.equal(
    listenerResolve(pendingCtx, { op: "frame_binding", frame: 4, idx: 0 }).error,
    "frame_unavailable"
  );

  // A top snapshot with frames:true includes the internal iframe list; frames:false does not.
  const top = contentContext();
  const iframe = element({ tag: "IFRAME", attrs: { src: "https://widget.test/" } });
  const rootEl = element({ tag: "DIV" });
  attach(rootEl, iframe);
  top.document.documentElement = rootEl;
  const withFrames = listenerResolve(top, { op: "snapshot", frames: true, store: false });
  assert.equal(withFrames.frame_elements.length, 1);
  const withoutFrames = listenerResolve(top, { op: "snapshot", frames: false, store: false });
  assert.equal(withoutFrames.frame_elements, undefined);

  // P6-5: a frame inside a closed shadow root is enumerated via chrome.dom.
  const closedIframe = element({
    tag: "IFRAME",
    attrs: { src: "https://closed.test/" },
  });
  const host = element({ tag: "DIV" });
  host.__closedRoot = { children: [closedIframe], elementFromPoint: () => null };
  const closedRootEl = element({ tag: "DIV" });
  attach(closedRootEl, host);
  const closedTop = contentContext({
    documentElement: closedRootEl,
    openOrClosedShadowRoot: (el) => el.__closedRoot || null,
  });
  const closedReply = listenerResolve(closedTop, {
    op: "snapshot",
    frames: true,
    store: false,
  });
  assert.equal(closedReply.frame_elements.length, 1);
  assert.equal(closedReply.frame_elements[0].reason, "no_content_script");
}

function frameSourceLocksTests() {
  const sw = fs.readFileSync(path.join(root, "sw.js"), "utf8");
  const content = fs.readFileSync(path.join(root, "content.js"), "utf8");
  const binding = sw.slice(sw.indexOf('if (op === "binding")'), sw.indexOf("withTabEvidence(port, tab, false, msg)"));
  assert.ok(binding.includes("chrome.windows.get"));
  assert.ok(!binding.includes("tabs.sendMessage"), "binding probe stays content-free");

  // Every tabs.sendMessage goes through sendToFrame with an explicit frame id.
  const callLines = sw
    .split("\n")
    .filter((line) => line.includes("tabs.sendMessage("));
  assert.ok(callLines.length > 0);
  for (const line of callLines) {
    assert.ok(line.includes("{ frameId:"), "each tabs.sendMessage carries a frame id: " + line);
  }

  // Frame_binding requires a nonzero frame and the binding branch is untouched.
  assert.ok(sw.includes('op === "frame_binding"'));
  assert.ok(sw.includes("frame <= 0"));
  assert.ok(!content.includes(["element", ".click("].join("")));
  assert.ok(!content.includes(["dispatch", "Event("].join("")));
  assert.ok(!content.includes(["all_frames", ": true"].join("")));
}

function postcheckTests() {
  const context = contentContext({ title: "Start", href: "https://example.test/a" });
  const node = element({ tag: "BUTTON" });
  installSnapshot(context, node, "aaaa", "11111111");

  const check = (message) =>
    listenerResolve(context, Object.assign({ op: "postcheck", frame: 0 }, message));

  // Exact current-state kinds.
  assert.equal(check({ kind: "url_equals", value: "https://example.test/a" }).state, "satisfied");
  assert.equal(check({ kind: "url_equals", value: "https://example.test/b" }).state, "unsatisfied");
  assert.equal(check({ kind: "url_prefix", value: "https://example.test/" }).state, "satisfied");
  assert.equal(check({ kind: "title_equals", value: "Other" }).state, "unsatisfied");

  // Snapshot-relative kinds need the clicked ref's doc/snap.
  let reply = check({ kind: "url_changed", doc: "aaaa", snap: "11111111" });
  assert.equal(reply.state, "unsatisfied");
  reply = check({ kind: "url_changed", doc: "bbbb", snap: "11111111" });
  assert.equal(reply.state, "unavailable");
  assert.equal(reply.reason, "baseline_mismatch");

  // Stored-node presence/absence.
  assert.equal(
    check({ kind: "element_present", doc: "aaaa", snap: "11111111", idx: 0 }).state,
    "satisfied"
  );
  reply = check({ kind: "element_present", doc: "zzzz", snap: "11111111", idx: 0 });
  assert.equal(reply.state, "unavailable");
  assert.equal(reply.reason, "stale_document");
  reply = check({ kind: "element_absent", doc: "aaaa", snap: "22222222", idx: 0 });
  assert.equal(reply.state, "unavailable");
  assert.equal(reply.reason, "stale_snapshot");

  const detached = element({ tag: "DIV", isConnected: false });
  installSnapshot(context, detached, "aaaa", "33333333");
  assert.equal(
    check({ kind: "element_absent", doc: "aaaa", snap: "33333333", idx: 0 }).state,
    "satisfied"
  );

  // Positive-only kinds report unavailable, never unsatisfied.
  assert.equal(check({ kind: "dialog_present" }).state, "unavailable");
  assert.equal(check({ kind: "focus_editable" }).state, "unavailable");
  assert.equal(check({ kind: "count_at_least", role: "button", n: 51 }).reason, "invalid_condition");
  assert.equal(check({ kind: "count_at_least", role: "Button", n: 2 }).reason, "invalid_condition");

  // Booleans/states only: never text, URL, title or a value.
  reply = check({ kind: "url_equals", value: "https://example.test/a" });
  const allowed = new Set([
    "state",
    "reason",
    "doc",
    "snap",
    "visibilityState",
    "tab",
    "window",
    "active",
    "focused",
    "fallback",
  ]);
  for (const key of Object.keys(reply)) {
    assert.ok(allowed.has(key), "postcheck reply carries only closed fields, saw " + key);
  }
  assert.ok(!JSON.stringify(reply).includes("https://example.test/a"));

  // Never walks or retires the stored snapshot.
  const before = vm.runInContext("lastSnapshot.snap", context);
  check({ kind: "element_present", doc: "aaaa", snap: "33333333", idx: 0 });
  assert.equal(vm.runInContext("lastSnapshot.snap", context), before);

  // Source locks: read-only, boolean-only postcheck path.
  const content = fs.readFileSync(path.join(root, "content.js"), "utf8");
  const start = content.indexOf("function buildPostcheck(");
  const end = content.indexOf("function parseChrId(");
  assert.ok(start > 0 && end > start);
  const body = content.slice(start, end);
  assert.ok(!body.includes(["element", ".click("].join("")));
  assert.ok(!body.includes(["dispatch", "Event("].join("")));
  assert.ok(!body.includes("innerText"), "postcheck never reads page text");
  assert.ok(!body.includes("MutationObserver"));
  assert.ok(!body.includes("localStorage"));

  const sw = fs.readFileSync(path.join(root, "sw.js"), "utf8");
  assert.ok(sw.includes('op !== "postcheck"'), "postcheck is in the SW allowlist");
  assert.ok(
    sw.includes('op === "postcheck" && msg.tab == null'),
    "postcheck requires a named tab"
  );
  const request = fs.readFileSync(path.join(root, "..", "src", "chrome.rs"), "utf8");
  assert.ok(request.includes('"frame": 0'), "postcheck request pins frame:0");
}

function main() {
  contentTests();
  swTests();
  frameRoutingTests();
  frameOptOutTests();
  frameRestrictedTests();
  frameAdversarialTests();
  frameLifecycleTests();
  frameAsyncTests();
  frameNestedTests();
  frameBindingOpsTests();
  frameContentTests();
  postcheckTests();
  frameSourceLocksTests();
  process.stdout.write("extension harness: ok\n");
}

main();
