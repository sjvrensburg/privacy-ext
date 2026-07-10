// Thin client: the content script asks the background to call the local daemon
// (background can do cross-origin fetch to 127.0.0.1 via host_permissions; a
// content script would be blocked by the page's CSP/CORS).
//
// Zero-config pairing: rather than the user typing a URL/token into the popup,
// we ask the ClipCloak tray app for its port + token via Chrome Native
// Messaging (see desktop/src-tauri/src/bin/clipcloak-native-host.rs). The result is
// cached in storage.local and reused until a request comes back 401 (token
// rotated) or the popup asks us to re-pair.

// Chrome runs this file as a service worker and pulls in the shared site list
// via importScripts. Firefox loads it as a classic background script (see the
// Firefox manifest's `background.scripts`), where importScripts doesn't exist
// and ai-sites.js is already loaded ahead of us — so guard the call.
if (typeof importScripts === "function") importScripts("ai-sites.js");

const NATIVE_HOST = "ai.semplifica.clipcloak";

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
    setTimeout(() => done({ ok: false, error: "native host timed out — is ClipCloak running?" }), timeoutMs);
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
  if (msg?.type === "PF_TAB_STATE") {
    // Sent by the content script once it resolves whether redaction is active
    // for this origin, so the toolbar badge reflects per-tab state without
    // needing the "tabs" permission (sender.tab is available on messages
    // from content scripts regardless of that permission).
    const tabId = sender.tab?.id;
    if (tabId != null) {
      // Explicit ON/OFF (not blank) so the state is unambiguous without
      // opening the popup — a blank badge could otherwise be misread as
      // "not loaded yet" rather than "redaction is off for this site".
      chrome.action.setBadgeText({ tabId, text: msg.active ? "ON" : "OFF" });
      chrome.action.setBadgeBackgroundColor({ tabId, color: msg.active ? "#22c55e" : "#94a3b8" });
      // Swap the toolbar icon between the full-colour and greyed variants so
      // the ON/OFF state is legible even where the badge text is cramped.
      chrome.action.setIcon({
        tabId,
        path: msg.active
          ? { 16: "icons/icon-16.png", 32: "icons/icon-32.png", 48: "icons/icon-48.png" }
          : { 16: "icons/off-16.png", 32: "icons/off-32.png", 48: "icons/off-48.png" },
      });
      chrome.action.setTitle({
        tabId,
        title: msg.active ? "ClipCloak — active on this site" : "ClipCloak — inactive on this site",
      });
    }
    return false;
  }
});

// Seed the curated AI-site list on first install, and merge in any newly
// added curated sites on update without clobbering the user's own toggles.
chrome.runtime.onInstalled.addListener(async (details) => {
  if (details.reason === "install") {
    const originSettings = Object.fromEntries(DEFAULT_AI_SITES.map((h) => [h, { active: true }]));
    await chrome.storage.local.set({
      originSettings,
      defaultOriginActive: false,
      seedVersion: AI_SITES_SEED_VERSION,
    });
  } else if (details.reason === "update") {
    const stored = await chrome.storage.local.get(["originSettings", "seedVersion", "defaultOriginActive"]);
    const seedVersion = stored.seedVersion ?? 0;
    const patch = {};
    if (seedVersion < AI_SITES_SEED_VERSION) {
      const merged = { ...(stored.originSettings || {}) };
      for (const h of DEFAULT_AI_SITES) if (!(h in merged)) merged[h] = { active: true };
      patch.originSettings = merged;
      patch.seedVersion = AI_SITES_SEED_VERSION;
    }
    // Migrating from a pre-per-site build (no defaultOriginActive was ever
    // persisted): that version redacted on every site. Fail safe for a privacy
    // tool — keep redacting broadly rather than silently narrowing to the
    // curated list; the user can switch sites off via the per-site toggle.
    if (!("defaultOriginActive" in stored)) {
      patch.defaultOriginActive = true;
    }
    if (Object.keys(patch).length) await chrome.storage.local.set(patch);
  }
});
