// Content script: intercepts paste into editable fields, asks the background
// (→offscreen model) to find PII, and offers to redact before inserting.
//
// Flow on paste: capture clipboard text, preventDefault, classify, and if PII is
// found show a small inline prompt letting the user insert Redacted or Original.

const MASK = (label) => `[${label.toUpperCase()}]`;
let enabled = true;
chrome.storage.local.get("enabled").then((s) => { enabled = s.enabled !== false; });
chrome.storage.onChanged.addListener((c) => { if (c.enabled) enabled = c.enabled.newValue !== false; });

function isEditable(el) {
  if (!el) return false;
  const tag = el.tagName;
  if (tag === "TEXTAREA") return true;
  if (tag === "INPUT") return /^(text|search|email|url|tel|password|)$/i.test(el.type || "text") && el.type !== "password";
  return el.isContentEditable;
}

function redact(text, entities) {
  // Replace spans back-to-front so offsets stay valid.
  const sorted = [...entities].sort((a, b) => b.start - a.start);
  let out = text;
  for (const e of sorted) out = out.slice(0, e.start) + MASK(e.label) + out.slice(e.end);
  return out;
}

function insertText(el, text) {
  if (el.isContentEditable) {
    document.execCommand("insertText", false, text);
  } else {
    const start = el.selectionStart ?? el.value.length;
    const end = el.selectionEnd ?? el.value.length;
    el.value = el.value.slice(0, start) + text + el.value.slice(end);
    const pos = start + text.length;
    el.setSelectionRange(pos, pos);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
}

function showPrompt(el, original, redacted, entities) {
  document.querySelectorAll(".pf-prompt").forEach((n) => n.remove());
  const box = document.createElement("div");
  box.className = "pf-prompt";
  const labels = [...new Set(entities.map((e) => e.label))].join(", ");
  box.innerHTML = `<span class="pf-msg">⚠ ${entities.length} PII item(s): ${labels}</span>`;
  const mk = (txt, cls, val) => {
    const b = document.createElement("button");
    b.textContent = txt; b.className = cls;
    b.addEventListener("click", () => { insertText(el, val); box.remove(); });
    return b;
  };
  box.appendChild(mk("Insert redacted", "pf-redact", redacted));
  box.appendChild(mk("Insert original", "pf-orig", original));
  const r = el.getBoundingClientRect();
  box.style.top = `${window.scrollY + r.bottom + 4}px`;
  box.style.left = `${window.scrollX + r.left}px`;
  document.body.appendChild(box);
  setTimeout(() => box.remove(), 15000);
}

document.addEventListener(
  "paste",
  (ev) => {
    if (!enabled) return;
    const el = ev.target;
    if (!isEditable(el)) return;
    const text = ev.clipboardData?.getData("text/plain");
    if (!text || text.length > 5000) return; // skip empty / very large pastes
    ev.preventDefault();
    ev.stopPropagation();
    chrome.runtime.sendMessage({ type: "PF_CLASSIFY", text }, (res) => {
      if (chrome.runtime.lastError || !res?.ok) { insertText(el, text); return; }
      if (!res.entities.length) { insertText(el, text); return; }
      showPrompt(el, text, redact(text, res.entities), res.entities);
    });
  },
  true
);
