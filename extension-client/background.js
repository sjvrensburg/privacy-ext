// Thin client: the content script asks the background to call the local daemon
// (background can do cross-origin fetch to 127.0.0.1 via host_permissions; a
// content script would be blocked by the page's CSP/CORS).
//
// Zero-config pairing: rather than the user typing a URL/token into the popup,
// we ask the Privacy Redactor tray app for its port + token via Chrome Native
// Messaging (see desktop/src-tauri/src/bin/pii-native-host.rs). The result is
// cached in storage.local and reused until a request comes back 401 (token
// rotated) or the popup asks us to re-pair.

const NATIVE_HOST = "ai.semplifica.privacy_redactor";

function sendNativeMessage(message, timeoutMs = 3000) {
  return new Promise((resolve) => {
    let settled = false;
    const done = (result) => {
      if (settled) return;
      settled = true;
      resolve(result);
    };
    try {
      chrome.runtime.sendNativeMessage(NATIVE_HOST, message, (resp) => {
        if (chrome.runtime.lastError || !resp) {
          done({ ok: false, error: chrome.runtime.lastError?.message || "no response from native host" });
          return;
        }
        if (!resp.ok) {
          done({ ok: false, error: resp.error || "pairing failed" });
          return;
        }
        done({ ok: true, port: resp.port, token: resp.token });
      });
    } catch (e) {
      done({ ok: false, error: String(e?.message || e) });
    }
    setTimeout(() => done({ ok: false, error: "native host timed out — is Privacy Redactor running?" }), timeoutMs);
  });
}

async function pair() {
  const result = await sendNativeMessage({ cmd: "pair" });
  if (!result.ok) throw new Error(result.error);
  const pairing = { port: result.port, token: result.token || null };
  await chrome.storage.local.set({ pairing });
  return pairing;
}

async function getPairing({ forceRepair = false } = {}) {
  if (!forceRepair) {
    const { pairing } = await chrome.storage.local.get(["pairing"]);
    if (pairing) return pairing;
  }
  return pair();
}

function daemonUrl(pairing, path) {
  return `http://127.0.0.1:${pairing.port}${path}`;
}

function authHeaders(pairing) {
  return pairing.token ? { Authorization: `Bearer ${pairing.token}` } : {};
}

async function classify(text) {
  const { threshold } = await chrome.storage.local.get(["threshold"]);
  let pairing = await getPairing();

  const doFetch = (p) =>
    fetch(daemonUrl(p, "/classify"), {
      method: "POST",
      headers: { "Content-Type": "application/json", ...authHeaders(p) },
      body: JSON.stringify({ text, threshold: threshold ?? 0.55 }),
    });

  let resp = await doFetch(pairing);
  if (resp.status === 401) {
    // Token likely rotated in the tray app since we last paired; re-pair once.
    pairing = await getPairing({ forceRepair: true });
    resp = await doFetch(pairing);
  }
  if (!resp.ok) throw new Error(`daemon ${resp.status}: ${await resp.text()}`);
  return resp.json(); // { entities, redacted }
}

async function checkHealth() {
  try {
    const pairing = await getPairing();
    const resp = await fetch(daemonUrl(pairing, "/health"), { headers: authHeaders(pairing) });
    return { ok: resp.ok, paired: true, port: pairing.port };
  } catch (e) {
    return { ok: false, paired: false, error: String(e?.message || e) };
  }
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg?.type === "PF_CLASSIFY") {
    classify(msg.text)
      .then((data) => sendResponse({ ok: true, ...data }))
      .catch((e) => sendResponse({ ok: false, error: String(e?.message || e) }));
    return true; // async
  }
  if (msg?.type === "PF_HEALTH") {
    checkHealth().then(sendResponse);
    return true;
  }
  if (msg?.type === "PF_REPAIR") {
    getPairing({ forceRepair: true })
      .then((p) => sendResponse({ ok: true, ...p }))
      .catch((e) => sendResponse({ ok: false, error: String(e?.message || e) }));
    return true;
  }
});
