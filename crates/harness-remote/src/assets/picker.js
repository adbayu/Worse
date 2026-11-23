// Full-screen model & options picker for the Harness phone client.
// Grouped by agent (collapsible), search filter, localStorage favourites, plus Options (Reasoning, Mode, Deep research).
import { agentGlyph } from '/assets/avatar.js';

const FAVS_KEY = 'harness.model.favs';
const MODELS_KEY = 'harness.agent.models';
const DEFAULT_ID = '__default__';

const ICON_CLOSE =
  '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>';
const ICON_BACK =
  '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M19 12H5M12 19l-7-7 7-7"/></svg>';
const ICON_SEARCH =
  '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>';
const ICON_CHEV_UP =
  '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m18 15-6-6-6 6"/></svg>';
const ICON_CHEV_RIGHT =
  '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 6 6 6-6 6"/></svg>';
const ICON_STAR_OUTLINE =
  '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2L12 17.3 6.4 20.2l1.1-6.2L3 9.6l6.2-.9Z"/></svg>';
const ICON_STAR_SOLID =
  '<svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" aria-hidden="true"><path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2L12 17.3 6.4 20.2l1.1-6.2L3 9.6l6.2-.9Z"/></svg>';

let host = null;
let api = null;
let subView = 'main'; // 'main' | 'effort' | 'mode'
let query = '';
let switchingKey = '';
const collapsed = new Set();
let favs = loadFavs();
let modelCache = loadModelCache();

function loadFavs() {
  try {
    const raw = JSON.parse(localStorage.getItem(FAVS_KEY) || '[]');
    return new Set(Array.isArray(raw) ? raw.filter((v) => typeof v === 'string') : []);
  } catch (_) {
    return new Set();
  }
}

function saveFavs() {
  try {
    localStorage.setItem(FAVS_KEY, JSON.stringify([...favs].slice(0, 100)));
  } catch (_) {}
}

function loadModelCache() {
  try {
    const raw = JSON.parse(localStorage.getItem(MODELS_KEY) || '{}');
    return raw && typeof raw === 'object' ? raw : {};
  } catch (_) {
    return {};
  }
}

function saveModelCache() {
  try {
    localStorage.setItem(MODELS_KEY, JSON.stringify(modelCache));
  } catch (_) {}
}

/** Remember the latest model list fetched for an agent so cross-agent switching shows real models. */
export function cacheAgentModels(agentId, list) {
  if (!agentId || !Array.isArray(list) || !list.length) return;
  modelCache[agentId] = list.map((m) => ({
    id: String(m.id || ''),
    name: String(m.name || m.id || ''),
    description: m.description ? String(m.description) : '',
  })).filter((m) => m.id);
  saveModelCache();
}

function favKey(agentId, modelId) {
  return agentId + '::' + modelId;
}

function toggleFav(agentId, modelId) {
  const key = favKey(agentId, modelId);
  if (!favs.delete(key)) favs.add(key);
  saveFavs();
  render();
}

function modelsForAgent(agent) {
  const activeAgent = api.activeAgent();
  if (agent.id === activeAgent) {
    const sel = api.select('model');
    if (sel && sel.options.length) {
      return Array.from(sel.options).map((opt, idx) => ({
        id: opt.value,
        name: opt.textContent || opt.value,
        isDefault: idx === 0,
      }));
    }
  }
  const cached = modelCache[agent.id];
  if (Array.isArray(cached) && cached.length) {
    return cached.map((m, idx) => ({ id: m.id, name: m.name || m.id, isDefault: idx === 0 }));
  }
  return [{ id: DEFAULT_ID, name: agent.displayName || agent.id, isDefault: true }];
}

function radioNode(selected) {
  const dot = api.el('span', 'pk-radio' + (selected ? ' on' : ''));
  dot.setAttribute('aria-hidden', 'true');
  return dot;
}

function modelRow(agent, item, activeAgent, currentModelId) {
  const isCurrentAgent = agent.id === activeAgent;
  const selected =
    isCurrentAgent &&
    (item.id === DEFAULT_ID ? !currentModelId : item.id === currentModelId);
  const key = favKey(agent.id, item.id);
  const isFav = favs.has(key);
  const busy = switchingKey === key;

  const row = api.el('div', 'pk-row' + (selected ? ' on' : '') + (busy ? ' is-busy' : ''));
  const main = api.el('button', 'pk-row-main');
  main.type = 'button';
  main.setAttribute('role', 'radio');
  main.setAttribute('aria-checked', selected ? 'true' : 'false');
  main.disabled = Boolean(switchingKey);
  main.appendChild(radioNode(selected));

  const labelWrap = api.el('span', 'pk-name-wrap');
  labelWrap.appendChild(api.el('span', 'pk-name', item.name));
  if (item.isDefault) labelWrap.appendChild(api.el('span', 'pk-tag', 'Default'));
  main.appendChild(labelWrap);

  main.addEventListener('click', () => {
    selectModel(agent.id, item.id).catch(() => undefined);
  });
  row.appendChild(main);

  if (item.id !== DEFAULT_ID) {
    const star = api.el('button', 'pk-star' + (isFav ? ' on' : ''));
    star.type = 'button';
    star.setAttribute('aria-label', (isFav ? 'Remove ' : 'Add ') + item.name + ' favourite');
    star.setAttribute('aria-pressed', isFav ? 'true' : 'false');
    star.innerHTML = isFav ? ICON_STAR_SOLID : ICON_STAR_OUTLINE;
    star.addEventListener('click', (event) => {
      event.stopPropagation();
      toggleFav(agent.id, item.id);
    });
    row.appendChild(star);
  }
  return row;
}

