// `presenter serve` live-reload client (docs/spec/live-reload.md §3). ES2020, no deps.
// Debugging: window.__presenterLiveReload = { connect, disconnect, state }.
(() => {
'use strict';
const KEY = 'presenter:build_id';
const state = { buildId: null, connected: false, retryMs: 500, lastStatus: null, swaps: { css: 0, content: 0 } };
let source = null;
let timer = null;
let queue = Promise.resolve();
const log = (...a) => console.debug('[presenter]', ...a);
// Stylesheets and scripts, ?v= ignored: a document with other ones needs a reload.
const signature = (doc) => Array.from(doc.querySelectorAll('link[rel~=stylesheet],script'), (el) =>
  (el.getAttribute('href') || el.getAttribute('src') || el.textContent).replace(/\?v=\d+$/, '')).join('\n');
let assets = signature(document);

function store(id) {
  state.buildId = id;
  try { sessionStorage.setItem(KEY, String(id)); } catch { /* storage disabled */ }
}

// Stores `id` (the build causing it) so the next hello does not reload again.
function fullReload(why, id) {
  if (id !== undefined) store(id);
  log('full reload:', why);
  location.reload(); // reveal `hash: true` restores the slide
  return false;
}

function showOverlay(err) {
  let el = document.getElementById('presenter-overlay');
  if (!el) (el = document.body.appendChild(document.createElement('div'))).id = 'presenter-overlay';
  el.style.cssText = 'position:fixed;top:12px;right:12px;max-width:50%;max-height:80vh;overflow:auto;' +
    'z-index:10000;background:#222;color:#eee;border-left:4px solid #e55;padding:12px;' +
    'font:13px monospace;white-space:pre-wrap;cursor:pointer';
  el.onclick = () => { el.style.display = 'none'; };
  const where = err.file ? err.file + (err.line ? ':' + err.line : '') : '';
  el.textContent = ['build error' + (err.kind ? ' (' + err.kind + ')' : ''), err.message || 'unknown error', where,
    '(click to dismiss)'].filter(Boolean).join('\n');
}
function hideOverlay() {
  const el = document.getElementById('presenter-overlay');
  if (el) el.style.display = 'none';
}

// Loads `href` in a new <link> after `old`, then drops `old` (no flash); false on failure.
const swapLink = (old, href) => new Promise((resolve) => {
  const link = old.cloneNode();
  link.removeAttribute('id');
  link.setAttribute('href', href);
  link.onload = link.onerror = (e) => {
    const ok = e.type === 'load';
    (ok ? old : link).remove();
    if (ok && old.id) link.id = old.id;
    if (!ok) log('stylesheet failed:', href);
    resolve(ok);
  };
  old.after(link);
});

// `all`: also re-request /__presenter/asset/ theme stylesheets.
async function swapCss(id, all) {
  const theme = document.getElementById('presenter-theme');
  if (!theme || theme.tagName !== 'LINK') return fullReload('no #presenter-theme link', id);
  const sel = 'link[rel~=stylesheet][href^="/__presenter/asset/"][href*=theme]';
  const extra = all ? document.querySelectorAll(sel) : [];
  const ok = await Promise.all([swapLink(theme, '/__presenter/theme.css?v=' + id),
    ...Array.from(extra, (l) => swapLink(l, l.getAttribute('href').replace(/\?.*/, '') + '?v=' + id))]);
  if (window.Reveal && Reveal.isReady()) Reveal.layout();
  state.swaps.css++;
  log('styles reloaded, build', id);
  return ok.every(Boolean);
}

// Mirrors the highlight plugin's init: wrapper class, data-trim, highlightBlock.
function rehighlight(R, root) {
  const hl = R.getPlugin && R.getPlugin('highlight');
  if (!hl || !hl.highlightBlock || (R.getConfig().highlight || {}).highlightOnLoad === false) return;
  root.querySelectorAll('pre code').forEach((code) => {
    code.parentNode.classList.add('code-wrapper');
    if (code.hasAttribute('data-trim')) {
      const lines = code.innerHTML.replace(/^\s*\n|\n\s*$/g, '').split('\n');
      const pad = Math.min(...lines.filter((l) => l.trim()).map((l) => l.length - l.trimStart().length));
      code.innerHTML = lines.map((l) => l.slice(pad)).join('\n');
    }
    hl.highlightBlock(code);
  });
}

// Restores the state, indices clamped to the new slide counts.
function restore(R, s) {
  const hs = R.getHorizontalSlides();
  const h = Math.max(0, Math.min(s.indexh || 0, hs.length - 1));
  const v = Math.max(0, Math.min(s.indexv || 0, (hs[h] ? hs[h].querySelectorAll('section').length : 0) - 1));
  const f = h === s.indexh && v === s.indexv ? s.indexf : undefined;
  try {
    R.setState({ ...s, indexh: h, indexv: v, indexf: f });
  } catch (e) {
    log('setState failed', e);
    R.slide(h, v, f);
  }
}

async function swapContent(id) {
  const html = await fetch('/', { cache: 'no-store' }).then((r) => (r.ok ? r.text() : null), () => null);
  if (html === null) {
    log('GET / failed, update skipped');
    return false;
  }
  const next = new DOMParser().parseFromString(html, 'text/html');
  const fresh = next.querySelector('.reveal .slides');
  const slides = document.querySelector('.reveal .slides');
  const R = window.Reveal;
  if (!fresh || !slides || !R || !R.isReady()) return fullReload('cannot update in place', id);
  const sig = signature(next); // scripts swapped into slides would not run
  if (sig !== assets || fresh.querySelector('script')) return fullReload('new stylesheets or scripts', id);
  const before = R.getState();
  try {
    slides.replaceChildren(...fresh.childNodes); // keep the element reveal holds on to
    R.removeHiddenSlides(); // as reveal does on init
    if (window.katex && window.__presenterRenderMath) window.__presenterRenderMath(slides);
    rehighlight(R, slides);
    R.sync();
    restore(R, before);
  } catch (e) {
    return fullReload('in-place update failed: ' + e, id);
  }
  if (next.title && next.title !== document.title) document.title = next.title;
  assets = sig;
  state.swaps.content++;
  log('slides updated, build', id);
  return true;
}

async function apply(d) {
  if (d.status === 'error') {
    state.lastStatus = 'error';
    showOverlay(d.error || {});
  } else {
    // A good build after a failed one may hold any earlier change: apply both.
    const css = d.kind === 'css';
    const afterError = state.lastStatus === 'error';
    if ((!css || afterError) && !(await swapContent(d.build_id))) return;
    // Content builds can change the theme stylesheet too (theme.toml, presenter.toml).
    if (!(await swapCss(d.build_id, css || afterError))) return;
    state.lastStatus = 'ok';
    hideOverlay();
  }
  store(d.build_id); // only once applied: a hello after a failed update reloads
}

function onHello(d) {
  state.retryMs = 500;
  state.connected = true;
  let prev = null;
  try { prev = sessionStorage.getItem(KEY); } catch { /* storage disabled */ }
  store(d.build_id);
  if (prev !== null && +prev !== d.build_id) return fullReload('build ' + prev + ' -> ' + d.build_id);
  log('connected, build', d.build_id);
  if (d.status === 'error') onBuild(d);
}

function onBuild(d) {
  log('build', d.build_id, d.kind ? d.status + '/' + d.kind : d.status);
  queue = queue.then(() => apply(d)).catch((e) => fullReload('update failed: ' + e, d.build_id));
}

// Parses event data; never throws out of an event handler.
const handler = (fn) => (ev) => {
  try {
    const d = JSON.parse(ev.data);
    if (d && typeof d.build_id === 'number') fn(d);
    else log('ignoring event:', ev.data);
  } catch (e) {
    log('event handler failed', e);
  }
};

function disconnect() {
  clearTimeout(timer);
  if (source) source.close();
  source = timer = null;
  state.connected = false;
}

function connect() {
  disconnect();
  const es = (source = new EventSource('/__presenter/events'));
  es.addEventListener('hello', handler(onHello));
  es.addEventListener('build', handler(onBuild));
  es.addEventListener('error', () => {
    if (source !== es) return;
    disconnect();
    const ms = state.retryMs;
    state.retryMs = Math.min(ms * 2, 5000);
    log('connection lost, retrying in', ms, 'ms');
    timer = setTimeout(connect, ms);
  });
}

window.__presenterLiveReload = { connect, disconnect, state };
connect();
})();
