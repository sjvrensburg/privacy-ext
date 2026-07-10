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

// ---- Custom regex rules ----

function makeRuleRow(rule) {
  const row = document.createElement("div");
  row.className = "rule";
  row.innerHTML = `
    <input type="checkbox" class="rule-enabled" title="Enable rule" ${rule.enabled ? "checked" : ""} />
    <input type="text" class="rule-name" placeholder="Name" value="${escapeAttr(rule.name)}" />
    <input type="text" class="rule-pattern mono" placeholder="Regex pattern" value="${escapeAttr(rule.pattern)}" />
    <button type="button" class="rule-del ghost" title="Delete rule">✕</button>`;
  row.querySelector(".rule-del").addEventListener("click", () => {
    row.remove();
    scheduleTest();
  });
  for (const inp of row.querySelectorAll("input")) {
    inp.addEventListener("input", scheduleTest);
    inp.addEventListener("change", scheduleTest);
  }
  return row;
}

function escapeAttr(s) {
  return String(s ?? "").replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");
}

function renderRules(rules) {
  const box = el("rules");
  box.innerHTML = "";
  for (const r of rules || []) box.appendChild(makeRuleRow(r));
}

function collectRules() {
  return [...el("rules").querySelectorAll(".rule")].map((row) => ({
    name: row.querySelector(".rule-name").value,
    pattern: row.querySelector(".rule-pattern").value,
    enabled: row.querySelector(".rule-enabled").checked,
  }));
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
  renderRules(c.rules);
  el("portHint").classList.toggle("hidden", !view.restart_needed);
  setStatus(view.model_ready, c.port);
  scheduleTest();
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
    rules: collectRules(),
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
el("addRule").addEventListener("click", () => {
  el("rules").appendChild(makeRuleRow({ name: "", pattern: "", enabled: true }));
});
el("testInput").addEventListener("input", scheduleTest);

// ---- Live rule testing (regex only, no model) ----

let testTimer = null;
function scheduleTest() {
  clearTimeout(testTimer);
  testTimer = setTimeout(runTest, 200);
}

async function runTest() {
  const sample = el("testInput").value;
  const rules = collectRules();
  const errBox = el("testErrors");
  const out = el("testOut");

  // Nothing to preview until there's sample text and at least one usable rule.
  const hasRule = rules.some((r) => r.enabled && r.name.trim() && r.pattern);
  if (!sample || !hasRule) {
    errBox.innerHTML = "";
    out.classList.add("hidden");
    return;
  }

  let res;
  try {
    res = await invoke("test_rules", { rules, sample });
  } catch (e) {
    errBox.innerHTML = `<div class="rule-err">Error: ${escapeAttr(e)}</div>`;
    out.classList.add("hidden");
    return;
  }

  errBox.innerHTML = (res.errors || [])
    .map((e) => `<div class="rule-err">${escapeAttr(e.name)}: ${escapeAttr(e.error)}</div>`)
    .join("");

  out.classList.remove("hidden");
  el("testRedacted").textContent = res.redacted;
  const n = res.matches.length;
  const byRule = {};
  for (const m of res.matches) byRule[m.rule] = (byRule[m.rule] || 0) + 1;
  const breakdown = Object.entries(byRule).map(([r, c]) => `${r} ×${c}`).join(", ");
  el("testSummary").textContent = n
    ? `${n} match${n === 1 ? "" : "es"}: ${breakdown}`
    : "No matches in this sample.";
}

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
