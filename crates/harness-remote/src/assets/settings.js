import { appearanceBody } from '/assets/appearance.js';
import { renderSettingRow, closeSettingSheet } from '/assets/settings_edit.js';

let root = null;
let api = null;
let seq = 0;

const GROUPS = [
  { id: 'connect', label: 'Connections' },
  { id: 'app', label: '' },
  { id: 'agent', label: 'Agents' },
  { id: 'data', label: 'Data' },
];

const PAGES = [
  { id: 'remote', title: 'Remote', icon: 'monitor', group: 'connect' },
  { id: 'projects', title: 'Projects', icon: 'folder', group: 'connect' },
  { id: 'notifications', title: 'Notifications', icon: 'bell', group: 'connect' },
  { id: 'general', title: 'General', icon: 'gear', group: 'app' },
  { id: 'appearance', title: 'Appearance', icon: 'palette', group: 'app' },
  { id: 'inbox', title: 'Inbox', icon: 'inbox', group: 'app' },
  { id: 'shortcuts', title: 'Shortcuts', icon: 'keyboard', group: 'app' },
  { id: 'agents', title: 'Agents', icon: 'bot', group: 'agent' },
  { id: 'providers', title: 'API Providers', icon: 'key', group: 'agent' },
  { id: 'orchestra', title: 'Orchestra', icon: 'network', group: 'agent' },
  { id: 'subagents', title: 'Subagents', icon: 'fork', group: 'agent' },
  { id: 'mcp', title: 'MCP', icon: 'plug', group: 'agent' },
  { id: 'instructions', title: 'Instructions', icon: 'file', group: 'agent' },
  { id: 'memory', title: 'Memory', icon: 'chip', group: 'agent' },
  { id: 'skills', title: 'Skills', icon: 'zap', group: 'agent' },
  { id: 'commands', title: 'Commands', icon: 'terminal', group: 'agent' },
  { id: 'hooks', title: 'Hooks', icon: 'link', group: 'agent' },
  { id: 'archive', title: 'Archive', icon: 'archive', group: 'data' },
  { id: 'import', title: 'Import', icon: 'download', group: 'data' },
];

const ICONS = {
  monitor: '<rect x="2" y="4" width="20" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  bell: '<path d="M6 9a6 6 0 0 1 12 0c0 6 2.5 7.5 2.5 7.5h-17S6 15 6 9z"/><path d="M10 20a2 2 0 0 0 4 0"/>',
  gear: '<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>',
  palette: '<path d="M12 2C6.5 2 2 6.5 2 12s4.5 10 10 10c.93 0 1.65-.75 1.65-1.69 0-.44-.18-.83-.44-1.12-.29-.29-.44-.65-.44-1.13a1.64 1.64 0 0 1 1.67-1.67h2c3.05 0 5.55-2.5 5.55-5.55C21.97 6.01 17.46 2 12 2z"/><circle cx="13.5" cy="6.5" r="0.6"/><circle cx="17.5" cy="10.5" r="0.6"/><circle cx="8.5" cy="7.5" r="0.6"/><circle cx="6.5" cy="12.5" r="0.6"/>',
  inbox: '<path d="M22 12h-6l-2 3h-4l-2-3H2"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>',
  keyboard: '<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
  bot: '<rect x="4" y="8" width="16" height="12" rx="2"/><path d="M12 8V4M9 13v2M15 13v2M2 14h2M20 14h2"/><circle cx="12" cy="3" r="1"/>',
  key: '<circle cx="7.5" cy="15.5" r="5.5"/><path d="m21 2-9.6 9.6M15.5 7.5l3 3L22 7l-3-3"/>',
  network: '<circle cx="12" cy="5" r="2.5"/><circle cx="5" cy="19" r="2.5"/><circle cx="19" cy="19" r="2.5"/><path d="M12 7.5V12M12 12l-5.4 4.9M12 12l5.4 4.9"/>',
  fork: '<circle cx="12" cy="18" r="3"/><circle cx="6" cy="6" r="3"/><circle cx="18" cy="6" r="3"/><path d="M18 9v2a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V9M12 12v3"/>',
  plug: '<path d="M12 22v-5M9 8V2M15 8V2M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8z"/>',
  file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6M8 13h8M8 17h8"/>',
  chip: '<rect x="5" y="5" width="14" height="14" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3"/>',
  zap: '<path d="M13 2 3 14h9l-1 8 10-12h-9z"/>',
  terminal: '<path d="m4 17 6-5-6-5M12 19h8"/>',
  link: '<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>',
  archive: '<rect x="2" y="3" width="20" height="5" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8M10 12h4"/>',
  download: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/>',
  chat: '<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/><path d="M8 9h8M8 13h5"/>',
  pull: '<circle cx="6" cy="6" r="3"/><circle cx="18" cy="18" r="3"/><path d="M6 9v12M13 6h3a2 2 0 0 1 2 2v7M15 4l-2 2 2 2"/>',
  usage: '<rect x="3" y="12" width="4" height="8" rx="1"/><rect x="10" y="5" width="4" height="15" rx="1"/><rect x="17" y="9" width="4" height="11" rx="1"/>',
  chevron: '<path d="m9 6 6 6-6 6"/>',
  back: '<path d="M19 12H5M12 19l-7-7 7-7"/>',
};

