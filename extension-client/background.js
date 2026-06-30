// Thin client: the content script asks the background to call the local daemon
// (background can do cross-origin fetch to 127.0.0.1 via host_permissions; a
// content script would be blocked by the page's CSP/CORS).

async function classify(text) {
  const { serverUrl, token, threshold } = await chrome.storage.local.get([
    "serverUrl", "token", "threshold",
  ]);
  const base = (serverUrl && serverUrl.trim()) || "http://127.0.0.1:8731";
  const headers = { "Content-Type": "application/json" };
  if (token) headers["Authorization"] = `Bearer ${token}`;
  const resp = await fetch(base.replace(/\/$/, "") + "/classify", {
    method: "POST",
    headers,
    body: JSON.stringify({ text, threshold: threshold ?? 0.55 }),
  });
  if (!resp.ok) throw new Error(`daemon ${resp.status}: ${await resp.text()}`);
  return resp.json(); // { entities, redacted }
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg?.type !== "PF_CLASSIFY") return;
  classify(msg.text)
    .then((data) => sendResponse({ ok: true, ...data }))
    .catch((e) => sendResponse({ ok: false, error: String(e?.message || e) }));
  return true; // async
});
