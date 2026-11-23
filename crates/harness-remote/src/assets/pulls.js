// Pulls tab: open/draft PRs across desktop projects via `list_pulls` RPC.
import { bottomNav } from '/assets/settings.js';

let root = null;
let api = null;
let filtersNode = null;
let bodyNode = null;
let countNode = null;
let refreshBtn = null;
let seq = 0;

const FILTERS = [
  { id: 'all', label: 'All' },
  { id: 'open', label: 'Open' },
  { id: 'draft', label: 'Draft' },
];

const REFRESH_SVG =
  '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12a9 9 0 1 1-2.64-6.36L21 8"/><path d="M21 3v5h-5"/></svg>';
const PR_SVG =
  '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="6" cy="6" r="3"/><circle cx="18" cy="18" r="3"/><path d="M6 9v12M13 6h3a2 2 0 0 1 2 2v7M15 4l-2 2 2 2"/></svg>';
const BRANCH_SVG =
  '<svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="6" cy="6" r="3"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M6 9v6M18 9a9 9 0 0 1-9 9"/></svg>';
const EMPTY_SVG =
  '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="6" cy="6" r="3"/><circle cx="18" cy="18" r="3"/><path d="M6 9v12M13 6h3a2 2 0 0 1 2 2v7M15 4l-2 2 2 2"/></svg>';

const state = {
  filter: 'all',
  loading: false,
  error: '',
  data: null,
};

