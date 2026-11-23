// Renderers for Usage view tabs: Cost, Tokens (stacked daily chart + Totals), and Limits.
import { agentAvatar } from '/assets/avatar.js';

export function num(v) {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0;
}

export function fmtMoney(amount, currency) {
  const cur = (currency || 'USD').toUpperCase();
  const n = num(amount);
  const sym = cur === 'USD' ? '$' : cur === 'EUR' ? '€' : cur === 'GBP' ? '£' : cur + ' ';
  if (n === 0) return sym + '0.00';
  if (n < 0.01) return sym + n.toFixed(4);
  if (n >= 1000) return sym + n.toLocaleString(undefined, { maximumFractionDigits: 0 });
  return sym + n.toFixed(2);
}

export function fmtTokens(n) {
  const v = Math.round(num(n));
  if (v >= 1_000_000_000) return (v / 1_000_000_000).toFixed(2).replace(/\.?0+$/, '') + 'B';
  if (v >= 1_000_000) return (v / 1_000_000).toFixed(2).replace(/\.?0+$/, '') + 'M';
  if (v >= 10_000) return (v / 1_000).toFixed(1).replace(/\.0$/, '') + 'K';
  if (v >= 1_000) return (v / 1_000).toFixed(2).replace(/\.?0+$/, '') + 'K';
  return String(v);
}

function hexToHsl(hex) {
  const clean = String(hex || '').trim().replace(/^#/, '');
  if (!/^[0-9a-f]{6}$/i.test(clean)) return [224, 92, 62];
  const r = parseInt(clean.slice(0, 2), 16) / 255;
  const g = parseInt(clean.slice(2, 4), 16) / 255;
  const b = parseInt(clean.slice(4, 6), 16) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  let h = 0;
  let s = 0;
  const l = (max + min) / 2;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
  }
  return [Math.round(h), Math.round(s * 100), Math.round(l * 100)];
}

// Derive per-agent palette from current theme --acc with distinct hue shifts.
function agentPalette(count) {
  const style = getComputedStyle(document.documentElement);
  const acc = style.getPropertyValue('--acc').trim() || '#4d7cfe';
  const light = document.documentElement.getAttribute('data-scheme') === 'light';
  const [h, s, l] = hexToHsl(acc);
  const shifts = [0, 48, -54, 148, -126, 92, 196];
  const sat = Math.max(54, Math.min(86, s || 72));
  const lum = light ? Math.max(40, Math.min(54, l)) : Math.max(58, Math.min(70, l));
  const out = [];
  for (let i = 0; i < Math.max(1, count); i++) {
    const hue = (h + shifts[i % shifts.length] + 360) % 360;
    out.push(i === 0 ? acc : `hsl(${hue} ${sat}% ${lum}%)`);
  }
  return out;
}

function fmtDayShort(ms, days) {
  const d = new Date(ms);
  if (days <= 7) return d.toLocaleDateString(undefined, { weekday: 'short', timeZone: 'UTC' });
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' });
}

function heroCard(api, kicker, pill, big, sub) {
  const hero = api.el('section', 'us-card us-hero');
  const top = api.el('div', 'us-hero-top');
  top.append(api.el('span', 'us-kicker', kicker), api.el('span', 'us-pill', pill));
  hero.append(top, api.el('p', 'us-big', big));
  if (sub) hero.appendChild(api.el('p', 'us-hero-sub', sub));
  return hero;
}

function agentListSection(api, items, colorOf, formatVal, total) {
  const sec = api.el('section', 'us-section');
  sec.appendChild(api.el('p', 'us-cap', 'By agent'));
  const card = api.el('div', 'us-card us-list-card');
  for (const item of items) {
    const pct = total > 0 ? Math.round((item.value / total) * 100) : 0;
    const row = api.el('div', 'us-agent-row');
    const top = api.el('div', 'us-agent-top');
    const left = api.el('div', 'us-agent-l');
    const dot = api.el('span', 'us-dot');
    dot.style.background = colorOf.get(item.id);
    left.append(dot, api.el('span', 'us-agent-name', item.name));
    const right = api.el('div', 'us-agent-r');
    right.append(
      api.el('span', 'us-agent-val', formatVal(item.value)),
      api.el('span', 'us-agent-pct', pct + '%'),
    );
    top.append(left, right);
    const track = api.el('div', 'us-bar-track');
    const fill = api.el('div', 'us-bar-fill');
    fill.style.width = Math.max(2, pct) + '%';
    fill.style.background = colorOf.get(item.id);
    track.appendChild(fill);
    row.append(top, track);
    card.appendChild(row);
  }
  sec.appendChild(card);
  return sec;
}

