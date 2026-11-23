// Settings → Appearance on the phone: colour mode (System / Light / Dark) and theme. Local to this device;
// theme.js owns the stored pref and applies it.

const MODES = [
  { id: 'system', label: 'System' },
  { id: 'light', label: 'Light' },
  { id: 'dark', label: 'Dark' },
];

function phone(scheme, theme) {
  const box = document.createElement('span');
  box.className = 'ap-phone';
  const paint = (node, values) => {
    node.style.setProperty('--p-bg', values[0]);
    node.style.setProperty('--p-card', values[1]);
    node.style.setProperty('--p-fg', values[4]);
    node.style.setProperty('--p-acc', values[6]);
  };
  if (scheme === 'system') {
    const half = document.createElement('span');
    half.className = 'ap-phone-half';
    paint(box, theme.light);
    paint(half, theme.dark);
    box.appendChild(half);
  } else {
    paint(box, theme[scheme]);
  }
  for (let i = 0; i < 3; i += 1) box.appendChild(document.createElement('i'));
  return box;
}

export function appearanceBody(api) {
  const engine = window.HarnessTheme;
  const wrap = api.el('div', 'ap');
  if (!engine) {
    wrap.appendChild(api.el('p', 'set-note', 'Themes are not available in this browser.'));
    return wrap;
  }

  const paint = () => {
    const pref = engine.get();
    const theme = engine.themes.find((t) => t.id === pref.theme) || engine.themes[0];
    wrap.textContent = '';

    const modeSec = api.el('section', 'set-section');
    modeSec.appendChild(api.el('p', 'set-caps', 'Color scheme'));
    const modes = api.el('div', 'ap-modes');
    for (const mode of MODES) {
      const on = pref.scheme === mode.id;
      const btn = api.el('button', 'ap-mode' + (on ? ' on' : ''));
      btn.type = 'button';
      btn.setAttribute('aria-pressed', on ? 'true' : 'false');
      btn.appendChild(phone(mode.id, theme));
      btn.appendChild(api.el('span', 'ap-mode-t', mode.label));
      btn.addEventListener('click', () => {
        engine.set({ scheme: mode.id });
        paint();
      });
      modes.appendChild(btn);
    }
    modeSec.appendChild(modes);
    wrap.appendChild(modeSec);

    const activeScheme = document.documentElement.getAttribute('data-scheme') || 'dark';
    const themeSec = api.el('section', 'set-section');
    themeSec.appendChild(api.el('p', 'set-caps', 'Themes'));
    const grid = api.el('div', 'ap-themes');
    for (const item of engine.themes) {
      const on = item.id === theme.id;
      const card = api.el('button', 'ap-theme' + (on ? ' on' : ''));
      card.type = 'button';
      card.setAttribute('aria-pressed', on ? 'true' : 'false');
      const orbs = api.el('span', 'ap-orbs');
      for (const pair of [
        { key: 'light', values: item.light },
        { key: 'dark', values: item.dark },
      ]) {
        const orb = api.el('span', 'ap-orb' + (on && activeScheme === pair.key ? ' is-cur' : ''));
        orb.style.background =
          'radial-gradient(circle at 32% 28%, ' +
          pair.values[6] +
          ' 0 22%, ' +
          pair.values[2] +
          ' 32% 62%, ' +
          pair.values[0] +
          ' 72%)';
        orbs.appendChild(orb);
      }
      card.appendChild(orbs);
      card.appendChild(api.el('span', 'ap-theme-t', item.name));
      card.addEventListener('click', () => {
        engine.set({ theme: item.id });
        paint();
      });
      grid.appendChild(card);
    }
    themeSec.appendChild(grid);
    wrap.appendChild(themeSec);
    wrap.appendChild(api.el('p', 'set-note', 'Saved on this phone only. The desktop keeps its own theme.'));
  };
  paint();
  return wrap;
}