const SVG_OPEN =
  '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">';

function icon(name, className) {
  const box = document.createElement('span');
  box.className = className;
  box.setAttribute('aria-hidden', 'true');
  box.innerHTML = SVG_OPEN + (ICONS[name] || ICONS.gear) + '</svg>';
  return box;
}

// 4-tab bottom navigation bar (Chats · Pulls · Usage · Settings) with optional compose FAB.
export function bottomNav(viewApi, active, action) {
  const dock = document.createElement('div');
  dock.className = 'app-dock';
  const nav = document.createElement('nav');
  nav.className = 'app-nav';
  nav.setAttribute('aria-label', 'Main');
  const items = [
    { id: 'chats', label: 'Chats', icon: 'chat', go: () => viewApi.closeSettings() },
    { id: 'pulls', label: 'Pulls', icon: 'pull', go: () => viewApi.openPulls() },
    { id: 'usage', label: 'Usage', icon: 'usage', go: () => viewApi.openUsage() },
    { id: 'settings', label: 'Settings', icon: 'gear', go: () => viewApi.openSettings('') },
  ];
  for (const item of items) {
    const on = item.id === active;
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = on ? 'app-nav-item on' : 'app-nav-item';
    btn.dataset.nav = item.id;
    if (on) btn.setAttribute('aria-current', 'page');
    btn.appendChild(icon(item.icon, 'app-nav-icon'));
    const label = document.createElement('span');
    label.className = 'app-nav-label';
    label.textContent = item.label;
    btn.appendChild(label);
    if (!on) btn.addEventListener('click', item.go);
    nav.appendChild(btn);
  }
  dock.appendChild(nav);
  if (action) {
    const fab = document.createElement('button');
    fab.type = 'button';
    fab.className = 'app-fab';
    fab.setAttribute('aria-label', action.title);
    fab.innerHTML =
      '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 20h9"/><path d="M16.4 3.6a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>';
    const label = document.createElement('span');
    label.textContent = action.title;
    fab.appendChild(label);
    fab.addEventListener('click', action.onClick);
    dock.appendChild(fab);
  }
  return dock;
}

const TONES = new Set(['default', 'accent', 'success', 'warning', 'danger', 'muted', 'info']);
const KINDS = new Set(['row', 'toggle', 'code', 'stat']);
let hashBound = false;

export function initSettings(node, viewApi) {
  root = node;
  api = viewApi;
  if (!hashBound) {
    hashBound = true;
    window.addEventListener('hashchange', closeSettingSheet);
  }
}

export function showSettings(page) {
  if (!root) return;
  closeSettingSheet();
  seq += 1;
  const token = seq;
  const id = typeof page === 'string' ? page.trim() : '';
  if (id) openPage(id, token);
  else openIndex(token);
}