export function renderCostTab(api, data, rangeLabel, emptyCard) {
  const costs = Array.isArray(data.costs) ? data.costs : [];
  const rows = Array.isArray(data.rows) ? data.rows : [];
  const totalTurns = rows.reduce((s, r) => s + num(r.turns), 0);
  const byAgent = new Map();
  let currency = 'USD';
  for (const c of costs) {
    if (!c || typeof c !== 'object') continue;
    if (c.currency) currency = c.currency;
    const id = c.agentId || c.agentName || 'other';
    const cur = byAgent.get(id) || { id, name: c.agentName || api.agentName(id), value: 0 };
    cur.value += num(c.amount);
    byAgent.set(id, cur);
  }
  const items = [...byAgent.values()].filter((x) => x.value > 0).sort((a, b) => b.value - a.value);
  const total = items.reduce((s, x) => s + x.value, 0);
  if (!items.length) {
    return [emptyCard('No cost data yet', `No recorded spend in ${rangeLabel.toLowerCase()}.`)];
  }
  const palette = agentPalette(items.length);
  const colorOf = new Map(items.map((a, i) => [a.id, palette[i]]));
  const sub = totalTurns > 0
    ? `${totalTurns.toLocaleString()} turn${totalTurns === 1 ? '' : 's'} across ${items.length} agent${items.length === 1 ? '' : 's'}`
    : '';
  return [
    heroCard(api, 'Total spend', rangeLabel, fmtMoney(total, currency), sub),
    agentListSection(api, items, colorOf, (v) => fmtMoney(v, currency), total),
  ];
}

