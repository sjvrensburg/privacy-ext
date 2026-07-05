// Thin client content script: intercept paste into editable fields, ask the
// background (→ local daemon) for PII, auto-redact in place, and offer an
// undo toast rather than blocking the paste on a confirm dialog.

const hostname = window.location.hostname;
const isTopFrame = window.top === window;
let state = { enabled: true, originSettings: {}, defaultOriginActive: false };
// Sentinel (not a boolean) so the first resolution always reports its state —
// otherwise a page that resolves to inactive (the common case) would match
// this initial value and updateActive() would never send PF_TAB_STATE at all,
// leaving the toolbar badge unset instead of explicitly showing OFF.
let active = null;

function resolveActive(s) {
  return s.enabled !== false && (s.originSettings?.[hostname]?.active ?? (s.defaultOriginActive ?? false));
}

function updateActive(next) {
  if (next === active) return;
  active = next;
  // The toolbar badge represents the tab's top-level origin (same as what
  // the popup shows via tab.url) — a subframe (ad, widget, etc.) resolving
  // its own origin's state must not be able to override that.
  if (isTopFrame) chrome.runtime.sendMessage({ type: "PF_TAB_STATE", active });
}

chrome.storage.local.get(["enabled", "originSettings", "defaultOriginActive"]).then((s) => {
  state = { ...state, ...s };
  updateActive(resolveActive(state));
});

chrome.storage.onChanged.addListener((changes) => {
  let touched = false;
  if (changes.enabled) { state.enabled = changes.enabled.newValue; touched = true; }
  if (changes.originSettings) { state.originSettings = changes.originSettings.newValue; touched = true; }
  if (changes.defaultOriginActive) { state.defaultOriginActive = changes.defaultOriginActive.newValue; touched = true; }
  if (touched) updateActive(resolveActive(state));
});

function isEditable(el) {
  if (!el) return false;
  if (el.tagName === "TEXTAREA") return true;
  if (el.tagName === "INPUT") {
    const t = (el.type || "text").toLowerCase();
    return t !== "password" && /^(text|search|email|url|tel|)$/.test(t);
  }
  return el.isContentEditable;
}

// Plain insert with no undo tracking — used for the "nothing to redact"
// fallback path where the original text is inserted verbatim.
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

// Insert via a raw Range rather than execCommand("insertText") so we keep a
// direct reference to the inserted text node for undo — execCommand can
// silently collapse adjacent whitespace, which would invalidate any
// character-offset bookkeeping computed beforehand.
function insertTextContentEditable(el, redacted, original) {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  range.deleteContents();
  const textNode = document.createTextNode(redacted);
  range.insertNode(textNode);

  const after = document.createRange();
  after.setStartAfter(textNode);
  after.collapse(true);
  sel.removeAllRanges();
  sel.addRange(after);

  // Nudge framework-controlled editors (React et al.) to notice the DOM edit.
  el.dispatchEvent(new InputEvent("beforeinput", { bubbles: true, inputType: "insertText", data: redacted }));
  el.dispatchEvent(new Event("input", { bubbles: true }));
  return { kind: "contenteditable", el, textNode, originalText: original };
}

function undoContentEditable(snap) {
  // The framework may have re-rendered and dropped our node — degrade to a
  // no-op rather than corrupting unrelated content.
  if (!snap.textNode.isConnected) return;
  const replacement = document.createTextNode(snap.originalText);
  snap.textNode.replaceWith(replacement);

  const sel = window.getSelection();
  const after = document.createRange();
  after.setStartAfter(replacement);
  after.collapse(true);
  sel.removeAllRanges();
  sel.addRange(after);

  snap.el.dispatchEvent(new InputEvent("beforeinput", { bubbles: true, inputType: "insertText", data: snap.originalText }));
  snap.el.dispatchEvent(new Event("input", { bubbles: true }));
}

function insertRedactedWithUndo(el, redacted, original) {
  if (el.isContentEditable) return insertTextContentEditable(el, redacted, original);
  const start = el.selectionStart ?? el.value.length;
  const end = el.selectionEnd ?? el.value.length;
  el.value = el.value.slice(0, start) + redacted + el.value.slice(end);
  const pos = start + redacted.length;
  el.setSelectionRange(pos, pos);
  el.dispatchEvent(new Event("input", { bubbles: true }));
  return { kind: "value", el, start, redactedLength: redacted.length, originalText: original };
}

function undoRedaction(snap) {
  if (!snap) return;
  if (snap.kind === "contenteditable") {
    undoContentEditable(snap);
    return;
  }
  const { el, start, redactedLength, originalText } = snap;
  el.value = el.value.slice(0, start) + originalText + el.value.slice(start + redactedLength);
  const pos = start + originalText.length;
  el.setSelectionRange(pos, pos);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function showToast(el, snapshot, entities) {
  document.querySelectorAll(".pf-toast").forEach((n) => n.remove());
  const box = document.createElement("div");
  box.className = "pf-toast";

  const labels = [...new Set(entities.map((e) => e.label))].join(", ");
  const msg = document.createElement("span");
  msg.className = "pf-msg";
  msg.textContent = `Redacted ${entities.length} item(s): ${labels}`;
  box.appendChild(msg);

  const undoBtn = document.createElement("button");
  undoBtn.textContent = "Undo";
  undoBtn.className = "pf-undo";
  undoBtn.addEventListener("click", () => { undoRedaction(snapshot); box.remove(); });
  box.appendChild(undoBtn);

  const dismissBtn = document.createElement("button");
  dismissBtn.textContent = "×";
  dismissBtn.className = "pf-dismiss";
  dismissBtn.title = "Dismiss";
  dismissBtn.addEventListener("click", () => box.remove());
  box.appendChild(dismissBtn);

  const r = el.getBoundingClientRect();
  box.style.top = `${window.scrollY + r.bottom + 4}px`;
  box.style.left = `${window.scrollX + r.left}px`;
  document.body.appendChild(box);
  setTimeout(() => box.remove(), 8000);
}

document.addEventListener(
  "paste",
  (ev) => {
    if (!active) return;
    const el = ev.target;
    if (!isEditable(el)) return;
    const text = ev.clipboardData?.getData("text/plain");
    if (!text || text.length > 5000) return;
    ev.preventDefault();
    ev.stopPropagation();
    chrome.runtime.sendMessage({ type: "PF_CLASSIFY", text }, (res) => {
      // On any failure (daemon down, etc.) or no PII found, insert as-is.
      if (chrome.runtime.lastError || !res?.ok || !res.entities?.length) {
        insertText(el, text);
        return;
      }
      const snapshot = insertRedactedWithUndo(el, res.redacted, text);
      showToast(el, snapshot, res.entities);
    });
  },
  true
);
