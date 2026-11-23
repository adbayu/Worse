// Interactive settings controls: optimistic toggles, choice sheet, number stepper sheet.
// Uses RPC `set_setting` ({ path, value }) with instant UI update and rollback + toast on error.

let activeSheet = null;

function text(v) {
  return typeof v === 'string' ? v : v == null ? '' : String(v);
}

const CHECK_SVG =
  '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>';
const CLOSE_SVG =
  '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true"><path d="M18 6 6 18M6 6l12 12"/></svg>';

export function closeSettingSheet() {
  if (!activeSheet) return;
  const { wrap, onKey } = activeSheet;
  activeSheet = null;
  document.removeEventListener('keydown', onKey);
  wrap.classList.remove('is-open');
  setTimeout(() => wrap.remove(), 220);
}

function openSheet(api, title, detail, buildBody) {
  closeSettingSheet();
  const wrap = api.el('div', 'se-wrap');
  const backdrop = api.el('div', 'se-backdrop');
  const sheet = api.el('div', 'se-sheet');
  sheet.setAttribute('role', 'dialog');
  sheet.setAttribute('aria-modal', 'true');
  sheet.setAttribute('aria-label', title || 'Edit setting');

  sheet.appendChild(api.el('div', 'se-grab'));
  const head = api.el('div', 'se-head');
  const headText = api.el('div', 'se-head-text');
  headText.appendChild(api.el('h2', 'se-title', title || 'Setting'));
  if (detail) headText.appendChild(api.el('p', 'se-sub', detail));
  head.appendChild(headText);
  const closeBtn = api.el('button', 'se-x');
  closeBtn.type = 'button';
  closeBtn.setAttribute('aria-label', 'Close');
  closeBtn.innerHTML = CLOSE_SVG;
  closeBtn.addEventListener('click', closeSettingSheet);
  head.appendChild(closeBtn);
  sheet.appendChild(head);

  const body = api.el('div', 'se-body');
  buildBody(body);
  sheet.appendChild(body);

  wrap.append(backdrop, sheet);
  backdrop.addEventListener('click', closeSettingSheet);
  const onKey = (ev) => {
    if (ev.key === 'Escape') closeSettingSheet();
  };
  document.addEventListener('keydown', onKey);
  document.body.appendChild(wrap);
  activeSheet = { wrap, onKey };
  requestAnimationFrame(() => wrap.classList.add('is-open'));
}

function showInlineError(container, api, msg) {
  let errNode = container.querySelector('.set-inline-err');
  if (!msg) {
    if (errNode) errNode.remove();
    return;
  }
  if (!errNode) {
    errNode = api.el('p', 'set-inline-err', '');
    errNode.setAttribute('role', 'alert');
    container.appendChild(errNode);
  }
  errNode.textContent = msg;
}

function openChoiceSheet(api, row, heading, detail, onPick) {
  const options = Array.isArray(row.options) ? row.options : [];
  openSheet(api, heading, detail, (body) => {
    const list = api.el('div', 'se-list');
    list.setAttribute('role', 'listbox');
    for (const opt of options) {
      if (!opt || typeof opt !== 'object') continue;
      const optVal = opt.value;
      const optLabel = text(opt.label || opt.value).trim();
      const selected = String(row.current) === String(optVal);
      const btn = api.el('button', 'se-opt' + (selected ? ' on' : ''));
      btn.type = 'button';
      btn.setAttribute('role', 'option');
      btn.setAttribute('aria-selected', selected ? 'true' : 'false');
      const labelSpan = api.el('span', 'se-opt-label', optLabel);
      btn.appendChild(labelSpan);
      const check = api.el('span', 'se-opt-check');
      check.setAttribute('aria-hidden', 'true');
      if (selected) check.innerHTML = CHECK_SVG;
      btn.appendChild(check);
      btn.addEventListener('click', () => {
        closeSettingSheet();
        if (!selected) onPick(optVal, optLabel);
      });
      list.appendChild(btn);
    }
    body.appendChild(list);
  });
}