export function renderTokensTab(api, data, state, rangeLabel, emptyCard) {
  const rows = Array.isArray(data.rows) ? data.rows : [];
  if (!rows.length) {
    return [emptyCard('No token activity', `No turns recorded in ${rangeLabel.toLowerCase()}.`)];
  }

  let totIn = 0;
  let totOut = 0;
  let totCacheRead = 0;
  let totCacheWrite = 0;
  let totThought = 0;
  const byAgent = new Map();
  const byDay = new Map();

  for (const r of rows) {
    if (!r || typeof r !== 'object') continue;
    const inp = num(r.input);
    const out = num(r.output);
    const cr = num(r.cachedRead);
    const cw = num(r.cachedWrite);
    const th = num(r.thought);
    const io = inp + out;
    totIn += inp;
    totOut += out;
    totCacheRead += cr;
    totCacheWrite += cw;
    totThought += th;

    const id = r.agentId || r.agentName || 'other';
    const ag = byAgent.get(id) || { id, name: r.agentName || api.agentName(id), value: 0 };
    ag.value += io;
    byAgent.set(id, ag);

    const dayKey = num(r.day);
    if (dayKey) {
      let bucket = byDay.get(dayKey);
      if (!bucket) {
        bucket = { day: dayKey, total: 0, agents: new Map() };
        byDay.set(dayKey, bucket);
      }
      bucket.total += io;
      bucket.agents.set(id, (bucket.agents.get(id) || 0) + io);
    }
  }

  const totalIO = totIn + totOut;
  const processed = totalIO + totCacheRead + totCacheWrite + totThought;
  const agents = [...byAgent.values()].sort((a, b) => b.value - a.value);
  const palette = agentPalette(agents.length);
  const colorOf = new Map(agents.map((a, i) => [a.id, palette[i]]));

  const hero = heroCard(
    api,
    'Total tokens',
    rangeLabel,
    fmtTokens(totalIO),
    `${fmtTokens(totIn)} input · ${fmtTokens(totOut)} output`,
  );

  // Daily stacked bar chart
  const chartSec = api.el('section', 'us-section');
  chartSec.appendChild(api.el('p', 'us-cap', 'Daily tokens'));
  const chartCard = api.el('div', 'us-card us-chart-card');
  const daysList = [...byDay.values()].sort((a, b) => a.day - b.day);
  const maxDay = daysList.reduce((m, d) => Math.max(m, d.total), 0) || 1;

  const readout = api.el('div', 'us-readout');
  const paintReadout = () => {
    const sel = daysList.find((d) => d.day === state.selectedDay);
    if (!sel) {
      readout.textContent = `Peak ${fmtTokens(maxDay)} / day`;
      return;
    }
    const label = new Date(sel.day).toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' });
    readout.textContent = `${label} · ${fmtTokens(sel.total)} tokens`;
  };
  paintReadout();
  chartCard.appendChild(readout);

  const bars = api.el('div', 'us-bars');
  const sparse = daysList.length > 10;
  daysList.forEach((d, idx) => {
    const col = api.el('button', 'us-col' + (state.selectedDay === d.day ? ' on' : ''));
    col.type = 'button';
    col.setAttribute('aria-label', `${fmtDayShort(d.day, state.days)}: ${fmtTokens(d.total)} tokens`);
    const stackWrap = api.el('span', 'us-stack-wrap');
    const stack = api.el('span', 'us-stack');
    stack.style.height = (d.total > 0 ? Math.max(6, Math.round((d.total / maxDay) * 100)) : 0) + '%';
    for (const ag of agents) {
      const part = d.agents.get(ag.id) || 0;
      if (!part) continue;
      const seg = api.el('span', 'us-stack-seg');
      seg.style.flexGrow = String(part);
      seg.style.background = colorOf.get(ag.id);
      stack.appendChild(seg);
    }
    stackWrap.appendChild(stack);
    col.appendChild(stackWrap);
    const showLbl = !sparse || idx === 0 || idx === daysList.length - 1 || idx === Math.floor(daysList.length / 2);
    col.appendChild(api.el('span', 'us-col-lbl', showLbl ? fmtDayShort(d.day, state.days) : ''));
    col.addEventListener('click', () => {
      state.selectedDay = state.selectedDay === d.day ? null : d.day;
      bars.querySelectorAll('.us-col').forEach((n, i) => n.classList.toggle('on', daysList[i].day === state.selectedDay));
      paintReadout();
    });
    bars.appendChild(col);
  });
  chartCard.appendChild(bars);

  const legend = api.el('div', 'us-legend');
  for (const ag of agents) {
    const item = api.el('span', 'us-leg-item');
    const dot = api.el('span', 'us-dot');
    dot.style.background = colorOf.get(ag.id);
    item.append(dot, api.el('span', null, ag.name));
    legend.appendChild(item);
  }
  chartCard.appendChild(legend);
  chartSec.appendChild(chartCard);

  const agSec = agentListSection(api, agents, colorOf, fmtTokens, totalIO);

  const totSec = api.el('section', 'us-section');
  totSec.appendChild(api.el('p', 'us-cap', 'Totals'));
  const totCard = api.el('div', 'us-card us-kv-card');
  const kvRows = [
    { label: 'Processed tokens', val: fmtTokens(processed), tone: '' },
    { label: 'Cache savings', val: fmtTokens(totCacheRead), tone: totCacheRead > 0 ? 'ok' : '' },
    { label: 'Cached input', val: fmtTokens(totCacheRead + totCacheWrite), tone: '' },
    { label: 'Uncached input', val: fmtTokens(totIn), tone: '' },
    { label: 'Output', val: fmtTokens(totOut), tone: '' },
  ];
  if (totThought > 0) kvRows.push({ label: 'Reasoning', val: fmtTokens(totThought), tone: '' });
  for (const kv of kvRows) {
    const r = api.el('div', 'us-kv');
    r.append(api.el('span', 'us-kv-k', kv.label), api.el('span', 'us-kv-v' + (kv.tone ? ' is-' + kv.tone : ''), kv.val));
    totCard.appendChild(r);
  }
  totSec.appendChild(totCard);
  return [hero, chartSec, agSec, totSec];
}