async function selectModel(agentId, modelId) {
  const key = favKey(agentId, modelId);
  const active = api.activeAgent();
  if (agentId === active) {
    if (modelId !== DEFAULT_ID) api.pickSelect('model', modelId);
    render();
    return;
  }
  switchingKey = key;
  render();
  try {
    await api.switchAgentAndModel(agentId, modelId === DEFAULT_ID ? '' : modelId);
  } finally {
    switchingKey = '';
    if (isOpen()) render();
  }
}

function groupBlock(groupKey, headerIcon, title, rows) {
  const isClosed = collapsed.has(groupKey);
  const sec = api.el('section', 'pk-group' + (isClosed ? ' is-closed' : ''));
  const head = api.el('button', 'pk-group-head');
  head.type = 'button';
  head.setAttribute('aria-expanded', isClosed ? 'false' : 'true');
  const left = api.el('span', 'pk-group-l');
  if (headerIcon) left.appendChild(headerIcon);
  left.appendChild(api.el('span', 'pk-group-title', title));
  head.appendChild(left);
  const chev = api.el('span', 'pk-group-chev');
  chev.innerHTML = ICON_CHEV_UP;
  head.appendChild(chev);
  head.addEventListener('click', () => {
    if (collapsed.has(groupKey)) collapsed.delete(groupKey);
    else collapsed.add(groupKey);
    render();
  });
  sec.appendChild(head);

  const card = api.el('div', 'pk-card');
  card.setAttribute('role', 'radiogroup');
  card.hidden = isClosed;
  for (const row of rows) card.appendChild(row);
  sec.appendChild(card);
  return sec;
}

function optionNavRow(label, valueText, targetView) {
  const row = api.el('button', 'pk-opt-row');
  row.type = 'button';
  row.appendChild(api.el('span', 'pk-opt-label', label));
  const right = api.el('span', 'pk-opt-right');
  right.appendChild(api.el('span', 'pk-opt-val', valueText));
  const chev = api.el('span', 'pk-opt-chev');
  chev.innerHTML = ICON_CHEV_RIGHT;
  right.appendChild(chev);
  row.appendChild(right);
  row.addEventListener('click', () => {
    subView = targetView;
    render();
  });
  return row;
}

function researchRow() {
  const on = api.researchOn();
  const row = api.el('button', 'pk-opt-row');
  row.type = 'button';
  row.setAttribute('role', 'switch');
  row.setAttribute('aria-checked', on ? 'true' : 'false');
  row.appendChild(api.el('span', 'pk-opt-label', 'Deep research'));
  const sw = api.el('span', 'pk-switch' + (on ? ' on' : ''));
  sw.appendChild(api.el('span', 'pk-knob'));
  row.appendChild(sw);
  row.addEventListener('click', () => {
    api.toggleResearch();
    render();
  });
  return row;
}

function renderMainBody(body) {
  const agents = api.agentChoices();
  const activeAgent = api.activeAgent();
  const modelSel = api.select('model');
  const currentModelId = modelSel ? modelSel.value : '';
  const needle = query.trim().toLowerCase();

  // Favourites group at top when present and matching search
  const favRows = [];
  for (const agent of agents) {
    for (const item of modelsForAgent(agent)) {
      if (item.id === DEFAULT_ID) continue;
      if (!favs.has(favKey(agent.id, item.id))) continue;
      const hay = (item.name + ' ' + (agent.displayName || agent.id)).toLowerCase();
      if (needle && !hay.includes(needle)) continue;
      favRows.push(modelRow(agent, item, activeAgent, currentModelId));
    }
  }
  if (favRows.length) {
    const starIcon = api.el('span', 'pk-fav-icon');
    starIcon.innerHTML = ICON_STAR_SOLID;
    body.appendChild(groupBlock('__favs__', starIcon, 'Favourites', favRows));
  }

  let matchedAny = favRows.length > 0;
  for (const agent of agents) {
    const items = modelsForAgent(agent).filter((item) => {
      if (!needle) return true;
      return (item.name + ' ' + (agent.displayName || agent.id)).toLowerCase().includes(needle);
    });
    if (!items.length) continue;
    matchedAny = true;
    const rows = items.map((item) => modelRow(agent, item, activeAgent, currentModelId));
    body.appendChild(groupBlock('agent:' + agent.id, agentGlyph(agent.id, 16), agent.displayName || agent.id, rows));
  }

  if (!matchedAny) {
    body.appendChild(api.el('p', 'pk-empty', 'No models match “' + query.trim() + '”'));
  }

  if (!needle) {
    const optSec = api.el('section', 'pk-group');
    optSec.appendChild(api.el('p', 'pk-sec-label', 'Options'));
    const card = api.el('div', 'pk-card');
    const effortSel = api.select('effort');
    if (effortSel && effortSel.options.length) {
      card.appendChild(optionNavRow('Reasoning', api.selectedName(effortSel), 'effort'));
    }
    const modeSel = api.select('mode');
    if (modeSel && modeSel.options.length) {
      card.appendChild(optionNavRow('Mode', api.selectedName(modeSel), 'mode'));
    }
    card.appendChild(researchRow());
    optSec.appendChild(card);
    body.appendChild(optSec);
  }
}

