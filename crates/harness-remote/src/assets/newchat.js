import { agentAvatar } from '/assets/avatar.js';

// "New chat" bottom sheet: pick a project the desktop knows, pick an agent, optionally type the first
// message, start. Styles: newchat.css.

let api = null;
let wrap = null;
let busy = false;
const pick = { workspace: '', agent: '' };

const FOLDER =
  '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round" aria-hidden="true"><path d="M3 7.5A1.5 1.5 0 0 1 4.5 6H9l2 2h8.5A1.5 1.5 0 0 1 21 9.5v8A1.5 1.5 0 0 1 19.5 19h-15A1.5 1.5 0 0 1 3 17.5Z"/></svg>';
const CHECK =
  '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>';

function last(path) {
  const parts = String(path || '').split(/[\\/]+/).filter(Boolean);
  const name = parts.length ? parts[parts.length - 1] : path;
  return name ? name.charAt(0).toUpperCase() + name.slice(1) : 'Project';
}

function usable(agent) {
  return !agent.availability || agent.availability.state === 'available';
}

async function loadProjects(sessions) {
  let list = [];
  try {
    const projects = await api.rpc('list_projects', {});
    if (Array.isArray(projects)) {
      list = projects
        .filter((p) => p && p.workspacePath)
        .sort((a, b) => (b.lastUsed || 0) - (a.lastUsed || 0))
        .map((p) => ({ path: p.workspacePath, count: p.sessions || 0 }));
    }
  } catch {
    list = [];
  }
  if (!list.length) {
    const seen = new Map();
    for (const s of sessions || []) {
      if (!s.workspacePath || s.parentId) continue;
      seen.set(s.workspacePath, (seen.get(s.workspacePath) || 0) + 1);
    }
    list = [...seen.entries()].map(([path, count]) => ({ path, count }));
  }
  return list;
}

function close() {
  if (!wrap) return;
  wrap.classList.remove('is-open');
  const node = wrap;
  wrap = null;
  setTimeout(() => node.remove(), 220);
}

function section(title) {
  const box = api.el('div', 'nc-sec');
  box.appendChild(api.el('p', 'nc-cap', title));
  return box;
}

function paintProjects(box, projects, onPick) {
  const card = api.el('div', 'nc-card');
  if (!projects.length) card.appendChild(api.el('p', 'nc-empty', 'No projects yet — open one on the desktop first.'));
  for (const project of projects) {
    const on = project.path === pick.workspace;
    const row = api.el('button', 'nc-row' + (on ? ' on' : ''));
    row.type = 'button';
    const tile = api.el('span', 'nc-tile');
    tile.innerHTML = FOLDER;
    row.appendChild(tile);
    const text = api.el('span', 'nc-text');
    text.appendChild(api.el('span', 'nc-name', last(project.path)));
    text.appendChild(api.el('span', 'nc-path', project.path));
    row.appendChild(text);
    const check = api.el('span', 'nc-check');
    if (on) check.innerHTML = CHECK;
    row.appendChild(check);
    row.addEventListener('click', () => onPick(project.path));
    card.appendChild(row);
  }
  box.appendChild(card);
}

function paintAgents(box, agents, onPick) {
  const grid = api.el('div', 'nc-agents');
  for (const agent of agents) {
    const on = agent.id === pick.agent;
    const chip = api.el('button', 'nc-agent' + (on ? ' on' : ''));
    chip.type = 'button';
    chip.setAttribute('aria-pressed', on ? 'true' : 'false');
    const name = agent.displayName || agent.id;
    chip.appendChild(agentAvatar(agent.id, name, { small: true }));
    chip.appendChild(api.el('span', 'nc-agent-t', name));
    chip.addEventListener('click', () => onPick(agent.id));
    grid.appendChild(chip);
  }
  box.appendChild(grid);
}