function fmtReset(resetText) {
  const clean = typeof resetText === 'string' ? resetText.trim() : '';
  if (!clean) return '';
  return /^resets?\b/i.test(clean) ? clean : 'Resets ' + clean;
}

function limitBar(api, label, used, resetText) {
  const pct = Math.max(0, Math.min(100, num(used)));
  const shown = Math.round(pct) + '%';
  const tone = pct >= 90 ? 'err' : pct >= 75 ? 'warn' : 'acc';
  const block = api.el('div', 'us-lim-row');
  const top = api.el('div', 'us-lim-top');
  top.appendChild(api.el('span', 'us-lim-lbl', label));
  const right = api.el('span', 'us-lim-r');
  const reset = fmtReset(resetText);
  if (reset) right.appendChild(api.el('span', 'us-lim-reset', reset));
  right.appendChild(api.el('span', 'us-lim-pct is-' + tone, shown));
  top.appendChild(right);
  const meter = api.el('div', 'us-meter');
  const fill = api.el('div', 'us-meter-fill is-' + tone);
  fill.style.width = pct + '%';
  meter.appendChild(fill);
  block.append(top, meter);
  return block;
}

export function renderLimitsTab(api, data, emptyCard) {
  const limits = Array.isArray(data.limits) ? data.limits : [];
  if (!limits.length) {
    return [emptyCard('No usage limits yet', 'Connect an account on the desktop or tap refresh to check quotas.')];
  }
  const nodes = [];
  for (const item of limits) {
    if (!item || typeof item !== 'object') continue;
    const card = api.el('section', 'us-card us-lim-card');
    const head = api.el('div', 'us-lim-head');
    const id = item.id || item.label || '';
    head.append(agentAvatar(id, item.label || id, { small: true }), api.el('span', 'us-lim-name', item.label || api.agentName(id)));
    if (item.plan) head.appendChild(api.el('span', 'us-pill', item.plan));
    card.appendChild(head);

    if (item.error) {
      card.appendChild(api.el('p', 'us-lim-err', item.error));
    } else {
      let drawn = 0;
      const hasS = typeof item.sessionUsed === 'number' && Number.isFinite(item.sessionUsed);
      const hasW = typeof item.weekUsed === 'number' && Number.isFinite(item.weekUsed);
      const hasO = typeof item.opusUsed === 'number' && Number.isFinite(item.opusUsed);
      if (hasS) {
        const lbl = (typeof item.sessionLabel === 'string' && item.sessionLabel.trim()) || 'Session';
        card.appendChild(limitBar(api, lbl, item.sessionUsed, item.sessionReset));
        drawn += 1;
      }
      if (hasW) {
        const lbl = (typeof item.weekLabel === 'string' && item.weekLabel.trim()) || 'Weekly';
        card.appendChild(limitBar(api, lbl, item.weekUsed, item.weekReset));
        drawn += 1;
      }
      if (hasO) {
        const lbl = (typeof item.opusLabel === 'string' && item.opusLabel.trim()) || 'Opus';
        card.appendChild(limitBar(api, lbl, item.opusUsed, ''));
        drawn += 1;
      }
      const extraBars = Array.isArray(item.bars) ? item.bars : [];
      for (const bar of extraBars) {
        if (!bar || typeof bar !== 'object') continue;
        if (typeof bar.used !== 'number' || !Number.isFinite(bar.used)) continue;
        const lbl = (typeof bar.label === 'string' && bar.label.trim()) || 'Usage';
        card.appendChild(limitBar(api, lbl, bar.used, bar.reset));
        drawn += 1;
      }
      if (!drawn) card.appendChild(api.el('p', 'us-lim-muted', 'Limits not reported for this account.'));
    }
    nodes.push(card);
  }
  return nodes;
}