function text(value) {
  return typeof value === 'string' ? value : value == null ? '' : String(value);
}

function toneOf(value) {
  return TONES.has(value) ? value : 'default';
}

function labelOf(pageId) {
  const hit = PAGES.find((p) => p.id === pageId);
  if (hit) return hit.title;
  const words = pageId.replace(/[-_]+/g, ' ').trim();
  return words ? words.charAt(0).toUpperCase() + words.slice(1) : 'Settings';
}

function emptyNote() {
  const box = api.el('div', 'set-empty-box');
  box.appendChild(icon('monitor', 'set-empty-ico'));
  box.appendChild(api.el('p', 'set-empty-title', "This page isn't available on the phone yet"));
  box.appendChild(api.el('p', 'set-empty-sub', 'Manage this setting on the Harness desktop app.'));
  return box;
}

function openIndex(token) {
  root.textContent = '';
  const page = api.el('div', 'set-page');

  const head = api.el('header', 'set-head');
  head.appendChild(api.el('h1', 'set-title', 'Settings'));
  const searchBtn = api.el('button', 'set-action');
  searchBtn.type = 'button';
  searchBtn.setAttribute('aria-label', 'Search settings');
  searchBtn.appendChild(icon('search', 'set-action-icon'));
  head.appendChild(searchBtn);
  page.appendChild(head);

  const body = api.el('div', 'set-body');

  const searchBox = api.el('div', 'set-searchbox');
  searchBox.hidden = true;
  searchBox.appendChild(icon('search', 'set-search-glyph'));
  const search = api.el('input', 'set-search');
  search.type = 'search';
  search.placeholder = 'Search settings';
  search.setAttribute('aria-label', 'Search settings');
  search.spellcheck = false;
  searchBox.appendChild(search);
  body.appendChild(searchBox);

  const subs = new Map();
  const sides = new Map();
  const sections = [];
  for (const group of GROUPS) {
    const wrap = api.el('section', 'set-section');
    const caps = api.el('p', 'set-caps', group.label);
    const card = api.el('div', 'set-card');
    const rows = [];
    for (const entry of PAGES) {
      if (entry.group !== group.id) continue;
      const row = api.el('button', 'set-row');
      row.type = 'button';
      row.dataset.group = group.id;
      row.appendChild(icon(entry.icon, 'set-glyph'));
      const lines = api.el('span', 'set-lines');
      lines.appendChild(api.el('span', 'set-label', entry.title));
      const sub = api.el('span', 'set-value empty');
      subs.set(entry.id, sub);
      lines.appendChild(sub);
      row.appendChild(lines);
      const side = api.el('span', 'set-side');
      sides.set(entry.id, side);
      row.appendChild(side);
      row.appendChild(icon('chevron', 'set-chev'));
      row.addEventListener('click', () => api.openSettings(entry.id));
      rows.push(row);
      card.appendChild(row);
    }
    if (group.label) wrap.appendChild(caps);
    wrap.appendChild(card);
    body.appendChild(wrap);
    sections.push({ caps, card, rows, wrap });
  }

  const none = api.el('p', 'set-none', '');
  none.hidden = true;
  body.appendChild(none);

  page.appendChild(body);
  page.appendChild(bottomNav(api, 'settings'));
  root.appendChild(page);

  search.addEventListener('input', () => filterIndex(sections, search.value, none));
  searchBtn.addEventListener('click', () => {
    const open = searchBox.hidden;
    searchBox.hidden = !open;
    searchBtn.classList.toggle('on', open);
    if (open) {
      search.focus();
    } else {
      search.value = '';
      filterIndex(sections, '', none);
    }
  });

  if (!overviewPending) {
    overviewPending = loadOverview();
  }
  overviewPending.then((overview) => {
    if (token !== seq) return;
    const host = overview && typeof overview === 'object' ? text(overview.host).trim() : '';
    const items = overview && overview.items && typeof overview.items === 'object' ? overview.items : {};
    for (const [pageId, side] of sides) {
      const shown = text(items[pageId]).trim();
      const sub = subs.get(pageId);
      let below = shown.length > 14 ? shown : '';
      if (pageId === 'remote' && host) below = 'Harness on ' + host;
      side.textContent = below === shown ? '' : shown;
      sub.textContent = below;
      sub.classList.toggle('empty', !below);
    }
  });
}