function openNumberSheet(api, row, heading, detail, onSave) {
  const hasMin = typeof row.min === 'number' && Number.isFinite(row.min);
  const hasMax = typeof row.max === 'number' && Number.isFinite(row.max);
  const step = typeof row.step === 'number' && row.step > 0 ? row.step : 1;
  const initial = typeof row.current === 'number' && Number.isFinite(row.current)
    ? row.current
    : Number(row.value) || (hasMin ? row.min : 0);

  openSheet(api, heading, detail, (body) => {
    let val = initial;
    const clamp = (n) => {
      let next = n;
      if (hasMin && next < row.min) next = row.min;
      if (hasMax && next > row.max) next = row.max;
      return next;
    };

    const stepper = api.el('div', 'se-stepper');
    const minus = api.el('button', 'se-step-btn', '−');
    minus.type = 'button';
    minus.setAttribute('aria-label', 'Decrease');

    const input = api.el('input', 'se-num-input');
    input.type = 'number';
    input.inputMode = 'numeric';
    input.value = String(val);
    if (hasMin) input.min = String(row.min);
    if (hasMax) input.max = String(row.max);
    input.step = String(step);
    input.setAttribute('aria-label', heading || 'Value');

    const plus = api.el('button', 'se-step-btn', '+');
    plus.type = 'button';
    plus.setAttribute('aria-label', 'Increase');

    const errBox = api.el('p', 'se-err', '');
    errBox.hidden = true;

    const syncButtons = () => {
      const n = Number(input.value);
      const valid = input.value.trim() !== '' && Number.isFinite(n);
      minus.disabled = valid && hasMin && n <= row.min;
      plus.disabled = valid && hasMax && n >= row.max;
      errBox.hidden = true;
    };

    minus.addEventListener('click', () => {
      const cur = Number.isFinite(Number(input.value)) ? Number(input.value) : val;
      val = clamp(cur - step);
      input.value = String(val);
      syncButtons();
    });
    plus.addEventListener('click', () => {
      const cur = Number.isFinite(Number(input.value)) ? Number(input.value) : val;
      val = clamp(cur + step);
      input.value = String(val);
      syncButtons();
    });
    input.addEventListener('input', syncButtons);
    syncButtons();

    stepper.append(minus, input, plus);
    body.appendChild(stepper);

    if (hasMin || hasMax) {
      const hint = hasMin && hasMax
        ? `Range: ${row.min} – ${row.max}`
        : hasMin ? `Minimum: ${row.min}` : `Maximum: ${row.max}`;
      body.appendChild(api.el('p', 'se-hint', hint));
    }
    body.appendChild(errBox);

    const actions = api.el('div', 'se-actions');
    const cancelBtn = api.el('button', 'se-btn se-cancel', 'Cancel');
    cancelBtn.type = 'button';
    cancelBtn.addEventListener('click', closeSettingSheet);
    const saveBtn = api.el('button', 'se-btn se-save', 'Save');
    saveBtn.type = 'button';
    saveBtn.addEventListener('click', () => {
      const raw = input.value.trim();
      const parsed = Number(raw);
      if (!raw || !Number.isFinite(parsed)) {
        errBox.textContent = 'Enter a valid number.';
        errBox.hidden = false;
        return;
      }
      if (hasMin && parsed < row.min) {
        errBox.textContent = `Value must be at least ${row.min}.`;
        errBox.hidden = false;
        return;
      }
      if (hasMax && parsed > row.max) {
        errBox.textContent = `Value must be at most ${row.max}.`;
        errBox.hidden = false;
        return;
      }
      closeSettingSheet();
      if (parsed !== initial) onSave(parsed);
    });
    actions.append(cancelBtn, saveBtn);
    body.appendChild(actions);
    setTimeout(() => input.focus(), 60);
  });
}

function lineText(api, heading, detail) {
  const box = api.el('div', 'set-line-text');
  if (heading) box.appendChild(api.el('p', 'set-line-title', heading));
  if (detail) box.appendChild(api.el('p', 'set-line-detail', detail));
  if (!heading && !detail) box.appendChild(api.el('span', 'set-line-detail', ''));
  return box;
}

function glyphBox(api, className, glyph, fallback) {
  const raw = text(glyph).trim();
  const letter = (raw || text(fallback).trim() || '?').charAt(0);
  const box = api.el('span', className, letter.toUpperCase());
  box.setAttribute('aria-hidden', 'true');
  return box;
}

