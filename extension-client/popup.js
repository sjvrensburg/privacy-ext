const $ = (id) => document.getElementById(id);
const status = (m, cls) => { const s = $("status"); s.textContent = m; s.className = "save-state" + (cls ? " " + cls : ""); };

let currentHostname = null;

async function getCurrentHostname() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.url) return null;
  try {
    return new URL(tab.url).hostname || null;
  } catch {
    return null;
  }
}

function setOriginChip(active) {
  $("originToggle").checked = active;
  $("originChip").classList.toggle("on", active);
  $("originChipText").textContent = active ? "Redaction is ON for this site" : "Redaction is OFF for this site";
}

async function refreshOriginToggle() {
  currentHostname = await getCurrentHostname();
  if (!currentHostname) {
    $("hostname").textContent = "(no active tab)";
    $("originToggle").disabled = true;
    return;
  }
  $("hostname").textContent = currentHostname;
  const s = await chrome.storage.local.get(["originSettings", "defaultOriginActive"]);
  const active = s.originSettings?.[currentHostname]?.active ?? (s.defaultOriginActive ?? false);
  setOriginChip(active);
}

$("originToggle").addEventListener("change", async () => {
  if (!currentHostname) return;
  const active = $("originToggle").checked;
  setOriginChip(active);
  const s = await chrome.storage.local.get(["originSettings"]);
  const originSettings = { ...(s.originSettings || {}), [currentHostname]: { active } };
  await chrome.storage.local.set({ originSettings });
  renderSiteList();
});

async function renderSiteList() {
  const s = await chrome.storage.local.get(["originSettings"]);
  const originSettings = s.originSettings || {};
  const hosts = [...new Set([...DEFAULT_AI_SITES, ...Object.keys(originSettings)])];
  const list = $("siteList");
  list.innerHTML = "";
  for (const h of hosts) {
    const active = originSettings[h]?.active ?? false;
    const chip = document.createElement("label");
    chip.className = "chip" + (active ? " on" : "");
    chip.innerHTML = `<input type="checkbox" ${active ? "checked" : ""}/>${h}`;
    const cb = chip.querySelector("input");
    cb.addEventListener("change", async () => {
      chip.classList.toggle("on", cb.checked);
      const cur = await chrome.storage.local.get(["originSettings"]);
      const merged = { ...(cur.originSettings || {}), [h]: { active: cb.checked } };
      await chrome.storage.local.set({ originSettings: merged });
      if (h === currentHostname) setOriginChip(cb.checked);
    });
    list.appendChild(chip);
  }
}

(async () => {
  const s = await chrome.storage.local.get(["enabled", "threshold"]);
  $("enabled").checked = s.enabled !== false;
  const threshold = s.threshold ?? 0.55;
  $("threshold").value = threshold;
  $("threshVal").textContent = Number(threshold).toFixed(2);
  await refreshOriginToggle();
  await renderSiteList();
})();

$("threshold").addEventListener("input", () => {
  $("threshVal").textContent = Number($("threshold").value).toFixed(2);
});

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
