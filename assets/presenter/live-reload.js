// Client of `presenter serve` (docs/spec/live-reload.md §3); ES2020, no deps.
// Notes: docs/dev/live-reload-qa.md; debug: window.__presenterLiveReload.
(() => {
'use strict';
// Per frame: the speaker view has two deck frames.
const KEY = 'presenter:build_id:' + location.pathname + location.search;
const state = { buildId: null, connected: false, retryMs: 500, lastStatus: 'ok', errorId: null };
let source = null;
let timer = null;
let queue = Promise.resolve();
const log = (...a) => console.debug('[presenter]', ...a);

// What a swap cannot update, sans ?v= (scripts add head elements after the theme).
const signature = (doc) => Array.from(doc.querySelectorAll('head>*,body>*'))
  .filter((el) => !el.matches('title,meta,#presenter-overlay,#presenter-theme~*,.reveal') &&
    !el.querySelector('.reveal'))
  .map((el) => el.outerHTML).join('\n').replace(/\?v=\d+/g, '') + doc.documentElement.lang;
// Once parsed: deck.html's body_end follows this script. '' reloads.
let assets = '';
const snap = () => { assets = signature(document); };
if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', snap);
else snap();

function store(id) {
  state.buildId = id;
  try { sessionStorage.setItem(KEY, id); } catch { /* storage disabled */ }
}

// Stored first: the reloaded page must not reload.
function fullReload(why, id) {
  store(id);
  disconnect();
  log('full reload:', why);
  location.reload(); // reveal `hash: true` restores the slide
  return false;
}

const overlay = () => document.getElementById('presenter-overlay');
function showOverlay(err, id) {
  const el = overlay() || document.body.appendChild(document.createElement('div'));
  el.id = 'presenter-overlay';
  el.style.cssText = 'position:fixed;top:1em;right:1em;max-width:50%;max-height:80vh;overflow:auto;' +
    'z-index:10000;background:#222;color:#eee;border-left:4px solid #e55;padding:1em;' +
    'font:13px monospace;white-space:pre-wrap;cursor:pointer';
  el.onclick = () => { el.style.display = 'none'; };
  el.textContent = ['build error' + (err.kind ? ' (' + err.kind + ')' : ''), err.message || 'unknown error',
    err.file && err.file + (err.line ? ':' + err.line : ''), '(click to dismiss)'].filter(Boolean).join('\n');
  state.errorId = id; // a reconnect leaves this one alone
}
function hideOverlay() {
  const el = overlay();
  if (el) el.style.display = 'none';
}

// Adds a <link> to `href`, drops `old` once it loads (no flash); false on failure.
const swapLink = (old, href) => new Promise((resolve) => {
  const link = old.cloneNode();
  link.href = href;
  link.onload = link.onerror = (e) => {
    const ok = e.type === 'load';
    (ok ? old : link).remove();
    if (!ok) log('stylesheet failed:', href);
    resolve(ok);
  };
  old.after(link);
});

async function swapCss(id) {
  const theme = document.querySelector('link#presenter-theme');
  if (!theme) return fullReload('no #presenter-theme link', id);
  const sel = 'link[rel~=stylesheet][href^="/__presenter/asset/"]:not([href^="/__presenter/asset/vendor"])';
  const ok = await Promise.all([swapLink(theme, '/__presenter/theme.css?v=' + id), ...Array.from(
    document.querySelectorAll(sel), (l) => swapLink(l, l.getAttribute('href').replace(/\?.*/, '') + '?v=' + id))]);
  if (window.Reveal?.isReady()) Reveal.layout();
  log('styles reloaded, build', id);
  return ok.every(Boolean);
}

// The old slide by id (as a reload would), else the old indices clamped to the new counts.
function restore(R, s, id) {
  const el = id && document.getElementById(id);
  let h, v, f;
  if (el && el.matches('.slides section')) {
    ({ h, v = 0 } = R.getIndices(el));
    f = s.indexf;
  } else {
    const hs = R.getHorizontalSlides();
    h = Math.min(s.indexh, hs.length - 1);
    v = Math.min(s.indexv || 0, Math.max(0, hs[h].querySelectorAll('section').length - 1));
    f = h === s.indexh && v === s.indexv ? s.indexf : undefined;
  }
  try {
    R.setState({ ...s, indexh: h, indexv: v, indexf: f });
  } catch {
    R.slide(h, v, f);
  }
}

async function swapContent(id) {
  const html = await fetch('/', { cache: 'no-store' }).then((r) => (r.ok ? r.text() : null), () => null);
  if (html === null) {
    state.lastStatus = 'stale';
    log('GET / failed, update postponed');
    return false;
  }
  const next = new DOMParser().parseFromString(html, 'text/html');
  const fresh = next.querySelector('.reveal .slides');
  const slides = document.querySelector('.reveal .slides');
  const R = window.Reveal;
  if (!fresh || !slides || !R?.isReady() || R.isScrollView() || R.isPrintView()) {
    return fullReload('cannot update in place', id);
  }
  const sig = signature(next); // scripts in the slides would not run
  if (sig !== assets || fresh.querySelector('script')) {
    return fullReload('page changed outside the slides', id);
  }
  const before = R.getState();
  const current = R.getCurrentSlide()?.id;
  try {
    slides.replaceChildren(...fresh.childNodes); // reveal holds on to .slides itself
    R.removeHiddenSlides();
    if (window.katex) window.__presenterRenderMath?.(slides);
    R.getPlugin?.('highlight')?.init?.(R); // its on-load pass; every `pre code` is new
    R.sync();
    restore(R, before, current);
  } catch (e) {
    return fullReload('in-place update failed: ' + e, id);
  }
  if (next.title) document.title = next.title;
  assets = sig;
  log('slides updated, build', id);
  return true;
}

async function apply(d) {
  if (d.status === 'error') {
    state.lastStatus = 'error';
    showOverlay(d.error || {}, d.build_id);
  } else {
    // After an error or a postponed update, apply everything.
    if ((d.kind !== 'css' || state.lastStatus !== 'ok') && !(await swapContent(d.build_id))) return;
    if (!(await swapCss(d.build_id))) return; // content builds can change it too
    state.lastStatus = 'ok';
    hideOverlay();
  }
  store(d.build_id); // only once applied: a hello after a failed update reloads
}

function onHello(d) {
  const again = state.buildId !== null; // a reconnect
  let prev = state.buildId;
  try { prev = prev ?? sessionStorage.getItem(KEY); } catch { /* storage disabled */ }
  state.retryMs = 500;
  state.connected = true;
  if (prev !== null && +prev !== d.build_id) {
    return fullReload('build ' + prev + ' -> ' + d.build_id, d.build_id);
  }
  store(d.build_id);
  log('connected, build', d.build_id);
  if (d.status === 'error' && state.errorId !== d.build_id) {
    state.lastStatus = 'error';
    state.errorId = d.build_id;
    // §2's hello has no details: keep the overlay of the server's fallback page.
    if (d.error || !overlay()) showOverlay(d.error || { message: 'details in the terminal' }, d.build_id);
  } else if (again && d.status !== 'error') {
    onBuild({ ...d, kind: 'content' }); // same id: the server may have restarted
  }
}

function onBuild(d) {
  log('build', d.build_id, d.kind ? d.status + '/' + d.kind : d.status);
  queue = queue.then(() => apply(d)).catch((e) => fullReload('update failed: ' + e, d.build_id));
}

// Never throws out of an event handler.
const handler = (fn) => (ev) => {
  try {
    const d = JSON.parse(ev.data);
    if (typeof d?.build_id === 'number') fn(d);
  } catch (e) {
    log('bad event', e);
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
