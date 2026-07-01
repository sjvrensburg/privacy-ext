const { invoke } = window.__TAURI__.core;

const el = (id) => document.getElementById(id);
let allLabels = [];

// Pretty display names for the raw GLiNER2 labels.
const LABEL_NAMES = {
  "name": "Name",
  "street address": "Street address",
  "email": "Email",
  "phone_num": "Phone",
  "id_num": "ID number",
  "url": "URL",
  "username": "Username",
};
const labelName = (l) => LABEL_NAMES[l] || l;

function renderLabels(enabled) {
  const set = new Set(enabled);
  el("labels").innerHTML = "";
  for (const l of allLabels) {
    const chip = document.createElement("label");
    chip.className = "chip" + (set.has(l) ? " on" : "");
    chip.innerHTML = `<input type="checkbox" ${set.has(l) ? "checked" : ""}/>${labelName(l)}`;
    const cb = chip.querySelector("input");
    cb.addEventListener("change", () => chip.classList.toggle("on", cb.checked));
    chip.dataset.label = l;
    el("labels").appendChild(chip);
  }
}

function collectEnabledLabels() {
  return [...el("labels").querySelectorAll(".chip")]
    .filter((c) => c.querySelector("input").checked)
    .map((c) => c.dataset.label);
}

function applyView(view) {
  allLabels = view.all_labels;
  const c = view.config;
  el("port").value = c.port;
  el("token").value = c.token;
  el("threshold").value = c.threshold;
  el("threshVal").textContent = Number(c.threshold).toFixed(2);
  el("autostart").checked = c.autostart;
  renderLabels(c.enabled_labels);
  el("portHint").classList.toggle("hidden", !view.restart_needed);
  setStatus(view.model_ready, c.port);
}

function setStatus(ready, port) {
  const dot = el("statusDot");
  dot.className = "dot " + (ready ? "ready" : "loading");
  el("statusText").textContent = ready ? "Daemon running" : "Loading model…";
  el("portBadge").textContent = `127.0.0.1:${port}`;
}

function randomToken() {
  const bytes = new Uint8Array(24);
  crypto.getRandomValues(bytes);
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function save() {
  const config = {
    port: parseInt(el("port").value, 10) || 8731,
    token: el("token").value.trim(),
    threshold: parseFloat(el("threshold").value),
    enabled_labels: collectEnabledLabels(),
    autostart: el("autostart").checked,
  };
  try {
    const view = await invoke("save_config", { config });
    applyView(view);
    const s = el("saveState");
    s.textContent = view.restart_needed ? "Saved — restart to apply port" : "Saved ✓";
    s.className = "save-state ok";
    setTimeout(() => (s.textContent = ""), 3000);
  } catch (e) {
    const s = el("saveState");
    s.textContent = "Error: " + e;
    s.className = "save-state";
  }
}

el("threshold").addEventListener("input", () => {
  el("threshVal").textContent = Number(el("threshold").value).toFixed(2);
});
el("genToken").addEventListener("click", () => (el("token").value = randomToken()));
el("save").addEventListener("click", save);

// Poll status until the model is ready, then settle into a slow heartbeat.
async function pollStatus() {
  try {
    const st = await invoke("get_status");
    setStatus(st.model_ready, st.port);
    if (!st.model_ready) return setTimeout(pollStatus, 800);
  } catch {}
  setTimeout(pollStatus, 5000);
}

(async function init() {
  const view = await invoke("get_config");
  applyView(view);
  pollStatus();
})();
