const $ = (id) => document.getElementById(id);
const status = (m, cls) => { const s = $("status"); s.textContent = m; s.className = cls || ""; };

(async () => {
  const s = await chrome.storage.local.get(["enabled", "threshold"]);
  $("enabled").checked = s.enabled !== false;
  $("threshold").value = s.threshold ?? "0.55";
})();

async function saveSettings() {
  await chrome.storage.local.set({
    enabled: $("enabled").checked,
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

// Live connection chip, Zotero-connector style: a small colored dot that
// polls /health via the background worker so the user always sees whether
// the tray daemon is reachable, without having to click "Test connection".
function setChip(state, text) {
  $("chipDot").className = `dot ${state}`;
  $("chipText").textContent = text;
}

function refreshChip() {
  setChip("checking", "Checking…");
  chrome.runtime.sendMessage({ type: "PF_HEALTH" }, (res) => {
    if (chrome.runtime.lastError) {
      setChip("disconnected", "Extension error");
      return;
    }
    if (res?.ok) {
      setChip("connected", `Connected (port ${res.port})`);
    } else if (res?.paired) {
      setChip("disconnected", "Paired, but daemon unreachable");
    } else {
      setChip("disconnected", res?.error || "Not paired");
    }
  });
}

$("repair").addEventListener("click", () => {
  setChip("checking", "Re-pairing…");
  chrome.runtime.sendMessage({ type: "PF_REPAIR" }, (res) => {
    if (chrome.runtime.lastError || !res?.ok) {
      setChip("disconnected", res?.error || chrome.runtime.lastError?.message || "Pairing failed");
      return;
    }
    refreshChip();
  });
});

refreshChip();
const chipInterval = setInterval(refreshChip, 3000);
window.addEventListener("unload", () => clearInterval(chipInterval));
