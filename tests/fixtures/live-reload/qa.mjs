#!/usr/bin/env node
// Scripted run of the live-reload QA checklist (docs/dev/live-reload-qa.md) against
// mock-server.py: drives headless Chrome over the DevTools protocol, runs each scenario in the
// page and prints one PASS/FAIL line per scenario; exit code 1 if any fails.
//
// usage: node tests/fixtures/live-reload/qa.mjs [--only ABCDEGHJRSF] [--chrome PATH] [--out DIR]
//   --only   sessions to run (default: all); F stops and restarts the mock process (~40 s)
//   --chrome Chrome or Chromium binary (default: $CHROME_PATH, else the usual install path)
//   --out    directory for a log and a final screenshot per session
// Needs Node >= 22 (global fetch and WebSocket), python3 and Chrome; no network beyond
// 127.0.0.1. Temporary Chrome profiles go to os.tmpdir() ($TMPDIR). The whole run takes
// about 2.5 minutes. It is not part of `cargo test`.
import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : fallback;
};
const ONLY = opt('--only', 'ABCDEGHJRSF');
const CHROME = opt('--chrome', process.env.CHROME_PATH ||
  (process.platform === 'darwin' ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome'));
const OUT = opt('--out', null);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------------------
// Page-side code. Each function is serialized with toString() and evaluated in the page, so
// it may use only page globals (window.qa from installHelpers, Reveal, document, ...).
// A function that returns { item, pass, ... } is a scenario result.
// ---------------------------------------------------------------------------------------

function installHelpers() {
  window.qa = {
    wait: (ms) => new Promise((r) => setTimeout(r, ms)),
    async until(fn, ms = 6000) {
      const end = Date.now() + ms;
      while (Date.now() < end) {
        try { if (fn()) return true; } catch { /* not yet */ }
        await qa.wait(50);
      }
      return false;
    },
    get: (p) => fetch(p).then((r) => r.text()),
    post: (p, body) => fetch(p, { method: 'POST', body }).then((r) => r.text()),
    async mock() { return JSON.parse(await qa.get('/__mock/log')); },
    slidesHtml: () => qa.get('/__mock/slides'),
    lr: () => window.__presenterLiveReload.state,
    // Edits the served slides with fn(html), emits a build of `kind`, then waits until the
    // client has applied (stored) that build and cond() holds.
    async edit(fn, kind, cond) {
      if (fn) await qa.post('/__mock/slides', fn(await qa.slidesHtml()));
      const ev = JSON.parse(await qa.get('/__mock/emit?kind=' + kind));
      const ok = await qa.until(() => qa.lr().buildId === ev.build_id && (!cond || cond()));
      await qa.wait(300);
      return { ev, ok, snap: qa.snap(), requests: await qa.mock() };
    },
    key: () => 'presenter:build_id:' + location.pathname + location.search,
    ov() {
      const o = document.getElementById('presenter-overlay');
      if (!o) return null;
      const cs = getComputedStyle(o), r = o.getBoundingClientRect();
      return { shown: cs.display !== 'none', position: cs.position, z: cs.zIndex, font: cs.fontFamily,
        width: Math.round(r.width), right: Math.round(innerWidth - r.right), top: Math.round(r.top),
        text: o.textContent, count: document.querySelectorAll('#presenter-overlay').length };
    },
    nav: () => performance.getEntriesByType('navigation')[0].type,
    snap() {
      const lr = window.__presenterLiveReload;
      return {
        idx: window.Reveal ? Reveal.getIndices() : null,
        marker: window.__qaMarker || null,
        hash: location.hash,
        title: document.title,
        theme: Array.from(document.querySelectorAll('#presenter-theme'), (l) => l.getAttribute('href')),
        sheets: Array.from(document.querySelectorAll('link[rel=stylesheet]'), (l) => l.getAttribute('href')),
        overlay: qa.ov(),
        lr: lr && JSON.parse(JSON.stringify(lr.state)),
        stored: sessionStorage.getItem(qa.key()),
        total: window.Reveal ? Reveal.getTotalSlides() : null,
        present: window.Reveal ? (Reveal.getCurrentSlide() || {}).id : null,
      };
    },
    // A content edit of #stack-bottom (the current slide); in place iff the marker survives
    // and the slide's element is a new one (tagged before the build).
    async editInPlace(label) {
      window.__qaMarker = label; await qa.mock();
      document.getElementById('stack-bottom').__qaOld = true;
      const text = 'bottom ' + label;
      const r = await qa.edit((h) => h.replace(/<p>bottom[^<]*<\/p>/, '<p>' + text + '</p>'), 'content',
        () => document.querySelector('#stack-bottom p').textContent === text);
      const s = r.snap;
      return { ok: r.ok && s.marker === label && !document.getElementById('stack-bottom').__qaOld
        && s.idx.h === 1 && s.idx.v === 1 && r.requests.includes('GET / [empty]'), idx: s.idx, requests: r.requests };
    },
    // A reconnect (same build id): in place iff the marker survives and the slides were swapped.
    async reconnectInPlace(label) {
      window.__qaMarker = label;
      document.getElementById('stack-bottom').__qaOld = true;
      await qa.mock();
      await qa.get('/__mock/down?ms=300');
      const reconnected = await qa.until(() => !qa.lr().connected) && await qa.until(() => qa.lr().connected);
      const swapped = await qa.until(() => !document.getElementById('stack-bottom').__qaOld);
      await qa.wait(500);
      const s = qa.snap(), req = await qa.mock();
      return { ok: reconnected && swapped && s.marker === label && s.idx.h === 1 && s.idx.v === 1
        && req.includes('GET / [empty]'), idx: s.idx, requests: req };
    },
  };
  return 'qa helpers installed';
}

// --- A: swaps (items 1-6, 11-16) ---------------------------------------------------------

async function a1Css() {
  window.__qaMarker = 'A'; await qa.mock();
  await qa.post('/__mock/css', '.reveal h2 { color: rgb(255, 0, 0); }');
  await qa.post('/__mock/asset?path=extra.css', '.reveal p { color: rgb(0, 128, 0); }');
  await qa.post('/__mock/asset?path=' + encodeURIComponent('@corporate/styles/hl.css'), '.reveal h1 { color: rgb(1, 2, 3); }');
  const col = (sel) => getComputedStyle(document.querySelector(sel)).color;
  const r = await qa.edit(null, 'css', () => col('#stack-bottom h2') === 'rgb(255, 0, 0)' &&
    col('#stack-bottom p') === 'rgb(0, 128, 0)' && col('#intro h1') === 'rgb(1, 2, 3)');
  const s = r.snap, v = '?v=' + r.ev.build_id;
  const want = ['GET /__presenter/asset/@corporate/styles/hl.css' + v + ' [style]',
    'GET /__presenter/asset/extra.css' + v + ' [style]', 'GET /__presenter/theme.css' + v + ' [style]'];
  const pass = r.ok && s.idx.h === 1 && s.idx.v === 1 && s.marker === 'A' && s.present === 'stack-bottom'
    && JSON.stringify(s.theme) === JSON.stringify(['/__presenter/theme.css' + v])
    && s.sheets.includes('/__presenter/asset/extra.css' + v) && s.sheets.includes('/__presenter/asset/@corporate/styles/hl.css' + v)
    && s.sheets.includes('/__presenter/asset/vendor/reveal/dist/theme/white.css')
    && JSON.stringify(r.requests.slice().sort()) === JSON.stringify(want);
  return { item: 'A1 [1,2] css build: theme.css and theme-chain/@theme stylesheets get ?v=<id>, vendor CSS untouched, no reload, slide kept', pass, idx: s.idx, sheets: s.sheets, requests: r.requests };
}

async function a2Text() {
  const r = await qa.edit((h) => h.replace('<p>bottom</p>', '<p>bottom EDITED</p>'), 'content',
    () => document.querySelector('#stack-bottom p').textContent === 'bottom EDITED');
  const s = r.snap, v = '?v=' + r.ev.build_id;
  const want = ['GET / [empty]', 'GET /__presenter/asset/@corporate/styles/hl.css' + v + ' [style]',
    'GET /__presenter/asset/extra.css' + v + ' [style]', 'GET /__presenter/theme.css' + v + ' [style]'];
  const pass = r.ok && s.idx.h === 1 && s.idx.v === 1 && s.marker === 'A' && s.present === 'stack-bottom'
    && document.querySelector('#stack-bottom').classList.contains('present')
    && JSON.stringify(r.requests.slice().sort()) === JSON.stringify(want);
  return { item: 'A2 [3] edit slide text: in-place swap, same slide; only GET / and the non-vendor stylesheets requested', pass, idx: s.idx, requests: r.requests };
}

async function a3Add() {
  const before = Reveal.getTotalSlides();
  const r = await qa.edit((h) => h + '<section id="added"><h2>Added</h2></section>', 'content',
    () => Reveal.getTotalSlides() === before + 1);
  const s = r.snap;
  Reveal.slide(5); await qa.wait(300);
  const nav = Reveal.getCurrentSlide().id;
  Reveal.slide(1, 1); await qa.wait(300);
  const pass = r.ok && s.idx.h === 1 && s.idx.v === 1 && s.present === 'stack-bottom' && s.marker === 'A' && nav === 'added';
  return { item: 'A3 [4] append a slide: it exists and can be reached; current slide kept', pass, before, after: s.total, idx: s.idx, nav };
}

async function a3bInsert() {
  Reveal.slide(3); await qa.wait(1200); // #math; give reveal time to write the hash
  const hashBefore = location.hash;
  const r = await qa.edit((h) => h.replace('<section id="intro">', '<section id="new-first"><h2>New first</h2></section><section id="intro">'),
    'content', () => !!document.getElementById('new-first'));
  await qa.wait(1200);
  const s = qa.snap();
  const pass = r.ok && hashBefore === '#/math' && s.present === 'math' && s.idx.h === 4 && location.hash === '#/math' && s.marker === 'A';
  return { item: 'A3b [5] insert a slide BEFORE the current one (with id): same slide shown, hash unchanged', pass, hashBefore, idx: s.idx, present: s.present, hash: location.hash };
}

async function a3cInsertNoId() {
  // Without ids the position is kept, as a full reload of `#/h` would do.
  await qa.edit((h) => h.replace('<section id="new-first"><h2>New first</h2></section>', '').replace('<section id="math">', '<section class="math-noid">'),
    'content', () => !document.getElementById('new-first'));
  Reveal.slide(3); await qa.wait(300);
  const at = (Reveal.getCurrentSlide().className || '').includes('math-noid');
  const r = await qa.edit((h) => h.replace('<section id="intro">', '<section id="new-first"><h2>New first</h2></section><section id="intro">'),
    'content', () => !!document.getElementById('new-first'));
  const s = r.snap, cur = Reveal.getCurrentSlide();
  const pass = r.ok && at && s.idx.h === 3 && cur.id === 'code' && s.marker === 'A';
  await qa.edit((h) => h.replace('<section id="new-first"><h2>New first</h2></section>', '').replace('<section class="math-noid">', '<section id="math">'),
    'content', () => !document.getElementById('new-first'));
  return { item: 'A3c [5] insert before a current slide WITHOUT id: position kept (documented; matches a reload)', pass, idx: s.idx, nowShowing: cur.id };
}

async function a4Remove() {
  Reveal.slide(5); await qa.wait(1200);
  const r = await qa.edit((h) => h.replace('<section id="added"><h2>Added</h2></section>', ''), 'content', () => !document.getElementById('added'));
  await qa.wait(1200);
  const s = qa.snap();
  const pass = r.ok && s.idx.h === 4 && s.present === 'last' && s.marker === 'A' && location.hash === '#/last';
  return { item: 'A4 [6] remove the current (last) slide: clamped to the new last slide', pass, idx: s.idx, present: s.present, hash: location.hash };
}

async function a5RemoveV() {
  Reveal.slide(1, 1); await qa.wait(300);
  const r = await qa.edit((h) => h.replace(/<section id="stack-bottom">.*?<\/section>/, ''), 'content', () => !document.getElementById('stack-bottom'));
  const s = r.snap;
  const pass = r.ok && s.idx.h === 1 && s.idx.v === 0 && s.present === 'stack-top' && s.marker === 'A';
  return { item: 'A5 [6] remove the current vertical slide: clamped within the stack', pass, idx: s.idx, present: s.present };
}

async function a6Code() {
  const probe = () => {
    const p = document.querySelector('#code pre');
    return { codes: p.querySelectorAll('code').length, wrapper: p.classList.contains('code-wrapper'),
      hljs: p.querySelectorAll('code.hljs').length, ln: p.querySelectorAll('table.hljs-ln').length,
      firstLine: (p.querySelector('.hljs-ln-code') || {}).textContent,
      steps: Array.from(p.querySelectorAll('code.fragment'), (f) => f.className + ' ' + f.getAttribute('data-line-numbers')),
      html: p.innerHTML.length };
  };
  Reveal.slide(2, 0, 0); await qa.wait(500);
  const before = probe();
  const r = await qa.edit((h) => h.replace('<h2>Code</h2>', '<h2>Code EDITED</h2>'), 'content', () => document.querySelector('#code h2').textContent === 'Code EDITED');
  await qa.wait(300);
  const after = probe(), s = qa.snap();
  const pass = r.ok && JSON.stringify(before) === JSON.stringify(after) && s.idx.h === 2 && s.idx.f === 0 && s.marker === 'A';
  return { item: 'A6 [11] stepped code slide on fragment 0: highlighted exactly as on load, fragment kept', pass, idx: s.idx, before, after };
}

async function a7Math() {
  Reveal.slide(3); await qa.wait(300);
  const r = await qa.edit((h) => h.replace('<h2>Math</h2>', '<h2>Math EDITED</h2>'), 'content', () => document.querySelector('#math h2').textContent === 'Math EDITED');
  const katex = document.querySelectorAll('#math .katex').length;
  return { item: 'A7 [12] math slide: KaTeX rendered in the swapped slides', pass: r.ok && katex === 1 && r.snap.idx.h === 3 && r.snap.marker === 'A', katex, idx: r.snap.idx };
}

async function a8Title() {
  await qa.get('/__mock/title?t=' + encodeURIComponent('New title'));
  const r = await qa.edit(null, 'content', () => document.title === 'New title');
  return { item: 'A8 [13] <title> change: document.title updated in place', pass: r.ok && r.snap.marker === 'A' && r.snap.idx.h === 3, title: document.title };
}

async function a9Overview() {
  Reveal.toggleOverview(true); Reveal.togglePause(true); await qa.wait(300);
  const r = await qa.edit((h) => h.replace('<h2>Math EDITED</h2>', '<h2>Math ov</h2>'), 'content', () => document.querySelector('#math h2').textContent === 'Math ov');
  const res = { overview: Reveal.isOverview(), paused: Reveal.isPaused(), idx: r.snap.idx };
  Reveal.togglePause(false); Reveal.toggleOverview(false); await qa.wait(300);
  return { item: 'A9 [14] overview + paused state survive a content swap', pass: r.ok && res.overview && res.paused && res.idx.h === 3 && r.snap.marker === 'A', ...res };
}

async function a10Hidden() {
  const r = await qa.edit((h) => h.replace('<section id="last">', '<section id="secret" data-visibility="hidden"><h2>Hidden</h2></section><section id="last">'), 'content');
  return { item: 'A10 [15] data-visibility=hidden slide removed after a swap (as on load)', pass: r.ok && !document.getElementById('secret') && r.snap.marker === 'A', total: r.snap.total };
}

async function a11NoFlash() {
  const gaps = [];
  const mo = new MutationObserver((ms) => {
    for (const m of ms) for (const n of m.removedNodes) {
      if (n.tagName !== 'LINK') continue;
      const base = n.getAttribute('href').replace(/\?.*/, '');
      const live = Array.from(document.querySelectorAll('link[rel=stylesheet]')).filter((l) => l.getAttribute('href').startsWith(base));
      gaps.push({ removed: n.getAttribute('href'), replacement: live.map((l) => ({ href: l.getAttribute('href'), loaded: !!l.sheet, id: l.id })) });
    }
  });
  mo.observe(document.head, { childList: true });
  const r = await qa.edit(null, 'css');
  mo.disconnect();
  const pass = r.ok && gaps.length === 3 && gaps.every((g) => g.replacement.length === 1 && g.replacement[0].loaded)
    && document.querySelectorAll('#presenter-theme').length === 1
    && document.querySelector('#presenter-theme').getAttribute('href') === '/__presenter/theme.css?v=' + r.ev.build_id;
  return { item: 'A11 [1] each old <link> removed only after its replacement loaded (no flash); one #presenter-theme', pass, gaps };
}

async function a12Burst() {
  let last;
  for (let i = 0; i < 5; i++) {
    await qa.post('/__mock/slides', (await qa.slidesHtml()).replace(/<h2>Math[^<]*<\/h2>/, '<h2>Math burst ' + i + '</h2>'));
    last = JSON.parse(await qa.get('/__mock/emit?kind=' + (i % 2 ? 'css' : 'content'))).build_id;
  }
  const ok = await qa.until(() => qa.lr().buildId === last);
  await qa.wait(500);
  const s = qa.snap();
  const pass = ok && document.querySelector('#math h2').textContent === 'Math burst 4' && s.stored === String(last)
    && JSON.stringify(s.theme) === JSON.stringify(['/__presenter/theme.css?v=' + last]) && s.marker === 'A' && s.idx.h === 3;
  return { item: 'A12 [16] burst of 5 builds (content/css alternating): applied in order, final state = last build', pass, last, theme: s.theme, stored: s.stored };
}

// --- B: error overlay (items 7, 8, 23) ---------------------------------------------------

async function b1Error() {
  window.__qaMarker = 'B'; Reveal.slide(2); await qa.wait(300); await qa.mock();
  const r = await qa.edit(null, 'error', () => qa.ov() && qa.ov().shown);
  const o = qa.ov();
  const pass = r.ok && o.position === 'fixed' && o.z === '10000' && /monospace/.test(o.font) && o.width <= innerWidth / 2 && o.top < 50 && o.right < 50
    && o.text === "build error (parse)\nslides.md:41: unknown directive 'include'\nslides.md:41\n(click to dismiss)"
    && r.snap.idx.h === 2 && r.snap.marker === 'B' && r.requests.length === 0 && !!document.querySelector('.reveal .slides section.present');
  return { item: 'B1 [7] parse error: overlay (kind, message, file:line, dismiss hint) over the visible deck', pass, ov: o, requests: r.requests };
}

async function b2Dismiss() {
  document.getElementById('presenter-overlay').click();
  const hidden = !qa.ov().shown;
  const r = await qa.edit(null, 'error&message=' + encodeURIComponent('theme.scss: expected "}"') + '&file=theme.scss&line=3', () => qa.ov().shown);
  const o = qa.ov();
  return { item: 'B2 [7] a click dismisses; the next error shows the overlay again', pass: hidden && r.ok && o.text.includes('theme.scss:3') && o.count === 1 && window.__qaMarker === 'B', text: o.text };
}

async function b5Reconnect() {
  const text = qa.ov().text;
  document.getElementById('presenter-overlay').click();
  await qa.mock();
  await qa.get('/__mock/down?ms=300');
  const reconnected = await qa.until(() => !qa.lr().connected) && await qa.until(() => qa.lr().connected);
  await qa.wait(800);
  const o = qa.ov(), req = await qa.mock();
  const pass = reconnected && !o.shown && o.text === text && o.count === 1 && window.__qaMarker === 'B' && !req.includes('GET / [empty]');
  return { item: 'B5 [23] dismissed overlay, reconnect while the same build fails: stays hidden and unchanged, no refresh', pass, ov: o, requests: req };
}

async function b3FixCss() {
  const r = await qa.edit((h) => h.replace('<h2>Code</h2>', '<h2>Code fixed by css</h2>'), 'css', () => !qa.ov().shown);
  const s = r.snap;
  const pass = r.ok && s.lr.lastStatus === 'ok' && s.marker === 'B' && s.idx.h === 2
    && document.querySelector('#code h2').textContent === 'Code fixed by css'
    && r.requests.includes('GET / [empty]') && r.requests.includes('GET /__presenter/theme.css?v=' + r.ev.build_id + ' [style]')
    && !r.requests.some((q) => q.includes('/vendor/'));
  return { item: 'B3 [8] css build after an error: overlay gone; slides and theme CSS refreshed', pass, requests: r.requests };
}

async function b4FixContent() {
  await qa.edit(null, 'error', () => qa.ov().shown);
  const r = await qa.edit((h) => h.replace(/<h2>Code[^<]*<\/h2>/, '<h2>Code fixed</h2>'), 'content', () => !qa.ov().shown);
  const pass = r.ok && document.querySelector('#code h2').textContent === 'Code fixed' && r.snap.marker === 'B' && r.snap.idx.h === 2;
  return { item: 'B4 [8] error then a good content build: overlay gone, new content shown', pass };
}

// --- C: outage (item 9) -----------------------------------------------------------------

async function c1Outage() {
  window.__qaMarker = 'C'; await qa.mock();
  const t0 = Date.now(), tl = [];
  let last = '';
  await qa.get('/__mock/down?ms=2500');
  while (Date.now() - t0 < 8000) {
    const s = qa.lr(), k = s.connected + '/' + s.retryMs;
    if (k !== last) { tl.push([Date.now() - t0, s.connected, s.retryMs]); last = k; }
    await qa.wait(25);
  }
  const s = qa.snap(), req = await qa.mock();
  const r = await qa.edit((h) => h.replace('<h2>Math</h2>', '<h2>Math after outage</h2>'), 'content', () => document.querySelector('#math h2').textContent === 'Math after outage');
  // retryMs is the delay of the *next* attempt: 1000/2000/4000 after waiting 500/1000/2000
  const down = tl.filter((e) => !e[1]).map((e) => e[2]), end = tl[tl.length - 1];
  const pass = JSON.stringify(down) === '[1000,2000,4000]' && end[1] && end[2] === 500 && s.marker === 'C' && s.idx.h === 3
    && r.ok && r.snap.marker === 'C' && req.includes('GET / [empty]');
  return { item: 'C1 [9] server unreachable (503): backoff 0.5/1/2 s, reconnect (same id: in-place refresh), no reload, updates resume', pass, timeline: tl, requests: req };
}

// --- D: full reloads (items 10, 17-19, 21) ----------------------------------------------

async function d1Missed() {
  window.__qaMarker = 'D1';
  await qa.get('/__mock/down?ms=1500');
  await qa.post('/__mock/slides', (await qa.slidesHtml()).replace('<h2>Math</h2>', '<h2>Math missed</h2>'));
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function d1Check() {
  const pass = !window.__qaMarker && qa.nav() === 'reload' && location.hash === '#/math'
    && document.querySelector('#math h2').textContent === 'Math missed' && qa.snap().stored === '2';
  return { item: 'D1 [17] build missed while disconnected: one full reload, slide kept', pass, hash: location.hash, stored: qa.snap().stored };
}
async function d2Restart() { window.__qaMarker = 'D2'; await qa.get('/__mock/restart'); return 'restart requested'; }
function d2Check() {
  const pass = !window.__qaMarker && qa.nav() === 'reload' && location.hash === '#/math' && qa.snap().stored === '1';
  return { item: 'D2 [10] server restart (hello build_id 1 != 2): one full reload, slide kept', pass, hash: location.hash, stored: qa.snap().stored };
}
async function d3Head() {
  window.__qaMarker = 'D3';
  await qa.post('/__mock/head', '\n<link rel="stylesheet" href="/__presenter/asset/vendor/reveal/dist/theme/black.css">');
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function d3Check() {
  const bg = getComputedStyle(document.body).backgroundColor;
  const pass = !window.__qaMarker && qa.nav() === 'reload' && location.hash === '#/math' && qa.snap().stored === '2' && bg === 'rgb(25, 25, 25)';
  return { item: 'D3 [18] document with other stylesheets (reveal_theme changed): one full reload', pass, bg };
}
async function d6Style() {
  window.__qaMarker = 'D6';
  await qa.post('/__mock/head', '\n<link rel="stylesheet" href="/__presenter/asset/vendor/reveal/dist/theme/black.css">\n<style id="tpl-style">.reveal h2 { letter-spacing: 7px; }</style>');
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function d6Check() {
  const ls = getComputedStyle(document.querySelector('#math h2')).letterSpacing;
  const pass = !window.__qaMarker && qa.nav() === 'reload' && !!document.getElementById('tpl-style') && ls === '7px' && qa.snap().stored === '3';
  return { item: 'D6 [19] template edit, inline <style> added to <head>: one full reload, style applied', pass, ls };
}
async function d7Body() {
  window.__qaMarker = 'D7';
  await qa.post('/__mock/body', '\n<div class="footer" style="position:fixed;bottom:0">footer v1</div>');
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function d7Check() {
  const footer = (document.querySelector('.footer') || {}).textContent;
  const pass = !window.__qaMarker && qa.nav() === 'reload' && footer === 'footer v1' && qa.snap().stored === '4';
  return { item: 'D7 [19] template edit, body_start markup added: one full reload, markup shown', pass, footer };
}
async function d7bBody() {
  window.__qaMarker = 'D7b';
  await qa.post('/__mock/body', '\n<div class="footer" style="position:fixed;bottom:0">footer v2</div>');
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function d7bCheck() {
  const footer = (document.querySelector('.footer') || {}).textContent;
  const pass = !window.__qaMarker && qa.nav() === 'reload' && footer === 'footer v2' && qa.snap().stored === '5';
  return { item: 'D7b [19] template edit, body_start markup changed (v1 -> v2): one full reload, new markup shown', pass, footer };
}
async function d7cText() {
  window.__qaMarker = 'D7c';
  const r = await qa.edit((h) => h.replace(/<h2>Math[^<]*<\/h2>/, '<h2>Math in place</h2>'), 'content', () => document.querySelector('#math h2').textContent === 'Math in place');
  return { item: 'D7c [19] slide edit on a page with head <style>, the highlight plugin <style> and body_start markup: still in place', pass: r.ok && window.__qaMarker === 'D7c' && r.snap.idx.h === 3, requests: r.requests };
}
async function d8Lang() {
  window.__qaMarker = 'D8';
  await qa.get('/__mock/lang?l=de');
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function d8Check() {
  const lang = document.documentElement.lang;
  return { item: 'D8 [19] <html lang> changed (front matter lang): one full reload, new lang', pass: !window.__qaMarker && qa.nav() === 'reload' && lang === 'de' && qa.snap().stored === '7', lang };
}
async function d4Script() {
  window.__qaMarker = 'D4';
  await qa.post('/__mock/slides', (await qa.slidesHtml()).replace(/<h2>Math[^<]*<\/h2>/,
    '<h2>Math</h2><div id="widget"></div><script>document.getElementById("widget").textContent = "widget ran";</script>'));
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function d4Check() {
  const widget = document.getElementById('widget').textContent;
  return { item: 'D4 [21] new slides contain a <script>: one full reload, so it runs', pass: !window.__qaMarker && qa.nav() === 'reload' && widget === 'widget ran' && location.hash === '#/math', widget };
}
async function d5Edit() {
  window.__qaMarker = 'D5';
  await qa.post('/__mock/slides', (await qa.slidesHtml()).replace('<h2>Math</h2>', '<h2>Math 2</h2>'));
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function d5Check() {
  const h2 = document.querySelector('#math h2').textContent;
  return { item: 'D5 [21] text edit, slides keep an unchanged <script>: still a full reload', pass: !window.__qaMarker && qa.nav() === 'reload' && document.getElementById('widget').textContent === 'widget ran' && h2 === 'Math 2', h2 };
}

// --- J: markup after the client's script, client script moved earlier (item 27) ----------

async function j1Edit() {
  const footer = (document.querySelector('.body-end-footer') || {}).textContent;
  const last = document.body.lastElementChild.className;
  const r = await qa.editInPlace('J1');
  return { item: 'J1 [27] body_end markup after the client script: a slide edit is swapped in place', pass: r.ok
    && footer === 'footer at body_end' && last === 'body-end-footer' && qa.nav() === 'navigate', footer, last, ...r };
}
async function j2Reconnect() {
  const r = await qa.reconnectInPlace('J2');
  return { item: 'J2 [27] body_end markup after the client script: a reconnect (same build id) refreshes in place', pass: r.ok && qa.nav() === 'navigate', ...r };
}
async function j3BodyEnd() {
  window.__qaMarker = 'J3';
  await qa.post('/__mock/bodyend', '\n<div class="body-end-footer">footer v2</div>');
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function j3Check() {
  const footer = (document.querySelector('.body-end-footer') || {}).textContent;
  const pass = !window.__qaMarker && qa.nav() === 'reload' && footer === 'footer v2' && qa.snap().stored === '4'
    && location.hash === '#/stack-bottom';
  return { item: 'J3 [19,27] template edit, body_end markup changed: one full reload, new markup shown', pass, footer, hash: location.hash };
}
async function j4Early() {
  window.__qaMarker = 'J4';
  await qa.get('/__mock/early?on=1');
  return JSON.parse(await qa.get('/__mock/emit?kind=content'));
}
function j4Check() {
  const client = document.querySelector('script[src="/__presenter/live-reload.js"]');
  const next = client.nextElementSibling.getAttribute('src');
  const pass = !window.__qaMarker && qa.nav() === 'reload' && next.endsWith('/reveal.js') && qa.snap().stored === '5';
  return { item: 'J4 [19,27] template edit, client script moved before reveal.js: one full reload', pass, next };
}
async function j5Early() {
  const a = await qa.editInPlace('J5');
  const b = await qa.reconnectInPlace('J5');
  return { item: 'J5 [27] client script before reveal.js (child theme `scripts` block): slide edit and reconnect in place', pass: a.ok && b.ok, a, b };
}

// --- E: page opened while the build fails (item 22) -------------------------------------

function e1Check() {
  const o = qa.ov();
  const pass = Reveal.isReady() && Reveal.getTotalSlides() === 6 && !!o && o.shown && o.text === 'build error\ndetails in the terminal\n(click to dismiss)'
    && qa.lr().lastStatus === 'error' && qa.snap().stored === '2';
  return { item: 'E1 [22] deck opened while the latest build failed, §2 hello {build_id, status}: deck usable, generic overlay', pass, ov: o };
}
async function e1bOn() { await qa.get('/__mock/hello-error?on=1'); location.reload(); return 'reloading with hello-error on'; }
async function e1bCheck() {
  const o = qa.ov();
  await qa.get('/__mock/hello-error?on=0');
  const pass = Reveal.isReady() && !!o && o.shown && o.text === 'build error (parse)\nboom\nslides.md:7\n(click to dismiss)' && qa.nav() === 'reload';
  return { item: 'E1b [22] same, with the proposed optional hello.error: overlay shows kind, message, file:line', pass, ov: o };
}
async function e2NoDoc() {
  await qa.get('/__mock/emit?kind=error&message=first&nodoc=1');
  await qa.until(() => qa.ov().text.includes('first'));
  location.reload();
  return 'reloading into the fallback page';
}
async function e2Check() {
  const pre = 'build error (parse)\nfirst\nslides.md:41';
  const o1 = qa.ov();
  await qa.get('/__mock/down?ms=300');
  await qa.until(() => !qa.lr().connected) && await qa.until(() => qa.lr().connected);
  await qa.wait(500);
  const o2 = qa.ov();
  const pass = !window.Reveal && o1 && o1.text === pre && o1.count === 1 && o1.shown && o2.text === pre && o2.count === 1
    && qa.lr().lastStatus === 'error' && qa.snap().stored === '3';
  return { item: "E2 [22] fallback page: the server's pre-rendered overlay is kept as is, also after a reconnect", pass, o1, o2 };
}
async function e3Good() { window.__qaMarker = 'E3'; return JSON.parse(await qa.get('/__mock/emit?kind=content')); }
function e3Check() {
  const pass = !window.__qaMarker && Reveal.isReady() && !document.getElementById('presenter-overlay') && qa.nav() === 'reload' && qa.snap().stored === '4';
  return { item: 'E3 [22] first good build on the fallback page: one full reload into the deck', pass, stored: qa.snap().stored };
}

// --- G: postponed content update (item 24) ---------------------------------------------

async function g1Postponed() {
  window.__qaMarker = 'G';
  await qa.post('/__mock/slides', (await qa.slidesHtml()) + '<section id="added"><h2>Added</h2></section>');
  await qa.get('/__mock/failroot?on=1');
  await qa.get('/__mock/emit?kind=content');
  const stale = await qa.until(() => qa.lr().lastStatus === 'stale');
  const mid = { buildId: qa.lr().buildId, added: !!document.getElementById('added') };
  await qa.get('/__mock/failroot?on=0');
  const r = await qa.edit(null, 'css', () => !!document.getElementById('added'));
  const s = r.snap;
  await qa.get('/__mock/down?ms=300'); // a reconnect afterwards changes nothing
  await qa.until(() => !qa.lr().connected) && await qa.until(() => qa.lr().connected);
  await qa.wait(800);
  const pass = stale && mid.buildId === 1 && !mid.added && r.ok && s.lr.lastStatus === 'ok' && s.stored === String(r.ev.build_id)
    && s.marker === 'G' && window.__qaMarker === 'G' && !!document.getElementById('added') && s.idx.h === 3;
  return { item: 'G1 [24] GET / fails during a content build: postponed (id not stored); the next css build applies the slides too', pass, mid, after: { idx: s.idx, stored: s.stored, lr: s.lr } };
}

// --- H: speaker-view frames (item 20) ---------------------------------------------------

async function h1Frames() {
  const f = [document.getElementById('current').contentWindow, document.getElementById('upcoming').contentWindow];
  const ready = await qa.until(() => f.every((w) => w.__presenterLiveReload?.state.connected && w.Reveal?.isReady()), 8000);
  f.forEach((w, i) => { w.__qaMarker = 'H' + i; });
  await qa.post('/__mock/slides', (await qa.slidesHtml()) + '<section id="added"><h2>Added</h2></section>');
  await qa.get('/__mock/down?ms=1200');
  await qa.until(() => f.every((w) => !w.__presenterLiveReload.state.connected));
  return { ready, ev: JSON.parse(await qa.get('/__mock/emit?kind=content')) };
}
function h1Check() {
  const f = [document.getElementById('current').contentWindow, document.getElementById('upcoming').contentWindow];
  const res = f.map((w) => ({ marker: w.__qaMarker || null, nav: w.performance.getEntriesByType('navigation')[0].type,
    added: !!w.document.getElementById('added'), total: w.Reveal.getTotalSlides(), buildId: w.__presenterLiveReload.state.buildId }));
  const keys = Object.keys(sessionStorage).filter((k) => k.startsWith('presenter:'));
  const pass = keys.length === 2 && res.every((r) => !r.marker && r.nav === 'reload' && r.added && r.total === 7 && r.buildId === 2);
  return { item: 'H1 [20] speaker view (two deck frames in one tab), build missed while disconnected: BOTH frames reload', pass, res, keys: keys.length };
}

// --- R: restart with the same build id (item 10) ----------------------------------------

async function r1RestartSame() {
  window.__qaMarker = 'R'; await qa.mock();
  await qa.get('/__mock/down?ms=1500');
  await qa.until(() => !qa.lr().connected);
  await qa.post('/__mock/slides', (await qa.slidesHtml()).replace('<h2>Math</h2>', '<h2>Math after restart</h2>'));
  await qa.get('/__mock/restart'); // build_id 1 again, new document
  const ok = await qa.until(() => document.querySelector('#math h2').textContent === 'Math after restart', 8000);
  await qa.wait(300);
  const s = qa.snap(), req = await qa.mock();
  const pass = ok && s.marker === 'R' && s.idx.h === 3 && s.stored === '1' && req.includes('GET / [empty]');
  return { item: 'R1 [10] server restarted with the SAME build_id and a changed deck: in-place refresh on reconnect, no reload', pass, idx: s.idx, requests: req };
}

// --- S: scroll view (item 25) -----------------------------------------------------------

async function s1Scroll() {
  const sv = Reveal.isScrollView();
  window.__qaMarker = 'S';
  await qa.post('/__mock/slides', (await qa.slidesHtml()).replace('<h2>Math</h2>', '<h2>Math scroll</h2>'));
  return { scrollView: sv, ev: JSON.parse(await qa.get('/__mock/emit?kind=content')) };
}
function s1Check() {
  const h2 = document.querySelector('#math h2').textContent;
  return { item: 'S1 [25] reveal scroll view: a content build reloads in full', pass: !window.__qaMarker && qa.nav() === 'reload' && Reveal.isScrollView() && h2 === 'Math scroll', h2 };
}

// --- F: real server process stopped and restarted (items 9, 10) -------------------------

function fRecorder() {
  window.__qaMarker = 'real';
  sessionStorage.removeItem('qa_tl');
  const t0 = Date.now(), tl = [];
  let last = '';
  setInterval(() => {
    const s = window.__presenterLiveReload.state, k = s.connected + '/' + s.retryMs;
    if (k === last) return;
    tl.push([Date.now() - t0, s.connected, s.retryMs, window.__qaMarker]);
    last = k;
    sessionStorage.setItem('qa_tl', JSON.stringify(tl)); // survives the reload
  }, 25);
  return 'recorder started';
}
function fCheck() {
  const tl = JSON.parse(sessionStorage.getItem('qa_tl'));
  // retry delays while down; fullReload() itself closes the stream (retryMs back at 500)
  const down = tl.filter((e) => !e[1] && e[2] !== 500).map((e) => e[2]);
  const pass = down.join() === '1000,2000,4000,5000' && tl.every((e) => e[3] === 'real') && !window.__qaMarker
    && qa.nav() === 'reload' && location.hash === '#/stack-bottom' && qa.snap().stored === '1';
  return { item: 'F1 [9,10] real process stopped 16 s: deck stays, backoff 0.5/1/2/4 s then 5 s; restarted (4 -> 1): one reload, slide kept', pass, timeline: tl };
}

// ---------------------------------------------------------------------------------------
// Sessions: each opens a fresh headless Chrome on `path`. Steps are page functions,
// { navigates: fn } (a page function that reloads the page, so its reply may be lost), a
// number (sleep ms) or { node(ctx) } (runs in Node; ctx.mock is the mock server).
// ---------------------------------------------------------------------------------------

const H = installHelpers;
const ctl = (mock, path, body) =>
  fetch(mock.base + path, body === undefined ? {} : { method: 'POST', body }).then((r) => r.text());

const SESSIONS = [
  { id: 'A', path: '/#/1/1', async setup(m) {
    // theme-chain and @theme stylesheets are in the served document from the start
    await ctl(m, '/__mock/head', '<link rel="stylesheet" href="/__presenter/asset/extra.css">\n' +
      '<link rel="stylesheet" href="/__presenter/asset/@corporate/styles/hl.css">');
    await ctl(m, '/__mock/asset?path=extra.css', '.reveal p { color: rgb(0, 0, 1); }');
    await ctl(m, '/__mock/asset?path=%40corporate%2Fstyles%2Fhl.css', '.reveal h1 { color: rgb(0, 0, 2); }');
    await ctl(m, '/__mock/emit?kind=content');
  }, steps: [H, a1Css, a2Text, a3Add, a3bInsert, a3cInsertNoId, a4Remove, a5RemoveV, a6Code, a7Math, a8Title,
    a9Overview, a10Hidden, a11NoFlash, a12Burst] },
  { id: 'B', path: '/#/2', steps: [H, b1Error, b2Dismiss, b5Reconnect, b3FixCss, b4FixContent] },
  { id: 'C', path: '/#/3', steps: [H, c1Outage] },
  { id: 'D', path: '/#/3', steps: [H, d1Missed, 5000, H, d1Check, d2Restart, 3000, H, d2Check,
    d3Head, 3000, H, d3Check, d6Style, 3000, H, d6Check, d7Body, 3000, H, d7Check, d7bBody, 3000, H, d7bCheck, d7cText,
    d8Lang, 3000, H, d8Check, d4Script, 3000, H, d4Check, d5Edit, 3000, H, d5Check] },
  { id: 'E', path: '/#/2', async setup(m) { await ctl(m, '/__mock/emit?kind=error&message=boom&file=slides.md&line=7'); },
    steps: [H, e1Check, { navigates: e1bOn }, 2500, H, e1bCheck, { navigates: e2NoDoc }, 2500, H, e2Check,
      e3Good, 3000, H, e3Check] },
  { id: 'G', path: '/#/3', steps: [H, g1Postponed] },
  { id: 'J', path: '/#/1/1', async setup(m) {
    await ctl(m, '/__mock/bodyend', '\n<div class="body-end-footer">footer at body_end</div>');
    await ctl(m, '/__mock/emit?kind=content'); // build 2
  }, // H again before each scenario, so that one that fails by reloading does not take the next ones with it
  steps: [H, j1Edit, 1500, H, j2Reconnect, 1500, H, j3BodyEnd, 3000, H, j3Check, j4Early, 3000, H, j4Check, j5Early] },
  { id: 'H', path: '/__mock/frames', steps: [H, h1Frames, 5000, h1Check] },
  { id: 'R', path: '/#/3', steps: [H, r1RestartSame] },
  { id: 'S', path: '/?view=scroll#/3', steps: [H, s1Scroll, 3000, H, s1Check] },
  { id: 'F', path: '/#/1/1', ownMock: true, async setup(m) {
    for (let i = 0; i < 3; i++) await ctl(m, '/__mock/emit?kind=content'); // build 4
  }, steps: [H, fRecorder, { async node(ctx) {
    await sleep(5000);
    await ctx.mock.stop();
    await sleep(16000);
    ctx.mock = await startMock(ctx.mock.port); // a new process: build_id 1
  } }, 9000, H, fCheck] },
];

// Starts mock-server.py on `port` (0: a free one).
function startMock(port = 0) {
  return new Promise((resolve, reject) => {
    const proc = spawn('python3', ['-B', join(HERE, 'mock-server.py'), '--port', String(port)], { stdio: ['ignore', 'pipe', 'inherit'] });
    let buf = '';
    proc.stdout.on('data', (d) => {
      buf += d;
      const m = buf.match(/http:\/\/127\.0\.0\.1:(\d+)/);
      if (!m) return;
      const stop = () => new Promise((r) => { proc.once('exit', r); proc.kill(); });
      resolve({ base: m[0], port: +m[1], stop });
    });
    proc.once('exit', (code) => reject(new Error('mock-server.py exited with ' + code)));
  });
}

// One headless Chrome with one tab, driven over the DevTools protocol.
async function launchChrome(width = 1280, height = 720) {
  const profile = mkdtempSync(join(tmpdir(), 'presenter-qa-'));
  const proc = spawn(CHROME, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
    '--disable-extensions', '--disable-background-networking', '--disable-sync', '--remote-debugging-port=0',
    `--user-data-dir=${profile}`, `--window-size=${width},${height}`, 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
  const port = await new Promise((resolve, reject) => {
    let buf = '';
    proc.stderr.on('data', (d) => {
      buf += d;
      const m = buf.match(/DevTools listening on ws:\/\/[^:]+:(\d+)\//);
      if (m) resolve(+m[1]);
    });
    proc.once('exit', (code) => reject(new Error('chrome exited with ' + code)));
  });
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })).json();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  let id = 0, loads = 0;
  const pending = new Map(), logs = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      const { res, rej } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) rej(new Error(JSON.stringify(msg.error)));
      else res(msg.result);
    } else if (msg.method === 'Page.loadEventFired') {
      loads++;
    } else if (msg.method === 'Runtime.consoleAPICalled') {
      logs.push(`console.${msg.params.type}: ` + msg.params.args.map((a) => a.value ?? a.description ?? a.type).join(' '));
    } else if (msg.method === 'Runtime.exceptionThrown') {
      logs.push('EXCEPTION: ' + (msg.params.exceptionDetails.exception?.description || msg.params.exceptionDetails.text));
    } else if (msg.method === 'Log.entryAdded') {
      logs.push(`log.${msg.params.entry.level}: ${msg.params.entry.text} ${msg.params.entry.url || ''}`);
    }
  };
  const send = (method, params = {}) => new Promise((res, rej) => {
    pending.set(++id, { res, rej });
    ws.send(JSON.stringify({ id, method, params }));
  });
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Log.enable');
  await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
  return {
    logs,
    async goto(url) {
      const n = loads;
      await send('Page.navigate', { url });
      while (loads === n) await sleep(50);
    },
    // Evaluates a page function; { value } or { error }.
    async run(fn) {
      try {
        const r = await send('Runtime.evaluate', { expression: `(${fn})()`, awaitPromise: true, returnByValue: true });
        if (r.exceptionDetails) return { error: r.exceptionDetails.exception?.description || r.exceptionDetails.text };
        return { value: r.result.value };
      } catch (e) {
        return { error: 'protocol: ' + e.message }; // e.g. the page navigated during the call
      }
    },
    async screenshot(file) {
      const r = await send('Page.captureScreenshot', { format: 'png' });
      writeFileSync(file, Buffer.from(r.data, 'base64'));
    },
    async close() {
      try { ws.close(); } catch { /* already closed */ }
      await new Promise((r) => { proc.once('exit', r); proc.kill('SIGKILL'); });
      rmSync(profile, { recursive: true, force: true });
    },
  };
}

async function runSession(s, shared, results) {
  const ctx = { mock: s.ownMock ? await startMock() : shared };
  await ctl(ctx.mock, '/__mock/reset');
  if (s.setup) await s.setup(ctx.mock);
  const page = await launchChrome();
  const out = [];
  try {
    await page.goto(ctx.mock.base + s.path);
    await sleep(1500);
    for (const step of s.steps) {
      if (typeof step === 'number') { await sleep(step); continue; }
      if (step.node) { await step.node(ctx); continue; }
      const fn = step.navigates || step;
      const r = await page.run(fn);
      out.push(fn.name + ': ' + JSON.stringify(r));
      if (r.value && typeof r.value === 'object' && 'pass' in r.value) results.push({ item: r.value.item, pass: r.value.pass });
      else if (r.error && !(step.navigates && r.error.startsWith('protocol:'))) {
        results.push({ item: `${s.id} ${fn.name} threw: ${r.error}`, pass: false });
      }
    }
    // e.g. while a reload the client should not have made is in progress
    if (OUT) await page.screenshot(join(OUT, s.id + '.png')).catch((e) => out.push('screenshot failed: ' + e.message));
  } finally {
    await page.close();
    if (s.ownMock) await ctx.mock.stop();
  }
  if (OUT) writeFileSync(join(OUT, s.id + '.log'), [...out, '', ...page.logs].join('\n') + '\n');
  return page.logs;
}

async function main() {
  if (OUT) mkdirSync(OUT, { recursive: true });
  const shared = await startMock();
  const results = [], logs = [];
  try {
    for (const s of SESSIONS.filter((x) => ONLY.includes(x.id))) {
      process.stderr.write(`session ${s.id}...\n`);
      logs.push(...await runSession(s, shared, results));
    }
  } finally {
    await shared.stop();
  }
  // Item 26: no uncaught exceptions; every console message of the client is [presenter]-prefixed
  // (Chrome's own "Failed to load resource" lines come from the deliberate outages).
  const bad = logs.filter((l) => l.startsWith('EXCEPTION') || (l.startsWith('console.') && !l.startsWith('console.debug: [presenter] ')));
  results.push({ item: 'Z1 [26] console: no exceptions, every client message prefixed [presenter]' + (bad.length ? ': ' + bad.join(' | ') : ''), pass: bad.length === 0 });
  for (const r of results) console.log(`${r.pass ? 'PASS' : 'FAIL'}  ${r.item}`);
  const failed = results.filter((r) => !r.pass).length;
  console.log(`${results.length - failed} passed, ${failed} failed`);
  process.exit(failed ? 1 : 0);
}

main().catch((e) => {
  console.error('qa.mjs: ' + (e.stack || e));
  process.exit(2);
});
