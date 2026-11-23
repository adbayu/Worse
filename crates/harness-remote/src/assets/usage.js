// Usage tab shell: segmented control (Cost / Tokens / Limits), range chips (24h/7d/30d/90d), RPC loader.
import { bottomNav } from '/assets/settings.js';
import { renderCostTab, renderTokensTab, renderLimitsTab } from '/assets/usage_charts.js';

let root = null;
let api = null;
let bodyNode = null;
let segNode = null;
let rangeNode = null;
let refreshBtn = null;
let seq = 0;

const RANGES = [
  { id: 1, label: '24h', title: 'Last 24 hours' },
  { id: 7, label: '7d', title: 'Last 7 days' },
  { id: 30, label: '30d', title: 'Last 30 days' },
  { id: 90, label: '90d', title: 'Last 90 days' },
];
const TABS = [
  { id: 'cost', label: 'Cost' },
  { id: 'tokens', label: 'Tokens' },
  { id: 'limits', label: 'Limits' },
];
const REFRESH_SVG =
  '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12a9 9 0 1 1-2.64-6.36L21 8"/><path d="M21 3v5h-5"/></svg>';
const EMPTY_SVG =
  '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="12" width="4" height="8" rx="1"/><rect x="10" y="5" width="4" height="15" rx="1"/><rect x="17" y="9" width="4" height="11" rx="1"/></svg>';

const state = {
  tab: 'cost',
  days: 7,
  loading: false,
  error: '',
  data: null,
  selectedDay: null,
};

function emptyCard(title, sub) {
  const box = api.el('div', 'us-empty');
  const ico = api.el('span', 'us-empty-ico');
  ico.setAttribute('aria-hidden', 'true');
  ico.innerHTML = EMPTY_SVG;
  box.append(ico, api.el('p', 'us-empty-title', title));
  if (sub) box.appendChild(api.el('p', 'us-empty-sub', sub));
  return box;
}

function rangeTitle() {
  const hit = RANGES.find((r) => r.id === state.days);
  return hit ? hit.title : `Last ${state.days} days`;
}

function build(node) {
  node.textContent = '';
  const page = api.el('div', 'us-page');

  const head = api.el('header', 'us-head');
  head.appendChild(api.el('h1', 'us-title', 'Usage'));
  refreshBtn = api.el('button', 'us-refresh');
  refreshBtn.type = 'button';
  refreshBtn.setAttribute('aria-label', 'Refresh usage');
  refreshBtn.innerHTML = REFRESH_SVG;
  refreshBtn.addEventListener('click', () => load(true));
  head.appendChild(refreshBtn);
  page.appendChild(head);

  const controls = api.el('div', 'us-controls');
  segNode = api.el('div', 'us-seg');
  segNode.setAttribute('role', 'tablist');
  segNode.setAttribute('aria-label', 'Usage view');
  controls.appendChild(segNode);

  rangeNode = api.el('div', 'us-ranges');
  rangeNode.setAttribute('role', 'group');
  rangeNode.setAttribute('aria-label', 'Time range');
  controls.appendChild(rangeNode);
  page.appendChild(controls);

  bodyNode = api.el('div', 'us-body');
  page.appendChild(bodyNode);
  page.appendChild(bottomNav(api, 'usage'));
  node.appendChild(page);
}

function paintControls() {
  segNode.textContent = '';
  for (const t of TABS) {
    const btn = api.el('button', 'us-seg-btn' + (state.tab === t.id ? ' on' : ''), t.label);
    btn.type = 'button';
    btn.dataset.tab = t.id;
    btn.setAttribute('role', 'tab');
    btn.setAttribute('aria-selected', state.tab === t.id ? 'true' : 'false');
    btn.addEventListener('click', () => {
      if (state.tab === t.id) return;
      state.tab = t.id;
      paintControls();
      paintBody();
    });
    segNode.appendChild(btn);
  }
  rangeNode.hidden = state.tab === 'limits';
  rangeNode.textContent = '';
  for (const r of RANGES) {
    const btn = api.el('button', 'us-chip' + (state.days === r.id ? ' on' : ''), r.label);
    btn.type = 'button';
    btn.dataset.days = String(r.id);
    btn.setAttribute('aria-pressed', state.days === r.id ? 'true' : 'false');
    btn.addEventListener('click', () => {
      if (state.days === r.id) return;
      state.days = r.id;
      state.selectedDay = null;
      paintControls();
      load(true);
    });
    rangeNode.appendChild(btn);
  }
  if (refreshBtn) refreshBtn.classList.toggle('is-spin', state.loading);
}

function paintBody() {
  if (!bodyNode) return;
  bodyNode.textContent = '';
  if (state.loading && !state.data) {
    bodyNode.appendChild(api.el('p', 'us-status', 'Loading usage…'));
    return;
  }
  if (state.error && !state.data) {
    bodyNode.appendChild(emptyCard('Could not load usage', state.error));
    return;
  }
  const data = state.data || {};
  const label = rangeTitle();
  let nodes = [];
  if (state.tab === 'cost') nodes = renderCostTab(api, data, label, emptyCard);
  else if (state.tab === 'tokens') nodes = renderTokensTab(api, data, state, label, emptyCard);
  else nodes = renderLimitsTab(api, data, emptyCard);
  for (const n of nodes) bodyNode.appendChild(n);
}

async function load(force) {
  if (!api) return;
  if (!force && state.data && state.data.days === state.days) {
    paintBody();
    return;
  }
  seq += 1;
  const token = seq;
  state.loading = true;
  state.error = '';
  paintControls();
  if (!state.data) paintBody();
  try {
    const res = await api.rpc('usage_stats', { days: state.days });
    if (token !== seq) return;
    state.data = res && typeof res === 'object' ? res : { days: state.days, rows: [], costs: [], limits: [] };
  } catch (err) {
    if (token !== seq) return;
    state.error = (err && err.message) || 'Failed to load usage';
    if (state.data) api.toast(state.error);
  } finally {
    if (token === seq) {
      state.loading = false;
      paintControls();
      paintBody();
    }
  }
}

export function initUsage(node, viewApi) {
  if (!node || !viewApi) return;
  root = node;
  api = viewApi;
  build(node);
  paintControls();
}

export function showUsage(subTab) {
  if (!root || !api) return;
  if (subTab && TABS.some((t) => t.id === subTab)) state.tab = subTab;
  paintControls();
  load(false);
}
