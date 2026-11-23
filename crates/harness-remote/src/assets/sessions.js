import { bottomNav } from '/assets/settings.js';
import { agentAvatar } from '/assets/avatar.js';
import { openNewChat } from '/assets/newchat.js';

// Session list: large title, glass search pill, agent pill tabs, project sections of inset glass cards with
// round agent avatars (styles: sessions.css + refresh.css).

const FALLBACK_GROUP = '';

let root = null;
let api = null;
let listNode = null;
let connDot = null;
let connText = null;
let tunnelChip = null;
let searchInput = null;
let tabsNode = null;

let model = { sessions: [], statusText: '', live: false, remote: null };
let query = '';
let agentFilter = '';
let tabsKey = '';
const collapsed = new Set();

function groupKey(session) {
  const parts = String(session.workspacePath || '').split(/[\\/]+/).filter(Boolean);
  if (parts.length >= 2) return parts.slice(-2).join('/');
  return parts.length ? parts[0] : FALLBACK_GROUP;
}

function projectName(key) {
  if (!key) return 'Other';
  const last = key.split('/').pop() || key;
  return last.charAt(0).toUpperCase() + last.slice(1);
}

function matches(session) {
  if (agentFilter && session.activeAgent !== agentFilter) return false;
  if (!query) return true;
  const hay = [session.title, session.workspacePath, api.agentName(session.activeAgent)]
    .join('\n')
    .toLowerCase();
  return hay.includes(query);
}

function groupSessions(sessions) {
  const pinned = [];
  const folders = new Map();
  for (const session of sessions) {
    if (session.pinned) {
      pinned.push(session);
      continue;
    }
    const key = groupKey(session);
    if (!folders.has(key)) folders.set(key, []);
    folders.get(key).push(session);
  }
  const byNewest = (a, b) => (b.updatedAt || 0) - (a.updatedAt || 0);
  for (const items of folders.values()) items.sort(byNewest);
  const groups = [];
  if (pinned.length) groups.push({ key: null, label: 'Pinned', path: '', pinned: true, sessions: pinned });
  const ordered = [...folders.entries()].sort((a, b) => {
    const emptyA = a[0] === FALLBACK_GROUP ? 1 : 0;
    const emptyB = b[0] === FALLBACK_GROUP ? 1 : 0;
    if (emptyA !== emptyB) return emptyA - emptyB;
    return byNewest(a[1][0], b[1][0]);
  });
  for (const [key, items] of ordered) {
    const path = items[0].workspacePath || '';
    groups.push({ key, label: projectName(key), path, pinned: false, sessions: items });
  }
  return groups;
}

function row(session) {
  const a = api.el('a', 'sx-row' + (session.unread ? ' is-unread' : ''));
  a.href = api.sessionHash(session.id);
  const badge = session.running ? 'running' : session.unread ? 'unread' : '';
  a.appendChild(agentAvatar(session.activeAgent, api.agentName(session.activeAgent), { badge }));

  const main = api.el('span', 'sx-main');
  main.appendChild(
    api.el('span', 'sx-title', session.title || api.baseName(session.workspacePath) || 'Untitled'),
  );
  main.appendChild(api.el('span', 'sx-sub', api.agentName(session.activeAgent)));
  a.appendChild(main);

  const side = api.el('span', 'sx-side');
  side.appendChild(api.el('span', 'sx-when', api.fmtWhen(session.updatedAt) || ''));
  if (session.running) side.appendChild(api.el('span', 'sx-state is-run', 'Running'));
  else if (session.unread) side.appendChild(api.el('span', 'sx-state is-new', 'New'));
  a.appendChild(side);

  const li = api.el('li');
  li.appendChild(a);
  return li;
}

function groupNode(group) {
  const wrap = api.el('section', 'sx-group');
  const closed = !group.pinned && collapsed.has(group.key);
  const head = api.el(group.pinned ? 'div' : 'button', 'sx-sec');
  if (!group.pinned) {
    head.type = 'button';
    head.setAttribute('aria-expanded', closed ? 'false' : 'true');
    head.addEventListener('click', () => {
      if (collapsed.has(group.key)) collapsed.delete(group.key);
      else collapsed.add(group.key);
      paint();
    });
  }
  const label = api.el('span', 'sx-sec-l');
  label.appendChild(api.el('span', 'sx-sec-title', group.label));
  if (group.path) label.appendChild(api.el('span', 'sx-sec-path', group.path));
  head.appendChild(label);
  const right = api.el('span', 'sx-sec-r');
  right.appendChild(api.el('span', 'sx-count', String(group.sessions.length)));
  if (!group.pinned) right.appendChild(api.el('span', 'sx-chev' + (closed ? ' is-closed' : ''), '›'));
  head.appendChild(right);
  const bar = api.el('div', 'sx-sec-bar');
  bar.appendChild(head);
  if (!group.pinned && group.path) {
    const add = api.el('button', 'sx-sec-add');
    add.type = 'button';
    add.setAttribute('aria-label', 'New chat in ' + group.label);
    add.innerHTML = PLUS;
    add.addEventListener('click', () => newChat(group.path));
    bar.appendChild(add);
  }
  wrap.appendChild(bar);

  const rows = api.el('ul', 'sx-card');
  rows.hidden = closed;
  for (const session of group.sessions) rows.appendChild(row(session));
  wrap.appendChild(rows);
  return wrap;
}

