const $ = (id) => document.getElementById(id);
const status = (m, cls) => { const s = $("status"); s.textContent = m; s.className = cls || ""; };

(async () => {
  const s = await chrome.storage.local.get(["enabled", "serverUrl", "token", "threshold"]);
  $("enabled").checked = s.enabled !== false;
  $("serverUrl").value = s.serverUrl ?? "http://127.0.0.1:8731";
  $("token").value = s.token ?? "";
  $("threshold").value = s.threshold ?? "0.55";
})();

async function saveSettings() {
  await chrome.storage.local.set({
    enabled: $("enabled").checked,
    serverUrl: $("serverUrl").value.trim(),
    token: $("token").value.trim(),
    threshold: parseFloat($("threshold").value) || 0.55,
  });
}

$("save").addEventListener("click", async () => {
  await saveSettings();
  status("Saved.", "ok");
});

// Round-trips a sample through the daemon via the background worker.
// Persist the current form values first so you don't have to Save separately.
$("test").addEventListener("click", async () => {
  await saveSettings();
  status("Testing…");
  chrome.runtime.sendMessage(
    { type: "PF_CLASSIFY", text: "Contact Jane Doe at jane@example.com." },
    (res) => {
      if (chrome.runtime.lastError) return status("Error: " + chrome.runtime.lastError.message, "err");
      if (!res?.ok) return status("Error: " + (res?.error || "unknown"), "err");
      status(`OK — found ${res.entities.length} item(s): "${res.redacted}"`, "ok");
    }
  );
});