// 概览只取一次：主题切换/重渲染时再刷会重复打 RPC。失败后允许下次重试。
let overviewPending = null;

function loadOverview() {
  const pending = (async () => {
    try {
      const data = await api.rpcSoft('settings_overview', {}, null);
      return data && typeof data === 'object' ? data : null;
    } catch (err) {
      return null;
    }
  })().then((data) => {
    if (overviewPending === pending && !data) overviewPending = null;
    return data;
  });
  return pending;
}

function filterIndex(sections, query, none) {
  const needle = text(query).trim().toLowerCase();
  let total = 0;
  for (const section of sections) {
    let shown = 0;
    for (const row of section.rows) {
      const hit = !needle || row.textContent.toLowerCase().includes(needle);
      row.hidden = !hit;
      if (hit) shown += 1;
    }
    section.caps.hidden = shown === 0;
    section.card.hidden = shown === 0;
    section.wrap.hidden = shown === 0;
    total += shown;
  }
  none.hidden = total > 0 || !needle;
  if (total === 0 && needle) none.textContent = 'No settings match “' + text(query).trim() + '”';
}

async function openPage(pageId, token) {
  root.textContent = '';
  const page = api.el('div', 'set-page');

  const head = api.el('header', 'set-head set-head-page');
  const back = api.el('button', 'set-back');
  back.type = 'button';
  back.setAttribute('aria-label', 'Back to Settings');
  back.appendChild(icon('back', 'set-back-icon'));
  back.addEventListener('click', () => api.openSettings(''));
  head.appendChild(back);
  const title = api.el('h1', 'set-title', labelOf(pageId));
  head.appendChild(title);
  page.appendChild(head);

  const body = api.el('div', 'set-body');
  if (pageId === 'appearance') {
    body.appendChild(appearanceBody(api));
    page.appendChild(body);
    root.appendChild(page);
    return;
  }
  const note = api.el('p', 'set-note', 'Loading…');
  body.appendChild(note);
  page.appendChild(body);
  root.appendChild(page);

  let data = null;
  try {
    data = await api.rpcSoft('settings_page', { page: pageId }, null);
  } catch (err) {
    data = null;
  }
  if (token !== seq) return;

  body.textContent = '';
  if (!data || typeof data !== 'object') {
    body.appendChild(emptyNote());
    return;
  }

  const heading = text(data.title).trim();
  if (heading) title.textContent = heading;
  const subtitle = text(data.subtitle).trim();
  if (subtitle) body.appendChild(api.el('p', 'set-sub', subtitle));

  const list = Array.isArray(data.sections) ? data.sections : [];
  let written = 0;
  for (const section of list) {
    if (!section || typeof section !== 'object') continue;
    written += 1;
    body.appendChild(sectionNode(section));
  }
  if (!written) body.appendChild(emptyNote());
}

function sectionNode(section) {
  const wrap = api.el('section', 'set-section');
  const heading = text(section.title).trim();
  if (heading) wrap.appendChild(api.el('p', 'set-caps', heading));
  const rows = Array.isArray(section.rows) ? section.rows : [];
  if (!rows.length) {
    wrap.appendChild(emptyNote());
  } else {
    const card = api.el('div', 'set-card');
    const rowCtx = {
      kinds: KINDS,
      toneOf,
      icon,
      onSaved: () => {
        overviewPending = null;
      },
    };
    for (const row of rows) {
      if (!row || typeof row !== 'object') continue;
      card.appendChild(renderSettingRow(row, api, rowCtx));
    }
    wrap.appendChild(card);
  }
  const note = text(section.note).trim();
  if (note) wrap.appendChild(api.el('p', 'set-note', note));
  return wrap;
}