const PLUS =
  '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14"/></svg>';

function newChat(workspacePath) {
  openNewChat(api, { workspacePath, sessions: model.sessions });
}

function paint() {
  if (!listNode) return;
  listNode.textContent = '';
  if (model.statusText) listNode.appendChild(api.el('p', 'sx-status', model.statusText));
  const visible = (Array.isArray(model.sessions) ? model.sessions : []).filter(matches);
  if (!visible.length) {
    if (!model.statusText) {
      listNode.appendChild(
        api.el('p', 'sx-empty', query || agentFilter ? 'No matching sessions' : 'No sessions yet'),
      );
    }
    return;
  }
  for (const group of groupSessions(visible)) listNode.appendChild(groupNode(group));
}

function paintTabs() {
  if (!tabsNode || !api) return;
  const names = new Map();
  for (const session of Array.isArray(model.sessions) ? model.sessions : []) {
    const id = session.activeAgent;
    if (id && !names.has(id)) names.set(id, api.agentName(id));
  }
  if (agentFilter && !names.has(agentFilter)) agentFilter = '';
  const key = [...names.entries()].flat().join('|') + '#' + agentFilter;
  if (key === tabsKey) return;
  tabsKey = key;
  tabsNode.textContent = '';
  tabsNode.hidden = names.size < 2;
  const tab = (id, label) => {
    const btn = api.el('button', 'sx-tab' + (agentFilter === id ? ' on' : ''));
    btn.type = 'button';
    btn.setAttribute('aria-pressed', agentFilter === id ? 'true' : 'false');
    if (id) btn.appendChild(agentAvatar(id, label, { small: true }));
    btn.appendChild(api.el('span', 'sx-tab-t', label));
    btn.addEventListener('click', () => {
      agentFilter = id;
      paintTabs();
      paint();
    });
    tabsNode.appendChild(btn);
  };
  tab('', 'All');
  for (const [id, label] of names) tab(id, label);
}

function setConnection() {
  if (!connDot || !connText) return;
  const live = Boolean(model.live);
  connDot.classList.toggle('is-off', !live);
  let text = live ? 'Connected' : 'Reconnecting…';
  const version = model.remote && model.remote.version;
  if (live && version) text += ' · v' + version;
  connText.textContent = text;
  const tunnel =
    (model.remote && (model.remote.tunnel || model.remote.via_tunnel)) ||
    /trycloudflare\.com$/i.test(location.hostname);
  tunnelChip.hidden = !tunnel;
}

function build(node) {
  node.textContent = '';

  const head = api.el('header', 'sx-head');
  const left = api.el('div', 'sx-head-l');
  left.appendChild(api.el('h1', 'sx-h1', 'Chats'));
  const conn = api.el('div', 'sx-conn');
  connDot = api.el('span', 'sx-conn-dot');
  connDot.setAttribute('aria-hidden', 'true');
  connText = api.el('span', 'sx-conn-text', 'Connecting…');
  tunnelChip = api.el('span', 'sx-tunnel', 'via tunnel');
  tunnelChip.hidden = true;
  conn.append(connDot, connText, tunnelChip);
  left.appendChild(conn);
  head.appendChild(left);
  const avatar = api.el('button', 'sx-me');
  avatar.type = 'button';
  avatar.setAttribute('aria-label', 'Settings');
  avatar.innerHTML =
    '<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="8.5" r="3.5"/><path d="M5 19.5c1.4-3 4-4.5 7-4.5s5.6 1.5 7 4.5"/></svg>';
  avatar.addEventListener('click', () => api.openSettings(''));
  head.appendChild(avatar);
  node.appendChild(head);

  const search = api.el('label', 'sx-search');
  search.innerHTML =
    '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>';
  searchInput = api.el('input', 'sx-input');
  searchInput.type = 'search';
  searchInput.placeholder = 'Search chats, projects, agents';
  searchInput.setAttribute('aria-label', 'Search sessions');
  searchInput.autocomplete = 'off';
  searchInput.spellcheck = false;
  const onSearch = () => {
    query = searchInput.value.trim().toLowerCase();
    paint();
  };
  searchInput.addEventListener('input', onSearch);
  searchInput.addEventListener('search', onSearch);
  search.appendChild(searchInput);
  node.appendChild(search);

  tabsNode = api.el('div', 'sx-tabs');
  tabsNode.setAttribute('role', 'toolbar');
  tabsNode.setAttribute('aria-label', 'Filter by agent');
  node.appendChild(tabsNode);

  listNode = api.el('div', 'sx-list');
  node.appendChild(listNode);

  node.appendChild(bottomNav(api, 'chats', { title: 'New chat', onClick: () => newChat('') }));
}

export function initSessions(node, viewApi) {
  if (!node || !viewApi) return;
  root = node;
  api = viewApi;
  build(node);
  setConnection();
}

export function renderSessions(next) {
  if (!root || !api) return;
  model = next || { sessions: [], statusText: '', live: false, remote: null };
  if (!Array.isArray(model.sessions)) model.sessions = [];
  setConnection();
  paintTabs();
  paint();
}