export function renderSettingRow(row, api, ctx) {
  const kind = ctx.kinds.has(row.kind) ? row.kind : 'row';
  const tone = ctx.toneOf(row.tone);
  const heading = text(row.title).trim();
  const detail = text(row.detail).trim();
  const value = text(row.value).trim();
  const settingPath = typeof row.setting === 'string' && row.setting.trim() ? row.setting.trim() : '';
  const glyph = text(row.glyph).trim();

  if (kind === 'code') {
    const line = api.el('div', 'set-line kind-code');
    const code = text(row.code || row.detail || row.value).trim();
    if (heading) line.appendChild(api.el('p', 'set-line-title', heading));
    if (code) {
      const block = api.el('pre', 'set-code');
      block.textContent = code;
      line.appendChild(block);
    }
    return line;
  }

  if (kind === 'toggle') {
    const editable = Boolean(settingPath);
    const baseCls = 'set-line kind-toggle has-toggle' + (editable ? ' is-editable' : ' is-readonly');
    const line = api.el(editable ? 'button' : 'div', baseCls);
    if (editable) {
      line.type = 'button';
      line.setAttribute('role', 'switch');
      line.setAttribute('aria-checked', row.on === true ? 'true' : 'false');
    }
    if (glyph) line.appendChild(glyphBox(api, 'set-icon tone-' + tone, glyph, heading));
    const textBox = lineText(api, heading, detail);
    line.appendChild(textBox);

    let state = row.on === true;
    const sw = api.el('span', 'set-switch' + (state ? ' on' : ''));
    if (!editable) {
      sw.setAttribute('role', 'switch');
      sw.setAttribute('aria-checked', state ? 'true' : 'false');
      sw.setAttribute('aria-disabled', 'true');
    } else {
      sw.setAttribute('aria-hidden', 'true');
    }
    sw.appendChild(api.el('span', 'set-knob'));
    const sr = api.el('span', 'set-sr', state ? 'on' : 'off');
    sw.appendChild(sr);
    line.appendChild(sw);
    if (value) line.appendChild(api.el('span', 'set-value', value));

    if (editable) {
      let busy = false;
      const paint = (on) => {
        state = on;
        row.on = on;
        sw.classList.toggle('on', on);
        line.setAttribute('aria-checked', on ? 'true' : 'false');
        sr.textContent = on ? 'on' : 'off';
      };
      line.addEventListener('click', async () => {
        if (busy) return;
        const prev = state;
        const next = !prev;
        busy = true;
        showInlineError(textBox, api, '');
        paint(next);
        try {
          await api.rpc('set_setting', { path: settingPath, value: next });
          if (ctx.onSaved) ctx.onSaved();
        } catch (err) {
          paint(prev);
          const msg = (err && err.message) || 'Could not save setting';
          showInlineError(textBox, api, msg);
          api.toast(msg);
        } finally {
          busy = false;
        }
      });
    }
    return line;
  }

  const isChoice = Boolean(settingPath && Array.isArray(row.options) && row.options.length);
  const isNumber = Boolean(settingPath && row.input === 'number');
  const editable = isChoice || isNumber;
  const classNames = 'set-line kind-' + kind + (editable ? ' is-editable' : '');
  // 行级 tone 只在行内没有独立着色元素时生效，否则会盖掉 badge 的弱化色。
  const fullCls = tone !== 'default' && !row.badge ? classNames + ' tone-' + tone : classNames;
  const line = api.el(editable ? 'button' : 'div', fullCls);
  if (editable) line.type = 'button';

  if (glyph) line.appendChild(glyphBox(api, 'set-icon tone-' + tone, glyph, heading));
  const textBox = lineText(api, heading, detail);
  line.appendChild(textBox);
  if (row.badge) line.appendChild(api.el('span', 'set-badge tone-' + tone, text(row.badge)));
  const valNode = api.el('span', 'set-value tone-' + tone, value);
  if (value || editable) line.appendChild(valNode);
  if (editable) line.appendChild(ctx.icon('chevron', 'set-chev'));

  if (editable) {
    let busy = false;
    const commit = async (nextRaw, nextLabel) => {
      if (busy) return;
      const prevRaw = row.current;
      const prevLabel = valNode.textContent;
      busy = true;
      showInlineError(textBox, api, '');
      row.current = nextRaw;
      row.value = nextLabel;
      valNode.textContent = nextLabel;
      try {
        await api.rpc('set_setting', { path: settingPath, value: nextRaw });
        if (ctx.onSaved) ctx.onSaved();
      } catch (err) {
        row.current = prevRaw;
        row.value = prevLabel;
        valNode.textContent = prevLabel;
        const msg = (err && err.message) || 'Could not save setting';
        showInlineError(textBox, api, msg);
        api.toast(msg);
      } finally {
        busy = false;
      }
    };
    line.addEventListener('click', () => {
      if (busy) return;
      if (isChoice) {
        openChoiceSheet(api, row, heading, detail, (v, l) => commit(v, l));
      } else if (isNumber) {
        openNumberSheet(api, row, heading, detail, (n) => {
          const suffix = typeof row.unit === 'string' && row.unit ? ' ' + row.unit : '';
          commit(n, String(n) + suffix);
        });
      }
    });
  }
  return line;
}
