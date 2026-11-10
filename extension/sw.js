const HOST_NAME = "com.helpinghands.host";

let hostPort = null;
let reconnectTimer = null;

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

function withTabEvidence(port, tab, fallback, msg) {
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
    chrome.tabs.sendMessage(tab.id, msg, (reply) => {
      if (chrome.runtime.lastError) {
        post(port, Object.assign({ error: "no_content" }, evidence));
        return;
      }
      if (!reply) {
        post(port, Object.assign({ error: "empty" }, evidence));
        return;
      }
      const out = {};
      for (const key in reply) {
        if (Object.prototype.hasOwnProperty.call(reply, key)) {
          out[key] = reply[key];
        }
      }
      Object.assign(out, evidence);
      post(port, out);
    });
  });
}

function forward(port, msg) {
  const op = msg && msg.op;
  if (
    op !== "snapshot" &&
    op !== "resolve" &&
    op !== "assess" &&
    op !== "binding"
  ) {
    post(port, { error: "unsupported-op" });
    return;
  }
  if (op === "binding" && msg.tab == null) {
    post(port, { error: "protocol_error" });
    return;
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
      withTabEvidence(port, fallback, true, msg);
    });
  });
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
