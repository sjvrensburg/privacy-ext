// Curated hostnames that default to redaction-active on first install.
// Single source of truth — read by background.js (onInstalled seeding) and
// popup.js (the "Auto-active sites" list).
const DEFAULT_AI_SITES = [
  "claude.ai",
  "chatgpt.com",
  "chat.openai.com",
  "gemini.google.com",
  "poe.com",
  "copilot.microsoft.com",
  "perplexity.ai",
];

const AI_SITES_SEED_VERSION = 1;