async function start(textarea, button) {
  if (busy || !pick.workspace) return;
  busy = true;
  button.disabled = true;
  button.querySelector('b').textContent = 'Starting…';
  try {
    const params = { workspacePath: pick.workspace };
    if (pick.agent) params.agent = pick.agent;
    const created = await api.rpc('create_session', params);
    const id = created && (created.id || (created.summary && created.summary.id));
    if (!id) throw new Error('The desktop did not return a session');
    const text = textarea.value.trim();
    if (text) {
      const clientMessageId = 'm' + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
      await api.rpc('send_message', { sessionId: id, text, agent: null, clientMessageId, context: [] });
    }
    close();
    location.hash = api.sessionHash(id);
    api.reload();
  } catch (err) {
    api.toast(err && err.message ? err.message : 'Could not start a chat');
    button.disabled = false;
    button.querySelector('b').textContent = 'Start chat';
  } finally {
    busy = false;
  }
}

export async function openNewChat(viewApi, options) {
  api = viewApi;
  if (wrap) return;
  const opts = options || {};
  const known = api.agents();
  const agents = [...(known instanceof Map ? known.values() : known || [])].filter(usable);
  if (!pick.agent || !agents.some((a) => a.id === pick.agent)) pick.agent = agents.length ? agents[0].id : '';
  if (opts.workspacePath) pick.workspace = opts.workspacePath;

  wrap = api.el('div', 'nc-wrap');
  const backdrop = api.el('div', 'nc-backdrop');
  backdrop.addEventListener('click', close);
  const sheet = api.el('div', 'nc-sheet');
  sheet.setAttribute('role', 'dialog');
  sheet.setAttribute('aria-modal', 'true');
  sheet.setAttribute('aria-label', 'New chat');
  sheet.appendChild(api.el('span', 'nc-grab'));

  const head = api.el('div', 'nc-head');
  head.appendChild(api.el('h2', 'nc-title', 'New chat'));
  const x = api.el('button', 'nc-x');
  x.type = 'button';
  x.setAttribute('aria-label', 'Close');
  x.innerHTML =
    '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg>';
  x.addEventListener('click', close);
  head.appendChild(x);
  sheet.appendChild(head);

  const body = api.el('div', 'nc-body');
  const projBox = section('Project');
  const agentBox = section('Agent');
  const msgBox = section('First message');
  const textarea = api.el('textarea', 'nc-input');
  textarea.rows = 3;
  textarea.placeholder = 'Optional — what should the agent do?';
  msgBox.appendChild(textarea);
  body.append(projBox, agentBox, msgBox);
  sheet.appendChild(body);

  const foot = api.el('div', 'nc-foot');
  const go = api.el('button', 'nc-go');
  go.type = 'button';
  go.innerHTML = '<b>Start chat</b><small></small>';
  foot.appendChild(go);
  sheet.appendChild(foot);

  wrap.append(backdrop, sheet);
  document.body.appendChild(wrap);
  requestAnimationFrame(() => wrap && wrap.classList.add('is-open'));
  const onKey = (event) => {
    if (!wrap) document.removeEventListener('keydown', onKey);
    else if (event.key === 'Escape') close();
  };
  document.addEventListener('keydown', onKey);

  let projects = [];
  let loaded = false;
  const sub = go.querySelector('small');
  const repaint = () => {
    projBox.querySelector('.nc-card')?.remove();
    if (loaded) paintProjects(projBox, projects, (path) => {
      pick.workspace = path;
      repaint();
    });
    agentBox.querySelector('.nc-agents')?.remove();
    paintAgents(agentBox, agents, (id) => {
      pick.agent = id;
      repaint();
    });
    agentBox.hidden = agents.length < 2;
    const agentName = pick.agent ? api.agentName(pick.agent) : '';
    sub.textContent = pick.workspace ? [last(pick.workspace), agentName].filter(Boolean).join(' · ') : '';
    go.disabled = !pick.workspace;
  };
  projBox.appendChild(api.el('p', 'nc-empty', 'Loading projects…'));
  repaint();
  go.addEventListener('click', () => start(textarea, go));

  projects = await loadProjects(opts.sessions);
  loaded = true;
  projBox.querySelector('.nc-empty')?.remove();
  if (!projects.some((p) => p.path === pick.workspace)) pick.workspace = projects.length ? projects[0].path : '';
  if (wrap) repaint();
}
