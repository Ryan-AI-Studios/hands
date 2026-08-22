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

function sendToContent(port, tabId, msg) {
  chrome.tabs.sendMessage(tabId, msg, (reply) => {
    const last = chrome.runtime.lastError;
    try {
      if (last) {
        port.postMessage({ error: "no-content" });
      } else {
        port.postMessage(reply || { error: "empty" });
      }
    } catch (_err) {}
  });
}

function forward(port, msg) {
  const op = msg && msg.op;
  if (op !== "snapshot" && op !== "resolve") {
    return;
  }
  chrome.tabs.query({ active: true, lastFocusedWindow: true }, (tabs) => {
    const tab = tabs && tabs[0];
    if (tab && tab.id != null) {
      sendToContent(port, tab.id, msg);
      return;
    }
    chrome.tabs.query({ active: true, windowType: "normal" }, (rest) => {
      const fallback = rest && rest[0];
      if (!fallback || fallback.id == null) {
        try {
          port.postMessage({ error: "no-tab" });
        } catch (_err) {}
        return;
      }
      sendToContent(port, fallback.id, msg);
    });
  });
}

connectHost();
chrome.runtime.onStartup.addListener(connectHost);
chrome.runtime.onInstalled.addListener(connectHost);

function keepWorker() {
  chrome.runtime.getPlatformInfo(() => {
    setTimeout(keepWorker, 20000);
  });
}
keepWorker();