function renderSubBody(body, kind) {
  const select = api.select(kind);
  if (!select || !select.options.length) {
    subView = 'main';
    render();
    return;
  }
  const card = api.el('div', 'pk-card');
  card.setAttribute('role', 'radiogroup');
  for (const opt of select.options) {
    const on = opt.selected;
    const row = api.el('button', 'pk-sub-row' + (on ? ' on' : ''));
    row.type = 'button';
    row.setAttribute('role', 'radio');
    row.setAttribute('aria-checked', on ? 'true' : 'false');
    row.appendChild(radioNode(on));
    const txt = api.el('span', 'pk-sub-text');
    txt.appendChild(api.el('span', 'pk-name', opt.textContent));
    if (opt.title) txt.appendChild(api.el('span', 'pk-desc', opt.title));
    row.appendChild(txt);
    row.addEventListener('click', () => {
      api.pickSelect(kind, opt.value);
      subView = 'main';
      render();
    });
    card.appendChild(row);
  }
  body.appendChild(card);
}

function render() {
  if (!host || !api) return;
  const scrollPrev = host.querySelector('.pk-body');
  const scrollTop = scrollPrev ? scrollPrev.scrollTop : 0;
  const hadFocus = document.activeElement && document.activeElement.classList.contains('pk-search-input');

  host.textContent = '';
  const screen = api.el('div', 'pk-screen');
  screen.setAttribute('role', 'dialog');
  screen.setAttribute('aria-modal', 'true');
  screen.setAttribute('aria-label', subView === 'main' ? 'Select model' : subView === 'effort' ? 'Reasoning' : 'Mode');

  const bar = api.el('header', 'pk-bar');
  const leftBtn = api.el('button', 'pk-icon-btn');
  leftBtn.type = 'button';
  if (subView === 'main') {
    leftBtn.innerHTML = ICON_CLOSE;
    leftBtn.setAttribute('aria-label', 'Close');
    leftBtn.addEventListener('click', closePicker);
    bar.appendChild(leftBtn);

    const searchWrap = api.el('label', 'pk-search');
    searchWrap.innerHTML = ICON_SEARCH;
    const input = api.el('input', 'pk-search-input');
    input.type = 'search';
    input.placeholder = 'Find a model';
    input.setAttribute('aria-label', 'Find a model');
    input.value = query;
    input.autocomplete = 'off';
    input.spellcheck = false;
    input.addEventListener('input', () => {
      query = input.value;
      const body = host.querySelector('.pk-body');
      if (body) {
        body.textContent = '';
        renderMainBody(body);
      }
    });
    searchWrap.appendChild(input);
    bar.appendChild(searchWrap);

    const done = api.el('button', 'pk-done', 'Done');
    done.type = 'button';
    done.addEventListener('click', closePicker);
    bar.appendChild(done);
  } else {
    leftBtn.innerHTML = ICON_BACK;
    leftBtn.setAttribute('aria-label', 'Back');
    leftBtn.addEventListener('click', () => {
      subView = 'main';
      render();
    });
    bar.appendChild(leftBtn);
    bar.appendChild(api.el('h2', 'pk-bar-title', subView === 'effort' ? 'Reasoning' : 'Mode'));
    const done = api.el('button', 'pk-done', 'Done');
    done.type = 'button';
    done.addEventListener('click', closePicker);
    bar.appendChild(done);
  }
  screen.appendChild(bar);

  const body = api.el('div', 'pk-body');
  if (subView === 'main') renderMainBody(body);
  else renderSubBody(body, subView);
  screen.appendChild(body);
  host.appendChild(screen);

  body.scrollTop = scrollTop;
  if (hadFocus) {
    const inp = host.querySelector('.pk-search-input');
    if (inp) {
      inp.focus();
      inp.setSelectionRange(inp.value.length, inp.value.length);
    }
  }
}

export function isOpen() {
  return Boolean(host && !host.hidden);
}

export function openPicker() {
  if (!host || !api) return;
  subView = 'main';
  query = '';
  switchingKey = '';
  render();
  host.hidden = false;
}

export function closePicker() {
  if (!host) return;
  host.hidden = true;
}

export function syncPickerIfOpen() {
  if (isOpen()) render();
}

export function initPicker(container, pickerApi) {
  host = container;
  api = pickerApi;
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape' || !isOpen()) return;
    if (subView !== 'main') {
      subView = 'main';
      render();
    } else {
      closePicker();
    }
  });
}
