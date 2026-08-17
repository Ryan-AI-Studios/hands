const HOST_NAME = "com.helpinghands.host";

function connectHost() {
  let port;
  try {
    port = chrome.runtime.connectNative(HOST_NAME);
  } catch (_err) {
    return;
  }
  port.onMessage.addListener((msg) => {
    forward(port, msg);
  });
  port.onDisconnect.addListener(() => {});
}

function forward(port, msg) {
  const op = msg && msg.op;
  if (op !== "snapshot" && op !== "resolve") {
    return;
  }
  chrome.tabs.query({ active: true, lastFocusedWindow: true }, (tabs) => {
    const tab = tabs && tabs[0];
    if (!tab || tab.id == null) {
      try {
        port.postMessage({ error: "no-tab" });
      } catch (_err) {}
      return;
    }
    chrome.tabs.sendMessage(tab.id, msg, (reply) => {
      const last = chrome.runtime.lastError;
      try {
        if (last) {
          port.postMessage({ error: "no-content" });
        } else {
          port.postMessage(reply || { error: "empty" });
        }
      } catch (_err) {}
    });
  });
}

connectHost();
