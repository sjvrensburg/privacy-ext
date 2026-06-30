// GLiNER2 PII pipeline for the browser (onnxruntime-web + transformers.js).
// Ported from the validated Node pipeline. Loaded inside the offscreen document.
//
// IMPORTANT: transformers.js tokenization is version-dependent. This extension
// vendors 3.8.1 (the version whose feed is bit-exact with the model export).
// Do not bump it without re-validating the feed, or detection silently breaks.

import { AutoTokenizer, env as hfEnv } from "./vendor/transformers.min.js";
import * as ort from "./vendor/ort/ort.wasm.bundle.min.mjs";

const SCHEMA_POS = [1, 6, 8, 10, 12, 16, 21, 24]; // [P] + 7×[E] subword positions (baked schema)
const MAX_WIDTH = 8;
const LABELS = ["name", "address", "email", "phone_num", "id_num", "url", "username"];
const GRAPHS = ["encoder", "schema_gather", "count_pred_argmax", "count_lstm_fixed",
                "token_gather", "span_rep", "scorer"];
const MODEL_CACHE = "pii-models-v1";
const WORD_RE = /https?:\/\/[^\s]+|www\.[^\s]+|[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}|@[a-z0-9_]+|\w+(?:[-_]\w+)*|\S/gi;

const sigmoid = (x) => 1 / (1 + Math.exp(-x));
const i64 = (a, d) => new ort.Tensor("int64", BigInt64Array.from(a.map(BigInt)), d);

let tokenizer = null;
let sessions = null;

// Configure runtimes to load entirely from the extension bundle (no network for code).
ort.env.wasm.numThreads = 1;          // single-thread => no SharedArrayBuffer / COOP-COEP
ort.env.wasm.wasmPaths = new URL("./vendor/ort/", import.meta.url).href;
hfEnv.allowRemoteModels = false;      // tokenizer is bundled
hfEnv.allowLocalModels = true;
hfEnv.localModelPath = new URL("./models/", import.meta.url).href;

async function fetchCached(url, onProgress) {
  const cache = await caches.open(MODEL_CACHE);
  let resp = await cache.match(url);
  if (!resp) {
    resp = await fetch(url);
    if (!resp.ok) throw new Error(`fetch ${url} -> ${resp.status}`);
    await cache.put(url, resp.clone());
  }
  const buf = await resp.arrayBuffer();
  onProgress?.(url);
  return buf;
}

// modelBaseUrl: directory holding <graph>_fp16.onnx (configurable; cached after first load)
export async function init(modelBaseUrl, onProgress) {
  if (sessions) return;
  tokenizer = await AutoTokenizer.from_pretrained("gliner2-tokenizer");
  const base = modelBaseUrl.endsWith("/") ? modelBaseUrl : modelBaseUrl + "/";
  sessions = {};
  for (const g of GRAPHS) {
    const buf = await fetchCached(base + `${g}_fp16.onnx`, onProgress);
    sessions[g] = await ort.InferenceSession.create(buf, { executionProviders: ["wasm"] });
  }
}

function buildFeed(text) {
  const lower = text.toLowerCase(), words = [];
  for (const m of lower.matchAll(WORD_RE)) words.push({ start: m.index, end: m.index + m[0].length, tok: m[0] });
  const schemaTokens = ["(", "[P]", "entities", "(", ...LABELS.flatMap((l) => ["[E]", l]), ")", ")"];
  const combined = [...schemaTokens, "[SEP_TEXT]", ...words.map((w) => w.tok)];
  const idsOf = (s) => Array.from(tokenizer.encode(s, { add_special_tokens: false }));
  const inputIds = [], wordFirst = [];
  combined.forEach((t, idx) => {
    const pos = inputIds.length, ids = idsOf(t);
    if (idx > schemaTokens.length && ids.length) wordFirst.push(pos);
    inputIds.push(...ids);
  });
  return { inputIds, wordFirst, words };
}

export async function classify(text, threshold = 0.55) {
  if (!sessions) throw new Error("pipeline not initialized");
  const { inputIds, wordFirst, words } = buildFeed(text);
  const W = wordFirst.length, seq = inputIds.length;
  if (W === 0) return [];

  const S = sessions;
  const { last_hidden_state } = await S.encoder.run({
    input_ids: i64(inputIds, [1, seq]),
    attention_mask: i64(inputIds.map(() => 1), [1, seq]),
  });
  const sg = await S.schema_gather.run({ last_hidden_state, schema_indices: i64(SCHEMA_POS, [SCHEMA_POS.length]) });
  const { pred_count } = await S.count_pred_argmax.run({ pc_emb: sg.pc_emb });
  if (Number(pred_count.data[0]) <= 0) return [];

  const { struct_proj } = await S.count_lstm_fixed.run({ field_embs: sg.field_embs });
  const { text_embs } = await S.token_gather.run({ last_hidden_state, word_indices: i64(wordFirst, [W]) });
  const spanFlat = [];
  for (let s = 0; s < W; s++) for (let w = 0; w < MAX_WIDTH; w++) {
    const e = s + w; if (e >= W) spanFlat.push(0, 0); else spanFlat.push(s, e);
  }
  const { span_embeddings } = await S.span_rep.run({ hidden_states: text_embs, span_idx: i64(spanFlat, [1, W * MAX_WIDTH, 2]) });
  const { entity_scores } = await S.scorer.run({ span_embeddings, struct_proj });

  const [, EW, EK, F] = entity_scores.dims, d = entity_scores.data;
  const at = (w, k, f) => Number(d[((0 * EW + w) * EK + k) * F + f]); // count slot 0
  const ents = [];
  for (let f = 0; f < F; f++) {
    const cand = [];
    for (let w = 0; w < W; w++) for (let k = 0; k < MAX_WIDTH; k++) {
      if (w + k >= W) continue;
      const sc = sigmoid(at(w, k, f));
      if (sc >= threshold) cand.push({ score: sc, start: words[w].start, end: words[w + k].end });
    }
    cand.sort((a, b) => b.score - a.score);
    const sel = [];
    for (const c of cand) if (!sel.some((s) => !(c.end <= s.start || c.start >= s.end))) sel.push(c);
    for (const c of sel) ents.push({ label: LABELS[f], score: +c.score.toFixed(3), start: c.start, end: c.end });
  }
  ents.sort((a, b) => a.start - b.start);
  return ents;
}
