// Thin client content script: intercept paste into editable fields, ask the
// background (→ local daemon) for PII, and offer to insert redacted vs original.

let enabled = true;
chrome.storage.local.get("enabled").then((s) => { enabled = s.enabled !== false; });
chrome.storage.onChanged.addListener((c) => { if (c.enabled) enabled = c.enabled.newValue !== false; });

function isEditable(el) {
  if (!el) return false;
  if (el.tagName === "TEXTAREA") return true;
  if (el.tagName === "INPUT") {
    const t = (el.type || "text").toLowerCase();
    return t !== "password" && /^(text|search|email|url|tel|)$/.test(t);
  }
  return el.isContentEditable;
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
  const msg = document.createElement("span");
  msg.className = "pf-msg";
  msg.textContent = `⚠ ${entities.length} PII item(s): ${labels}`;
  box.appendChild(msg);
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
    if (!text || text.length > 5000) return;
    ev.preventDefault();
    ev.stopPropagation();
    chrome.runtime.sendMessage({ type: "PF_CLASSIFY", text }, (res) => {
      // On any failure (daemon down, etc.) fall back to inserting the original.
      if (chrome.runtime.lastError || !res?.ok || !res.entities?.length) {
        insertText(el, text);
        return;
      }
      showPrompt(el, text, res.redacted, res.entities);
    });
  },
  true
);
