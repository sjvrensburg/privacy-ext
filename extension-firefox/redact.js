// Standalone extension page: drag/drop a text-like file (Markdown, Quarto, R
// Markdown, CSV, plain text), redact it on-device via the same local daemon the
// paste hook uses, and download the redacted copy. No text leaves the machine.
//
// The daemon caps a single /classify body at MAX_TEXT_BYTES (40 KB) and detection
// cost grows with length, so we don't send the whole file at once. These formats
// are all line-oriented and PII effectively never straddles a newline, so we
// split the file into batches of whole lines (each under BATCH_LIMIT), redact
// each through the background worker (which handles pairing + token + re-pair),
// and concatenate the redacted batches. Splitting on line boundaries means no
// overlap/stitching is needed: the joined batches reproduce the file exactly,
// minus the redacted spans. The daemon's own windowing still covers any single
// line longer than a batch.

const $ = (id) => document.getElementById(id);

// The daemon rejects a single /classify body over 40 KB (MAX_TEXT_BYTES). Batch
// just under that so a file that fits goes in ONE request: then the daemon runs
// the model over it (windowed internally, with overlap) AND applies user regex
// rules over the whole batch — including multi-line rules — exactly as the paste
// path would. Only files bigger than this get split, always on line boundaries.
const BATCH_LIMIT = 38_000;
// The daemon's hard per-request ceiling. A single line bigger than this can't be
// redacted without cutting it (which could split an entity), so we refuse it.
const SERVER_LIMIT = 40_000;
// Above this the browser round-trips get slow (daemon is serialized); warn but allow.
const SOFT_WARN_BYTES = 200 * 1024;
// Refuse absurdly large files outright rather than hang the tab.
const HARD_MAX_BYTES = 10 * 1024 * 1024;

const LABEL_NAMES = {
  name: "Name",
  "street address": "Street address",
  email: "Email",
  phone_num: "Phone",
  id_num: "ID number",
  url: "URL",
  username: "Username",
};
const labelName = (l) => LABEL_NAMES[l] || l;

const enc = new TextEncoder();
const byteLen = (s) => enc.encode(s).length;
const fmtBytes = (n) =>
  n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1024 / 1024).toFixed(1)} MB`;

let selectedFile = null;
let redactedText = null; // result held for download

// --- File splitting ---------------------------------------------------------

// Split text into "line units" that each include their trailing newline (and a
// preceding \r, if any), so concatenating them reproduces the original exactly.
function lineUnits(text) {
  return text.match(/[^\n]*\n?/g)?.filter((u) => u !== "") ?? [];
}

// Group whole lines into batches under BATCH_LIMIT bytes. We never cut a line in
// the middle: a single line that is itself over the limit becomes its own batch,
// sent whole so the daemon's internal windowing (with overlap) handles it rather
// than a client-side cut splitting an entity across two independent requests.
// Concatenating every batch in order reproduces the input text exactly.
function makeBatches(text) {
  const batches = [];
  let cur = "";
  let curBytes = 0;
  const push = () => {
    if (cur) batches.push(cur);
    cur = "";
    curBytes = 0;
  };
  for (const unit of lineUnits(text)) {
    const b = byteLen(unit);
    if (b > BATCH_LIMIT) {
      push();
      batches.push(unit); // an over-long single line, sent whole
      continue;
    }
    if (curBytes + b > BATCH_LIMIT) push();
    cur += unit;
    curBytes += b;
  }
  push();
  return batches;
}

// --- Daemon calls -----------------------------------------------------------

function classifyBatch(text) {
  return new Promise((resolve, reject) => {
    chrome.runtime.sendMessage({ type: "PF_CLASSIFY", text }, (res) => {
      if (chrome.runtime.lastError) return reject(new Error(chrome.runtime.lastError.message));
      if (!res?.ok) return reject(new Error(res?.error || "daemon error"));
      resolve(res); // { entities, redacted, parts }
    });
  });
}

function refreshChip() {
  chrome.runtime.sendMessage({ type: "PF_HEALTH" }, (res) => {
    const reachable = !chrome.runtime.lastError && res?.ok;
    $("chipDot").className = "dot " + (reachable ? "connected" : "disconnected");
    $("daemonWarn").classList.toggle("hidden", reachable);
  });
}

// --- Redaction flow ---------------------------------------------------------

async function redactFile() {
  const file = selectedFile;
  if (!file) return;
  hide("errorMsg");
  $("redactBtn").disabled = true;
  show("progCard");
  hide("resultCard");

  let text;
  try {
    text = await file.text();
  } catch (e) {
    return fail(`Couldn't read the file: ${e.message}`);
  }

  const batches = makeBatches(text);
  const total = batches.length || 1;
  const parts = [];
  const byLabel = {};
  let done = 0;

  setProgress(0, total);
  for (const batch of batches) {
    // Skip the network only for batches with no visible characters at all
    // (blank lines / whitespace). Anything with content — including non-Latin
    // scripts a `[A-Za-z0-9]` test would wrongly skip — must be sent so its PII
    // is actually redacted.
    if (!/\S/.test(batch)) {
      parts.push(batch);
      setProgress(++done, total);
      continue;
    }
    // A single line over the daemon's hard limit can't be redacted without
    // cutting it (risking a split entity), so refuse rather than leak.
    if (byteLen(batch) > SERVER_LIMIT) {
      return fail(
        `One line is larger than ${fmtBytes(SERVER_LIMIT)}, which can't be redacted safely. Split that line and try again.`
      );
    }
    let res;
    try {
      res = await classifyBatch(batch);
    } catch (e) {
      return fail(`Redaction failed: ${e.message}. Is the ClipCloak app running?`);
    }
    parts.push(res.redacted);
    for (const ent of res.entities) byLabel[ent.label] = (byLabel[ent.label] || 0) + 1;
    setProgress(++done, total);
  }

  redactedText = parts.join("");
  hide("progCard");
  showResult(byLabel);
}