function fmtRelative(iso) {
  if (!iso) return '';
  const ms = typeof iso === 'number' ? iso : Date.parse(iso);
  if (!Number.isFinite(ms)) return '';
  const diff = Math.max(0, Date.now() - ms);
  const mins = Math.floor(diff / 60_000);
  if (mins < 1) return 'just now';
  if (mins < 60) return mins + 'm ago';
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return hrs + 'h ago';
  const days = Math.floor(hrs / 24);
  if (days < 30) return days + 'd ago';
  return new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

function badgeOf(pr) {
  if (pr.isDraft) return { text: 'Draft', tone: 'muted' };
  const dec = String(pr.reviewDecision || '').toUpperCase();
  if (dec === 'APPROVED') return { text: 'Approved', tone: 'ok' };
  if (dec === 'CHANGES_REQUESTED') return { text: 'Changes', tone: 'err' };
  if (dec === 'REVIEW_REQUIRED') return { text: 'Review', tone: 'warn' };
  return null;
}

function matchesFilter(pr) {
  if (state.filter === 'draft') return Boolean(pr.isDraft);
  if (state.filter === 'open') return !pr.isDraft && String(pr.state || 'OPEN').toUpperCase() === 'OPEN';
  return true;
}

function emptyCard(title, sub) {
  const box = api.el('div', 'pr-empty');
  const ico = api.el('span', 'pr-empty-ico');
  ico.setAttribute('aria-hidden', 'true');
  ico.innerHTML = EMPTY_SVG;
  box.append(ico, api.el('p', 'pr-empty-title', title));
  if (sub) box.appendChild(api.el('p', 'pr-empty-sub', sub));
  return box;
}

function build(node) {
  node.textContent = '';
  const page = api.el('div', 'pr-page');

  const head = api.el('header', 'pr-head');
  const left = api.el('div', 'pr-head-l');
  left.appendChild(api.el('h1', 'pr-title', 'Pulls'));
  countNode = api.el('span', 'pr-head-count', '');
  countNode.hidden = true;
  left.appendChild(countNode);
  head.appendChild(left);

  refreshBtn = api.el('button', 'pr-refresh');
  refreshBtn.type = 'button';
  refreshBtn.setAttribute('aria-label', 'Refresh pull requests');
  refreshBtn.innerHTML = REFRESH_SVG;
  refreshBtn.addEventListener('click', () => load(true));
  head.appendChild(refreshBtn);
  page.appendChild(head);

  filtersNode = api.el('div', 'pr-filters');
  filtersNode.setAttribute('role', 'tablist');
  filtersNode.setAttribute('aria-label', 'Filter pull requests');
  page.appendChild(filtersNode);

  bodyNode = api.el('div', 'pr-body');
  page.appendChild(bodyNode);
  page.appendChild(bottomNav(api, 'pulls'));
  node.appendChild(page);
}

function paintFilters() {
  const prs = state.data && Array.isArray(state.data.prs) ? state.data.prs : [];
  const counts = {
    all: prs.length,
    open: prs.filter((p) => !p.isDraft && String(p.state || 'OPEN').toUpperCase() === 'OPEN').length,
    draft: prs.filter((p) => Boolean(p.isDraft)).length,
  };
  countNode.textContent = String(prs.length);
  countNode.hidden = !prs.length;
  filtersNode.hidden = Boolean(state.data && state.data.ghMissing);
  filtersNode.textContent = '';
  for (const f of FILTERS) {
    const on = state.filter === f.id;
    const btn = api.el('button', 'pr-chip' + (on ? ' on' : ''));
    btn.type = 'button';
    btn.dataset.filter = f.id;
    btn.setAttribute('role', 'tab');
    btn.setAttribute('aria-selected', on ? 'true' : 'false');
    btn.appendChild(api.el('span', null, f.label));
    if (prs.length) btn.appendChild(api.el('span', 'pr-chip-n', String(counts[f.id])));
    btn.addEventListener('click', () => {
      if (state.filter === f.id) return;
      state.filter = f.id;
      paintFilters();
      paintBody();
    });
    filtersNode.appendChild(btn);
  }
  if (refreshBtn) refreshBtn.classList.toggle('is-spin', state.loading);
}

function prItemNode(pr) {
  const btn = api.el('button', 'pr-item');
  btn.type = 'button';

  const top = api.el('div', 'pr-top');
  const icon = api.el('span', 'pr-ico' + (pr.isDraft ? ' is-draft' : ' is-open'));
  icon.setAttribute('aria-hidden', 'true');
  icon.innerHTML = PR_SVG;
  top.append(icon, api.el('span', 'pr-num', '#' + (pr.number || '')));
  const badge = badgeOf(pr);
  if (badge) top.appendChild(api.el('span', 'pr-badge is-' + badge.tone, badge.text));
  const when = fmtRelative(pr.updatedAt);
  if (when) top.appendChild(api.el('span', 'pr-when', when));
  btn.appendChild(top);

  btn.appendChild(api.el('p', 'pr-name', pr.title || 'Untitled pull request'));

  const meta = api.el('div', 'pr-meta');
  const metaLeft = api.el('div', 'pr-meta-l');
  if (pr.branch) {
    const br = api.el('span', 'pr-branch');
    const brIco = api.el('span', 'pr-br-ico');
    brIco.setAttribute('aria-hidden', 'true');
    brIco.innerHTML = BRANCH_SVG;
    br.append(brIco, api.el('span', 'pr-br-t', pr.branch));
    metaLeft.appendChild(br);
  }
  if (pr.author) metaLeft.appendChild(api.el('span', 'pr-author', '@' + pr.author));
  meta.appendChild(metaLeft);

  const hasDiff = typeof pr.additions === 'number' || typeof pr.deletions === 'number';
  if (hasDiff) {
    const diff = api.el('span', 'pr-diff');
    diff.append(
      api.el('span', 'pr-add', '+' + (pr.additions || 0)),
      api.el('span', 'pr-del', '−' + (pr.deletions || 0)),
    );
    meta.appendChild(diff);
  }
  btn.appendChild(meta);

  btn.addEventListener('click', () => {
    const url = typeof pr.url === 'string' ? pr.url.trim() : '';
    if (/^https?:\/\//i.test(url)) {
      window.open(url, '_blank', 'noopener,noreferrer');
    } else {
      api.toast('No pull request URL available');
    }
  });
  return btn;
}

function paintBody() {
  if (!bodyNode) return;
  bodyNode.textContent = '';
  if (state.loading && !state.data) {
    bodyNode.appendChild(api.el('p', 'pr-status', 'Loading pull requests…'));
    return;
  }
  if (state.error && !state.data) {
    bodyNode.appendChild(emptyCard('Could not load pull requests', state.error));
    return;
  }
  const data = state.data || {};
  if (data.ghMissing) {
    bodyNode.appendChild(
      emptyCard('GitHub CLI not installed', 'Install GitHub CLI on the desktop (Settings → Setup)'),
    );
    return;
  }

  const prs = (Array.isArray(data.prs) ? data.prs : []).filter(matchesFilter);
  const errors = Array.isArray(data.errors) ? data.errors : [];

  if (!prs.length) {
    const msg = state.filter === 'draft'
      ? 'No draft pull requests'
      : state.filter === 'open'
        ? 'No ready pull requests'
        : 'No open pull requests';
    bodyNode.appendChild(emptyCard(msg, 'Open pull requests across your projects will appear here.'));
  } else {
    const groups = new Map();
    for (const pr of prs) {
      if (!pr || typeof pr !== 'object') continue;
      const key = pr.project || api.baseName(pr.projectPath) || 'Repository';
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(pr);
    }
    for (const [project, list] of groups) {
      const sec = api.el('section', 'pr-sec');
      const secHead = api.el('div', 'pr-sec-head');
      secHead.append(
        api.el('span', 'pr-sec-title', project),
        api.el('span', 'pr-sec-count', String(list.length)),
      );
      sec.appendChild(secHead);
      const card = api.el('div', 'pr-card');
      for (const pr of list) card.appendChild(prItemNode(pr));
      sec.appendChild(card);
      bodyNode.appendChild(sec);
    }
  }

  if (errors.length) {
    const errWrap = api.el('div', 'pr-errors');
    for (const e of errors) {
      if (!e || typeof e !== 'object') continue;
      const proj = e.project ? e.project + ': ' : '';
      errWrap.appendChild(api.el('p', 'pr-err-note', proj + (e.error || 'Could not check repository')));
    }
    bodyNode.appendChild(errWrap);
  }
}

async function load(force) {
  if (!api) return;
  if (!force && state.data) {
    paintFilters();
    paintBody();
    return;
  }
  seq += 1;
  const token = seq;
  state.loading = true;
  state.error = '';
  paintFilters();
  if (!state.data) paintBody();
  try {
    const res = await api.rpc('list_pulls', {});
    if (token !== seq) return;
    state.data = res && typeof res === 'object' ? res : { ghMissing: false, prs: [], errors: [] };
  } catch (err) {
    if (token !== seq) return;
    state.error = (err && err.message) || 'Failed to load pull requests';
    if (state.data) api.toast(state.error);
  } finally {
    if (token === seq) {
      state.loading = false;
      paintFilters();
      paintBody();
    }
  }
}

export function initPulls(node, viewApi) {
  if (!node || !viewApi) return;
  root = node;
  api = viewApi;
  build(node);
  paintFilters();
}

export function showPulls() {
  if (!root || !api) return;
  load(false);
}
