// Round agent avatars for the phone client: a tinted disc with a white glyph per agent family, plus an optional
// corner badge (running / unread). Pure DOM, no network.

const FAMILIES = [
  { re: /claude/i, key: 'claude' },
  { re: /deepseek/i, key: 'deepseek' },
  { re: /codex|openai|gpt/i, key: 'codex' },
  { re: /antigravity|agy|gemini/i, key: 'agy' },
  { re: /api|provider|openrouter|kimi|qwen|glm/i, key: 'api' },
];

// 24×24 glyphs drawn in currentColor.
const GLYPHS = {
  claude:
    '<g stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><path d="M12 3.5v17M3.5 12h17M6 6l12 12M18 6 6 18"/></g>',
  codex:
    '<g fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 8 4 4-4 4M12.5 16.5H18"/></g>',
  deepseek:
    '<path fill="currentColor" d="M20.6 7.3c-.3-.1-.5.1-.7.3l-.3.3a3.4 3.4 0 0 0-2.9-1.3c-1.4 0-2.4.4-3.4 1.3-.9-.6-1.9-1-3.3-1.1-1.2 0-2.6.5-3.6 1.2-.9.6-1.5 1.5-2 2.4-.5 1.1-.6 2.3-.4 3.4.3 1.5 1.1 2.7 2.4 3.6 1.3.9 2.8 1.3 4.4 1.2 1.8-.1 3.4-.8 4.7-2.1.4.2 1 .4 1.6.4h.5c.3 0 .5-.1.5-.3 0-.2-.2-.3-.4-.4-.6-.1-1-.4-1.3-.7a8 8 0 0 0 1.3-3c.9.2 1.8-.1 2.4-.7.7-.7 1-1.7.9-2.6v-1.3c.1-.2 0-.5-.4-.6Zm-9.4 9.6c-2.3 0-4.3-1.6-4.3-3.9 0-.4.4-.6.7-.4 1.5 1 3.3 1.7 5.2 1.7.4 0 .6.4.4.7-.5 1.1-1.1 1.9-2 1.9Zm6.1-6.4c-.4 0-.7-.3-.7-.7s.3-.7.7-.7.7.3.7.7-.3.7-.7.7Z"/>',
  agy:
    '<g fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V6M7 10.5 12 5.5l5 5"/><path d="M5 19.5c2.2-1.3 4.5-2 7-2s4.8.7 7 2" opacity=".7"/></g>',
  api:
    '<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 7 4 12l5 5M15 7l5 5-5 5"/></g>',
};

/** Agent family key (`claude`, `codex`, …) or `other`. */
export function agentFamily(id) {
  const key = String(id || '');
  for (const family of FAMILIES) if (family.re.test(key)) return family.key;
  return 'other';
}

/** Inline 24×24 SVG icon (colored by CSS via `.ag-glyph-<family>`) for composer & picker headers. */
export function agentGlyph(id, size = 18) {
  const family = agentFamily(id);
  const wrap = document.createElement('span');
  wrap.className = 'ag-glyph ag-glyph-' + family;
  wrap.setAttribute('aria-hidden', 'true');
  const glyph = GLYPHS[family];
  if (glyph) {
    wrap.innerHTML = '<svg viewBox="0 0 24 24" width="' + size + '" height="' + size + '">' + glyph + '</svg>';
  } else {
    wrap.innerHTML =
      '<svg viewBox="0 0 24 24" width="' + size + '" height="' + size + '"><circle cx="12" cy="12" r="5" fill="currentColor"/></svg>';
  }
  return wrap;
}

/**
 * `<span class="av av-<family>">` with the glyph (or the name's first letter) and an optional badge:
 * `running` (pulsing ring) or `unread` (dot).
 */
export function agentAvatar(id, name, opts = {}) {
  const family = agentFamily(id);
  const box = document.createElement('span');
  box.className = 'av av-' + family + (opts.small ? ' av-sm' : '');
  box.setAttribute('aria-hidden', 'true');
  const glyph = GLYPHS[family];
  if (glyph) {
    box.innerHTML = '<svg viewBox="0 0 24 24" width="22" height="22">' + glyph + '</svg>';
  } else {
    box.textContent = String(name || id || '?').trim().charAt(0).toUpperCase() || '?';
  }
  if (opts.badge) {
    const badge = document.createElement('i');
    badge.className = 'av-badge av-badge-' + opts.badge;
    box.appendChild(badge);
  }
  return box;
}
