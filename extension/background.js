// MV3 service worker: owns the offscreen document (which hosts the model) and
// brokers classify requests from content scripts / popup to it.
//
// The service worker itself can't run onnxruntime (no DOM, killed when idle), so
// all inference lives in the offscreen document. The worker only ensures that
// document exists and relays messages.

const DEFAULT_MODEL_BASE =
  "https://huggingface.co/SemplificaAI/gliner2-privacy-filter-PII-multi/resolve/main/fp16_v2";

let creating = null; // de-dupe concurrent offscreen creation

async function ensureOffscreen() {
  const has = await chrome.offscreen.hasDocument?.();
  if (has) return;
  if (creating) { await creating; return; }
  creating = chrome.offscreen.createDocument({
    url: "offscreen.html",
    reasons: ["WORKERS"],
    justification: "Run the on-device PII detection model (onnxruntime-web).",
  });
  try { await creating; } catch (e) {
    // Another context may have created it first; ignore "single document" races.
    if (!String(e?.message || e).includes("Only a single offscreen")) throw e;
  } finally { creating = null; }
}

function sendToOffscreen(message, tries = 20) {
  // Retry briefly: the offscreen document may still be evaluating its module
  // when we first message it.
  return new Promise((resolve, reject) => {
    const attempt = (n) => {
      chrome.runtime.sendMessage(message, (res) => {
        const err = chrome.runtime.lastError;
        if (!err) return resolve(res);
        if (n <= 0) return reject(new Error(err.message));
        setTimeout(() => attempt(n - 1), 100);
      });
    };
    attempt(tries);
  });
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg?.type !== "PF_CLASSIFY") return; // ignore PF_*_OFFSCREEN / PF_PING etc.
  (async () => {
    try {
      await ensureOffscreen();
      const cfg = await chrome.storage.local.get(["modelBaseUrl", "threshold"]);
      const res = await sendToOffscreen({
        type: "PF_CLASSIFY_OFFSCREEN",
        text: msg.text,
        threshold: msg.threshold ?? cfg.threshold ?? 0.55,
        modelBaseUrl: (cfg.modelBaseUrl && cfg.modelBaseUrl.trim()) || DEFAULT_MODEL_BASE,
      });
      sendResponse(res);
    } catch (e) {
      sendResponse({ ok: false, error: String(e?.message || e) });
    }
  })();
  return true; // async response
});
