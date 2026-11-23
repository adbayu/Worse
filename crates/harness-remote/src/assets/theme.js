/* Phone themes: solid surfaces + one accent. Runs as a classic script in <head> so the first paint already
   has the chosen colours. Pref lives in localStorage; `window.HarnessTheme` lets Settings → Appearance change it.
   theme.css derives every other token (panes, rims, md-*) from the base set written here. */
(function () {
  var KEY = 'harness.theme';
  // [bg, card, raised, fg, fg2, line, accent, onAccent] per scheme.
  var THEMES = [
    {
      id: 'graphite',
      name: 'Graphite',
      dark: ['#09090a', '#18181a', '#242428', '#f5f5f7', '#8e8e93', 'rgba(255,255,255,0.075)', '#4d7cfe', '#ffffff'],
      light: ['#f5f5f7', '#ffffff', '#ebebee', '#121214', '#6e6e73', 'rgba(0,0,0,0.075)', '#2f63f0', '#ffffff'],
    },
    {
      id: 'iris',
      name: 'Iris',
      dark: ['#15111e', '#231d32', '#302843', '#f4f1fa', '#9e96b2', 'rgba(255,255,255,0.075)', '#ee7cc8', '#26091e'],
      light: ['#f8f5fb', '#ffffff', '#eee8f6', '#1a1524', '#675e7a', 'rgba(30,20,50,0.08)', '#bf3292', '#ffffff'],
    },
    {
      id: 'ocean',
      name: 'Ocean',
      dark: ['#091016', '#131f29', '#1c2c3a', '#eef5fa', '#879cad', 'rgba(255,255,255,0.075)', '#47b2f5', '#041c2c'],
      light: ['#f2f6fa', '#ffffff', '#e6eef5', '#0e1b24', '#586c7c', 'rgba(15,40,60,0.08)', '#0275bd', '#ffffff'],
    },
    {
      id: 'ember',
      name: 'Ember',
      dark: ['#120e0b', '#1e1814', '#2a221c', '#f7f0eb', '#a6968c', 'rgba(255,255,255,0.075)', '#f4863d', '#261002'],
      light: ['#faf5f1', '#ffffff', '#f1e9e2', '#20160f', '#706056', 'rgba(60,35,20,0.08)', '#c6490b', '#ffffff'],
    },
    {
      id: 'grove',
      name: 'Grove',
      dark: ['#0b110d', '#151e18', '#1f2b23', '#edf5ef', '#8ea394', 'rgba(255,255,255,0.075)', '#54ce84', '#06220f'],
      light: ['#f3f7f4', '#ffffff', '#e7f0e9', '#111e16', '#586a5e', 'rgba(20,50,30,0.08)', '#15803d', '#ffffff'],
    },
  ];
  var NAMES = ['--bg', '--bg2', '--bg3', '--fg', '--fg2', '--line', '--acc', '--acc-fg'];
  var media = window.matchMedia ? window.matchMedia('(prefers-color-scheme: light)') : null;

  function read() {
    var pref = { theme: 'graphite', scheme: 'system' };
    try {
      var saved = JSON.parse(localStorage.getItem(KEY) || '{}');
      if (saved && typeof saved.theme === 'string') pref.theme = saved.theme;
      if (saved && /^(system|light|dark)$/.test(saved.scheme)) pref.scheme = saved.scheme;
    } catch (e) {
      /* 读不到就用默认主题 */
    }
    return pref;
  }

  function find(id) {
    for (var i = 0; i < THEMES.length; i++) if (THEMES[i].id === id) return THEMES[i];
    return THEMES[0];
  }

  function apply() {
    var pref = read();
    var scheme = pref.scheme === 'system' ? (media && media.matches ? 'light' : 'dark') : pref.scheme;
    var theme = find(pref.theme);
    var values = theme[scheme];
    var root = document.documentElement;
    root.setAttribute('data-theme', theme.id);
    root.setAttribute('data-scheme', scheme);
    for (var i = 0; i < NAMES.length; i++) root.style.setProperty(NAMES[i], values[i]);
    var meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute('content', values[0]);
  }

  function set(change) {
    var pref = read();
    if (change && change.theme) pref.theme = find(change.theme).id;
    if (change && /^(system|light|dark)$/.test(change.scheme)) pref.scheme = change.scheme;
    try {
      localStorage.setItem(KEY, JSON.stringify(pref));
    } catch (e) {
      /* 存不了也照样切换本次显示 */
    }
    apply();
    return pref;
  }

  if (media && media.addEventListener) media.addEventListener('change', apply);
  apply();
  window.HarnessTheme = { themes: THEMES, get: read, set: set, apply: apply };
})();