function setProgress(done, total) {
  const pct = Math.round((done / total) * 100);
  $("barFill").style.width = pct + "%";
  $("progPct").textContent = pct + "%";
  $("progText").textContent = done < total ? `Redacting… batch ${done + 1} of ${total}` : "Finishing…";
}

function showResult(byLabel) {
  const totals = Object.values(byLabel).reduce((a, b) => a + b, 0);
  $("summary").textContent = totals ? `Redacted ${totals} item${totals === 1 ? "" : "s"}` : "No PII detected";

  const counts = $("counts");
  counts.replaceChildren();
  const entries = Object.entries(byLabel).sort((a, b) => b[1] - a[1]);
  if (entries.length) {
    counts.classList.remove("none");
    for (const [label, n] of entries) {
      const chip = document.createElement("span");
      chip.className = "count-chip";
      const num = document.createElement("span");
      num.className = "n";
      num.textContent = n;
      chip.append(num, document.createTextNode(" " + labelName(label)));
      counts.appendChild(chip);
    }
  } else {
    counts.classList.add("none");
    const chip = document.createElement("span");
    chip.className = "count-chip";
    chip.textContent = "Nothing matched — the file is unchanged.";
    counts.appendChild(chip);
  }

  // Show a bounded slice of the redacted text; huge files would freeze layout.
  const PREVIEW_MAX = 20_000;
  const clipped = redactedText.length > PREVIEW_MAX;
  $("preview").textContent = clipped ? redactedText.slice(0, PREVIEW_MAX) : redactedText;
  $("previewNote").textContent = clipped ? "showing the first 20,000 characters" : "";

  show("resultCard");
}

function download() {
  if (redactedText == null || !selectedFile) return;
  const blob = new Blob([redactedText], { type: selectedFile.type || "text/plain" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = redactedName(selectedFile.name);
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// "report.Rmd" -> "report-redacted.Rmd"; "notes" -> "notes-redacted".
function redactedName(name) {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? `${name.slice(0, dot)}-redacted${name.slice(dot)}` : `${name}-redacted`;
}

// --- File selection / UI ----------------------------------------------------

function selectFile(file) {
  if (!file) return;
  if (file.size > HARD_MAX_BYTES) {
    return fail(`That file is ${fmtBytes(file.size)}. The limit is ${fmtBytes(HARD_MAX_BYTES)} for the in-browser redactor.`);
  }
  selectedFile = file;
  redactedText = null;
  hide("errorMsg");
  hide("resultCard");
  hide("progCard");
  $("fileName").textContent = file.name;
  $("fileMeta").textContent = fmtBytes(file.size) + (file.type ? ` · ${file.type}` : "");
  $("redactBtn").disabled = false;
  const big = file.size > SOFT_WARN_BYTES;
  $("sizeWarn").textContent = big ? "Large file — redaction may take a while, and paste redaction pauses while it runs." : "";
  $("sizeWarn").classList.toggle("hidden", !big);
  show("fileCard");
}

function reset() {
  selectedFile = null;
  redactedText = null;
  $("file").value = "";
  hide("fileCard");
  hide("progCard");
  hide("resultCard");
  hide("errorMsg");
}

function fail(msg) {
  const b = $("errorMsg");
  b.textContent = msg;
  show("errorMsg");
  hide("progCard");
  $("redactBtn").disabled = false;
}

const show = (id) => $(id).classList.remove("hidden");
const hide = (id) => $(id).classList.add("hidden");

// Drag & drop + click-to-choose.
const drop = $("drop");
drop.addEventListener("click", () => $("file").click());
drop.addEventListener("keydown", (e) => {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    $("file").click();
  }
});
$("file").addEventListener("change", (e) => selectFile(e.target.files[0]));
["dragenter", "dragover"].forEach((ev) =>
  drop.addEventListener(ev, (e) => {
    e.preventDefault();
    drop.classList.add("dragover");
  })
);
["dragleave", "drop"].forEach((ev) =>
  drop.addEventListener(ev, (e) => {
    e.preventDefault();
    drop.classList.remove("dragover");
  })
);
drop.addEventListener("drop", (e) => selectFile(e.dataTransfer.files[0]));
// Keep a stray drop elsewhere on the page from navigating away from the tab.
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => e.preventDefault());

$("redactBtn").addEventListener("click", redactFile);
$("downloadBtn").addEventListener("click", download);
$("anotherBtn").addEventListener("click", reset);

refreshChip();
setInterval(refreshChip, 4000);
