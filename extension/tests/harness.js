/* Executable harness for the read-only extension logic.
 * Run with: node extension/tests/harness.js
 * No network, no browser, no synthetic DOM input.
 */
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const root = path.resolve(__dirname, "..");

function element(options) {
  const {
    tag = "BUTTON",
    attrs = {},
    innerText = "",
    contentEditable = false,
    type = "",
  } = options || {};
  return {
    nodeType: 1,
    tagName: tag,
    type,
    innerText,
    isContentEditable: contentEditable,
    isConnected: true,
    href: attrs.href || "",
    getAttribute(name) {
      return Object.prototype.hasOwnProperty.call(attrs, name) ? attrs[name] : null;
    },
    hasAttribute(name) {
      return Object.prototype.hasOwnProperty.call(attrs, name);
    },
    getBoundingClientRect() {
      return { left: 10, top: 20, width: 100, height: 40 };
    },
    matches() {
      return true;
    },
    closest() {
      return null;
    },
  };
}

function contentContext() {
  const context = {
    console,
    Math,
    Date,
    Number,
    String,
    Array,
    Object,
    JSON,
    Uint32Array,
    setTimeout() {
      return 0;
    },
    chrome: {
      runtime: {
        onMessage: {
          addListener(listener) {
            context.__listener = listener;
          },
        },
      },
      dom: {},
    },
    crypto: {
      getRandomValues(array) {
        array[0] = 0x12345678;
        return array;
      },
    },
    window: {
      screen: {},
      visualViewport: null,
      innerWidth: 800,
      innerHeight: 600,
      outerWidth: 800,
      outerHeight: 600,
      screenX: 0,
      screenY: 0,
      getComputedStyle() {
        return {};
      },
    },
    document: {
      title: "",
      visibilityState: "visible",
      documentElement: null,
      body: null,
      querySelector() {
        return null;
      },
      querySelectorAll() {
        return [];
      },
      elementFromPoint() {
        return null;
      },
    },
    location: { href: "https://example.test/", search: "" },
    performance: { now: () => 0 },
    CSS: { escape: (value) => value },
  };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(path.join(root, "content.js"), "utf8"), context, {
    filename: "content.js",
  });
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

function swContext() {
  const calls = { query: 0, get: 0, windows: 0, send: [], posts: [] };
  const tabs = {
    12: { id: 12, windowId: 45, active: true, url: "https://example.test/", title: "Same title" },
    14: { id: 14, windowId: 46, active: true, url: "https://other.test/", title: "Same title" },
  };
  const port = {
    onMessage: { addListener() {} },
    onDisconnect: { addListener() {} },
    postMessage(message) {
      calls.posts.push(message);
    },
  };
  let focused = true;
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
    setTimeout() {
      return 0;
    },
    chrome: {
      runtime: {
        connectNative() {
          return port;
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
        sendMessage(id, _message, callback) {
          calls.send.push(id);
          callback(contentReply);
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
    setFocused(value) {
      focused = value;
    },
    setActive(tabId, value) {
      tabs[tabId].active = value;
    },
    setContentReply(value) {
      contentReply = value;
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
}

function main() {
  contentTests();
  swTests();
  process.stdout.write("extension harness: ok\n");
}

main();
