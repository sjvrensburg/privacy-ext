const $ = (id) => document.getElementById(id);
const status = (m) => { $("status").textContent = m; };

(async () => {
  const s = await chrome.storage.local.get(["enabled", "threshold", "modelBaseUrl"]);
  $("enabled").checked = s.enabled !== false;
  $("threshold").value = s.threshold ?? "0.55";
  $("modelBaseUrl").value = s.modelBaseUrl ?? "";
})();

$("save").addEventListener("click", async () => {
  await chrome.storage.local.set({
    enabled: $("enabled").checked,
    threshold: parseFloat($("threshold").value) || 0.55,
    modelBaseUrl: $("modelBaseUrl").value.trim(),
  });
  status("Saved.");
});

// Warm the model by sending a tiny classify request (triggers offscreen + download).
$("warm").addEventListener("click", () => {
  status("Loading model (first run downloads ~620 MB, then cached)…");
  chrome.runtime.sendMessage({ type: "PF_CLASSIFY", text: "warmup" }, (res) => {
    if (chrome.runtime.lastError) status("Error: " + chrome.runtime.lastError.message);
    else status(res?.ok ? "Model ready." : "Error: " + (res?.error || "unknown"));
  });
});
