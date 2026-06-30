// Offscreen document: hosts the model and answers classify requests from the
// service worker.
//
// The message listener is registered SYNCHRONOUSLY at module top so the offscreen
// document can receive requests the instant it loads. The heavy pipeline (ort +
// transformers + model) is imported lazily on the first request.
//
// NOTE: offscreen documents are limited mostly to chrome.runtime — chrome.storage
// is NOT available here, so config (modelBaseUrl/threshold) is passed in by the
// background service worker, which can read storage.

let pipe = null;   // { init, classify }
let ready = null;  // in-flight init promise

async function ensureReady(modelBaseUrl) {
  if (ready) return ready;
  ready = (async () => {
    pipe = await import("./pipeline.js");
    await pipe.init(modelBaseUrl);
  })().catch((e) => { ready = null; throw e; });
  return ready;
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg?.type !== "PF_CLASSIFY_OFFSCREEN") return;
  (async () => {
    try {
      await ensureReady(msg.modelBaseUrl);
      const entities = await pipe.classify(msg.text, msg.threshold ?? 0.55);
      sendResponse({ ok: true, entities });
    } catch (e) {
      sendResponse({ ok: false, error: String(e?.message || e) });
    }
  })();
  return true; // async
});
