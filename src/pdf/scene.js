// The scene extractor of `presenter export pptx` (src/pdf/capture.rs embeds this file).
//
// Runs in reveal.js' print view (`?print-pdf`, after the pages are laid out) as
// `(SCENE)(pageIndex)`: it walks the DOM of one `.pdf-page` and returns a JSON scene, the
// page as PowerPoint shapes in slide coordinates (CSS pixels from the top-left corner of the
// slide area inside the page). A page taller than one page is split into `parts`, one per
// printed page, each with the shapes whose centre falls on it.
//
//   { background: { color, image, alpha, crop, fit },
//     parts: [ { shapes: [ shape, ... ] }, ... ],
//     images: n }
//
// A shape is one of
//   { kind: "text", frame, insets, fill, line, radius, wrap, title, paragraphs }
//   { kind: "picture", frame, imageRef | shot, alt, alpha, crop, fit }
//   { kind: "rect", frame, fill, line, radius }
//   { kind: "table", frame, columns, rows }
// with frame = { x, y, w, h } in slide pixels. A picture's bytes are fetched afterwards:
// `imageRef` indexes `window.__presenterImages` (an element with a `src`, or a URL string),
// which `(IMAGE)(ref)` resolves to a data URL; `shot` is a rectangle in document pixels to
// photograph instead (SVG, canvas, iframes, display math, gradients: what PowerPoint cannot
// hold as native content). Colours are { r, g, b, a }.
//
// Text keeps what PowerPoint can keep: paragraphs with alignment, line height, bullets or
// numbering and indentation; runs with the font family Chrome used (the first installed
// family of the CSS stack; generic families become Arial, Times New Roman and Courier New),
// size, weight, style, decorations, colour, highlight, hyperlink, capitals, baseline shift
// and letter spacing. Fragments that are not shown on this page are hidden by reveal's own
// CSS (`visibility: hidden`), so they are left out like any other hidden element; reveal's
// slide number and the speaker notes are left out too.
(function (pageIndex) {
  'use strict';

  const ELEMENT = 1;
  const TEXT = 3;
  // What becomes a picture: what PowerPoint cannot hold as text or as a native shape.
  const RASTER = 'svg, canvas, video, iframe, object, embed, math, .katex-display';
  const SKIP = 'script, style, template, noscript, aside.notes, .slide-number-pdf, ' +
    '.speaker-notes-pdf, audio, link, meta, .katex-html';
  const GENERIC_FONTS = {
    'sans-serif': 'Arial', 'system-ui': 'Arial', '-apple-system': 'Arial',
    'blinkmacsystemfont': 'Arial', 'ui-sans-serif': 'Arial', 'ui-rounded': 'Arial',
    'serif': 'Times New Roman', 'ui-serif': 'Times New Roman',
    'monospace': 'Courier New', 'ui-monospace': 'Courier New',
    'cursive': 'Comic Sans MS', 'fantasy': 'Impact', 'math': 'Cambria Math',
    'emoji': 'Apple Color Emoji', 'fangsong': 'FangSong',
  };
  const NUMBERING = {
    'decimal': 'arabicPeriod', 'decimal-leading-zero': 'arabicPeriod',
    'lower-alpha': 'alphaLcPeriod', 'lower-latin': 'alphaLcPeriod',
    'upper-alpha': 'alphaUcPeriod', 'upper-latin': 'alphaUcPeriod',
    'lower-roman': 'romanLcPeriod', 'upper-roman': 'romanUcPeriod',
  };
  const BULLETS = { disc: '•', circle: '◦', square: '▪' };
  // Combining characters for the accents KaTeX puts over a base (`\hat x` is an `mover`).
  const ACCENTS = {
    '^': '̂', 'ˆ': '̂', '¯': '̄', '‾': '̄', '˙': '̇', '¨': '̈',
    '˜': '̃', '~': '̃', '→': '⃗', '⃗': '⃗', '˘': '̆', 'ˇ': '̌',
    '´': '́', '`': '̀',
  };

  const config = Reveal.getConfig();
  const size = Reveal.getComputedSlideSize(window.innerWidth, window.innerHeight);
  const pageWidth = Math.floor(size.width * (1 + config.margin));
  const pageHeight = Math.floor(size.height * (1 + config.margin));
  const page = document.querySelectorAll('.reveal .slides .pdf-page')[pageIndex];
  if (!page) throw new Error('no page ' + pageIndex);
  const section = page.querySelector(':scope > section');
  const pageRect = page.getBoundingClientRect();
  const parts = Math.max(1, Math.round(page.offsetHeight / pageHeight));
  const marginX = (pageWidth - size.width) / 2;
  const marginY = (pageHeight - size.height) / 2;
  const scrollX = window.scrollX;
  const scrollY = window.scrollY;
  const images = (window.__presenterImages = []);
  const scene = { background: null, parts: [], images: 0 };
  for (let i = 0; i < parts; i++) scene.parts.push({ shapes: [] });
  let titleTaken = false;

  // --- Geometry -------------------------------------------------------------------------

  const num = (v) => {
    const n = parseFloat(v);
    return Number.isFinite(n) ? n : 0;
  };
  const round = (n) => Math.round(n * 100) / 100;

  // A viewport rectangle (`getBoundingClientRect`) as a frame in slide coordinates, and the
  // part of the page it falls on.
  function frameOf(rect) {
    const centre = rect.top + rect.height / 2 - pageRect.top;
    const part = Math.min(parts - 1, Math.max(0, Math.floor(centre / pageHeight)));
    return {
      part,
      frame: {
        x: round(rect.left - pageRect.left - marginX),
        y: round(rect.top - pageRect.top - part * pageHeight - marginY),
        w: round(rect.width),
        h: round(rect.height),
      },
    };
  }

  // A viewport rectangle in document coordinates, for a photograph.
  function shotOf(rect) {
    return {
      x: round(rect.left + scrollX),
      y: round(rect.top + scrollY),
      w: round(rect.width),
      h: round(rect.height),
    };
  }

  function emit(shape, part) {
    if (shape.frame.w <= 0 || shape.frame.h <= 0) return;
    scene.parts[part].shapes.push(shape);
  }

  // --- Colours and fills ----------------------------------------------------------------

  function alphaOf(text) {
    if (text === undefined || text === null || text === '') return 1;
    const n = parseFloat(text);
    if (!Number.isFinite(n)) return 1;
    return String(text).trim().endsWith('%') ? n / 100 : n;
  }

  // `rgb(…)`, `rgba(…)` or `color(srgb …)` (what computed styles use) as { r, g, b, a };
  // null for `transparent` or anything else.
  function color(text) {
    if (!text) return null;
    text = String(text).trim();
    if (text === 'transparent' || text === 'none') return null;
    let m = text.match(/^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:\s*[,/]\s*([\d.]+%?))?\s*\)$/);
    if (m) {
      const a = alphaOf(m[4]);
      if (a <= 0) return null;
      return { r: Math.round(+m[1]), g: Math.round(+m[2]), b: Math.round(+m[3]), a };
    }
    m = text.match(/^color\(srgb\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*([\d.]+%?))?\s*\)$/);
    if (m) {
      const a = alphaOf(m[4]);
      if (a <= 0) return null;
      const c = (v) => Math.max(0, Math.min(255, Math.round(parseFloat(v) * 255)));
      return { r: c(m[1]), g: c(m[2]), b: c(m[3]), a };
    }
    m = text.match(/^(oklab|oklch)\(\s*([^)]*?)\s*\)$/);
    if (m) return oklab(m[1], m[2]);
    return viaCanvas(text);
  }

  // An `oklab(L a b / alpha)` or `oklch(L C h / alpha)` colour (what Chrome computes for
  // `color-mix(in oklab, …)`, which canvases keep as they are) converted to sRGB.
  function oklab(kind, args) {
    const [values, alphaText] = args.split('/').map((part) => part.trim());
    const parts = values.split(/\s+/);
    if (parts.length < 3) return null;
    const number = (word, scale) => {
      if (word === 'none') return 0;
      const n = parseFloat(word);
      if (!Number.isFinite(n)) return NaN;
      return word.endsWith('%') ? (n / 100) * scale : n;
    };
    const L = number(parts[0], 1);
    let a;
    let b;
    if (kind === 'oklch') {
      const C = number(parts[1], 0.4);
      let h = parts[2] === 'none' ? 0 : parseFloat(parts[2]);
      if (parts[2].endsWith('rad')) h = (h * 180) / Math.PI;
      else if (parts[2].endsWith('turn')) h *= 360;
      else if (parts[2].endsWith('grad')) h *= 0.9;
      a = C * Math.cos((h * Math.PI) / 180);
      b = C * Math.sin((h * Math.PI) / 180);
    } else {
      a = number(parts[1], 0.4);
      b = number(parts[2], 0.4);
    }
    if (![L, a, b].every(Number.isFinite)) return null;
    const alpha = alphaOf(alphaText);
    if (alpha <= 0) return null;
    const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
    const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
    const s_ = L - 0.0894841775 * a - 1.291485548 * b;
    const l = l_ * l_ * l_;
    const m = m_ * m_ * m_;
    const s = s_ * s_ * s_;
    const linear = [
      4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
      -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
      -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
    ];
    const channel = (x) => {
      x = Math.max(0, Math.min(1, x));
      const v = x <= 0.0031308 ? 12.92 * x : 1.055 * Math.pow(x, 1 / 2.4) - 0.055;
      return Math.round(v * 255);
    };
    return { r: channel(linear[0]), g: channel(linear[1]), b: channel(linear[2]), a: alpha };
  }

  // Any other colour syntax (`oklab()`, `oklch()`, `lab()`, a name…) through a canvas, whose
  // fill style serialises as `#rrggbb` or `rgba(…)` in sRGB; null when the canvas does not
  // take it.
  const colorContext = document.createElement('canvas').getContext('2d');
  function viaCanvas(text) {
    if (!colorContext || /^(rgba?|color)\(/.test(text)) return null;
    colorContext.fillStyle = '#010203';
    colorContext.fillStyle = text;
    const out = colorContext.fillStyle;
    if (out === '#010203') return null;
    const hex = out.match(/^#([0-9a-f]{6})$/i);
    if (hex) {
      const n = parseInt(hex[1], 16);
      return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255, a: 1 };
    }
    const m = out.match(/^rgba\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)[\s,/]+([\d.]+)\s*\)$/);
    if (!m) return null;
    const a = alphaOf(m[4]);
    return a <= 0 ? null : { r: Math.round(+m[1]), g: Math.round(+m[2]), b: Math.round(+m[3]), a };
  }

  function withOpacity(c, opacity) {
    if (!c) return null;
    const a = Math.max(0, Math.min(1, c.a * opacity));
    return a <= 0 ? null : { r: c.r, g: c.g, b: c.b, a };
  }

  // `c` composited over the opaque `base`.
  function over(c, base) {
    if (!c) return base;
    if (c.a >= 1 || !base) return { r: c.r, g: c.g, b: c.b, a: 1 };
    const mix = (x, y) => Math.round(x * c.a + y * (1 - c.a));
    return { r: mix(c.r, base.r), g: mix(c.g, base.g), b: mix(c.b, base.b), a: 1 };
  }

  // Splits `text` at the commas outside parentheses.
  function splitTopLevel(text) {
    const out = [];
    let depth = 0;
    let start = 0;
    for (let i = 0; i < text.length; i++) {
      const ch = text[i];
      if (ch === '(') depth++;
      else if (ch === ')') depth--;
      else if (ch === ',' && depth === 0) {
        out.push(text.slice(start, i).trim());
        start = i + 1;
      }
    }
    out.push(text.slice(start).trim());
    return out;
  }

  // The first `linear-gradient(…)` of a computed `background-image` as a fill; null for
  // anything else. The angle is CSS's (0 = to top, 90 = to right).
  function gradient(text) {
    const m = String(text).match(/linear-gradient\((.*)\)/s);
    if (!m) return null;
    const args = splitTopLevel(m[1]);
    let angle = 180;
    let first = args[0] || '';
    if (/^to\s/.test(first)) {
      const dirs = {
        'top': 0, 'right': 90, 'bottom': 180, 'left': 270, 'top right': 45, 'right top': 45,
        'bottom right': 135, 'right bottom': 135, 'bottom left': 225, 'left bottom': 225,
        'top left': 315, 'left top': 315,
      };
      angle = dirs[first.slice(3).trim().split(/\s+/).join(' ')] ?? 180;
      args.shift();
    } else if (/^-?[\d.]+(deg|turn|rad|grad)$/.test(first)) {
      const n = parseFloat(first);
      if (first.endsWith('turn')) angle = n * 360;
      else if (first.endsWith('rad')) angle = (n * 180) / Math.PI;
      else if (first.endsWith('grad')) angle = n * 0.9;
      else angle = n;
      args.shift();
    }
    const stops = [];
    for (const arg of args) {
      const pos = arg.match(/\s(-?[\d.]+)%\s*$/);
      const c = color(pos ? arg.slice(0, pos.index).trim() : arg) || { r: 0, g: 0, b: 0, a: 0 };
      stops.push({ color: c, pos: pos ? Math.max(0, Math.min(1, parseFloat(pos[1]) / 100)) : null });
    }
    if (stops.length < 2) return null;
    for (let i = 0; i < stops.length; i++) {
      if (stops[i].pos === null) {
        stops[i].pos = i === 0 ? 0 : i === stops.length - 1 ? 1 : null;
      }
    }
    // Stops without a position are spread evenly between their neighbours.
    for (let i = 0; i < stops.length; i++) {
      if (stops[i].pos !== null) continue;
      let j = i;
      while (stops[j].pos === null) j++;
      const from = stops[i - 1].pos;
      const to = stops[j].pos;
      for (let k = i; k < j; k++) stops[k].pos = from + ((to - from) * (k - i + 1)) / (j - i + 1);
    }
    return { gradient: { angle: ((angle % 360) + 360) % 360, stops } };
  }

  // The fill of an element's box from its computed style (a gradient wins over the colour).
  function fillOf(cs, opacity) {
    const grad = gradient(cs.backgroundImage);
    if (grad) {
      grad.gradient.stops = grad.gradient.stops.map((s) => ({ ...s, color: { ...s.color, a: s.color.a * opacity } }));
      return grad;
    }
    const c = withOpacity(color(cs.backgroundColor), opacity);
    return c ? { color: c } : null;
  }

  // The uniform border of an element's box as a line, or null when the sides differ (then
  // they become their own rectangles) or there is none.
  function lineOf(cs, opacity) {
    const sides = ['Top', 'Right', 'Bottom', 'Left'].map((side) => borderSide(cs, side, opacity));
    if (sides.some((s) => !s)) return null;
    const [t] = sides;
    const same = sides.every((s) => s.width === t.width && s.dash === t.dash && JSON.stringify(s.color) === JSON.stringify(t.color));
    return same ? t : null;
  }

  function borderSide(cs, side, opacity) {
    const width = num(cs['border' + side + 'Width']);
    const style = cs['border' + side + 'Style'];
    const c = withOpacity(color(cs['border' + side + 'Color']), opacity);
    if (width <= 0 || !style || style === 'none' || style === 'hidden' || !c) return null;
    return { color: c, width, dash: style === 'dashed' || style === 'dotted' };
  }

  function radiusOf(cs) {
    const r = Math.min(num(cs.borderTopLeftRadius), num(cs.borderTopRightRadius), num(cs.borderBottomLeftRadius), num(cs.borderBottomRightRadius));
    return r > 0 ? round(r) : null;
  }

  // --- Fonts ----------------------------------------------------------------------------

  const canvas = document.createElement('canvas');
  const measure = canvas.getContext('2d');
  const fontCache = new Map();

  function textWidth(font) {
    measure.font = '72px ' + font;
    return measure.measureText('mmmmmmmmmmlli1!@#WWW0O0').width;
  }

  // Whether `family` is available to Chrome (installed, or a loaded web font): rendered in
  // it, a test string differs in width from the generic fallbacks.
  function installed(family) {
    const quoted = JSON.stringify(family);
    return ['monospace', 'serif', 'sans-serif'].some((g) => textWidth(quoted + ', ' + g) !== textWidth(g));
  }

  // The font PowerPoint gets for a computed `font-family` stack: the first family Chrome
  // can use, generic families mapped to fonts every Office has.
  function fontOf(stack) {
    if (fontCache.has(stack)) return fontCache.get(stack);
    let found = null;
    for (let family of splitTopLevel(stack)) {
      family = family.replace(/^["']|["']$/g, '').trim();
      if (!family) continue;
      const generic = GENERIC_FONTS[family.toLowerCase()];
      if (generic) {
        found = generic;
        break;
      }
      if (installed(family)) {
        found = family;
        break;
      }
    }
    if (!found) found = 'Arial';
    fontCache.set(stack, found);
    return found;
  }

  // --- Visibility -----------------------------------------------------------------------

  // Whether `el` is not drawn. Only the computed style counts: reveal.js puts the `hidden`
  // attribute on the slides that are not the current one, and its print stylesheet shows
  // them regardless.
  function hidden(el, cs) {
    if (el.matches(SKIP)) return true;
    return cs.display === 'none' || cs.visibility !== 'visible';
  }

  // The opaque colour behind `el`: the nearest ancestor with an opaque background, else the
  // slide's background colour, else white.
  function backdropOf(el) {
    let base = scene.background && scene.background.color ? over(scene.background.color, { r: 255, g: 255, b: 255, a: 1 }) : { r: 255, g: 255, b: 255, a: 1 };
    const chain = [];
    for (let e = el; e && e !== page; e = e.parentElement) chain.push(e);
    for (let i = chain.length - 1; i >= 0; i--) {
      const c = color(getComputedStyle(chain[i]).backgroundColor);
      if (c) base = over(c, base);
    }
    return base;
  }

  // --- Pseudo-elements ------------------------------------------------------------------

  // The `content` of a pseudo-element as text: quoted strings and `attr()` values
  // concatenated; null for `none`/`normal` or when it holds anything else.
  function contentText(content, el) {
    if (!content || content === 'none' || content === 'normal') return null;
    let text = '';
    const re = /"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|attr\(\s*([\w-]+)\s*\)|url\([^)]*\)|[a-z-]+\([^)]*\)|\S+/g;
    let m;
    let any = false;
    while ((m = re.exec(content))) {
      if (m[1] !== undefined || m[2] !== undefined) {
        const raw = m[1] !== undefined ? m[1] : m[2];
        text += raw.replace(/\\([0-9a-fA-F]{1,6})\s?/g, (_, hex) => String.fromCodePoint(parseInt(hex, 16))).replace(/\\(.)/g, '$1');
        any = true;
      } else if (m[3] !== undefined) {
        text += el.getAttribute(m[3]) || '';
        any = true;
      } else if (/^url\(/.test(m[0]) || /^[a-z-]+\(/.test(m[0])) {
        // An image or a counter: not text.
      } else if (m[0] === 'open-quote' || m[0] === 'close-quote') {
        text += m[0] === 'open-quote' ? '“' : '”';
        any = true;
      }
    }
    return any ? text : null;
  }

  function urlOf(text) {
    const m = String(text).match(/url\(\s*["']?(.*?)["']?\s*\)/);
    return m ? m[1] : null;
  }

  // Whether `el`'s `::before`/`::after` box is drawn: block pseudo-elements with a size or a
  // background, and inline ones with text.
  function pseudoOf(el, which, opacity) {
    const ps = getComputedStyle(el, which);
    const content = ps.content;
    if (!content || content === 'none' || content === 'normal') return null;
    if (ps.display === 'none' || ps.visibility !== 'visible') return null;
    const own = num(ps.opacity);
    if (own <= 0) return null;
    const text = contentText(content, el);
    const image = urlOf(ps.backgroundImage) || urlOf(content);
    const block = ps.display !== 'inline' && ps.display !== 'inline-block' && ps.display !== 'contents';
    return { ps, text, image, block, opacity: opacity * own };
  }

  // The containing block of an absolutely positioned descendant of `el`: the nearest
  // positioned ancestor (the slide itself is positioned in the print view).
  function containingBlock(el) {
    for (let e = el; e && e !== page; e = e.parentElement) {
      const pos = getComputedStyle(e).position;
      if (pos && pos !== 'static') return e;
    }
    return section || page;
  }

  // The box of a block pseudo-element of `el`, as a viewport rectangle: an absolutely
  // positioned one from its offsets, else in flow at the top (`::before`) or bottom
  // (`::after`) of `el`'s content box.
  function pseudoBox(el, which, pseudo) {
    const ps = pseudo.ps;
    const cs = getComputedStyle(el);
    const rect = el.getBoundingClientRect();
    const auto = (v) => v === 'auto' || v === '';
    if (ps.position === 'absolute' || ps.position === 'fixed') {
      const cb = containingBlock(el);
      const cbs = getComputedStyle(cb);
      const cbRect = cb.getBoundingClientRect();
      const left = cbRect.left + num(cbs.borderLeftWidth);
      const top = cbRect.top + num(cbs.borderTopWidth);
      const width = cbRect.width - num(cbs.borderLeftWidth) - num(cbs.borderRightWidth);
      const height = cbRect.height - num(cbs.borderTopWidth) - num(cbs.borderBottomWidth);
      let w = auto(ps.width) ? null : num(ps.width);
      let h = auto(ps.height) ? null : num(ps.height);
      let x;
      let y;
      if (!auto(ps.left)) x = left + num(ps.left);
      else if (!auto(ps.right) && w !== null) x = left + width - num(ps.right) - w;
      else x = left;
      if (!auto(ps.top)) y = top + num(ps.top);
      else if (!auto(ps.bottom) && h !== null) y = top + height - num(ps.bottom) - h;
      else y = top;
      if (w === null) w = !auto(ps.left) && !auto(ps.right) ? width - num(ps.left) - num(ps.right) : 0;
      if (h === null) h = !auto(ps.top) && !auto(ps.bottom) ? height - num(ps.top) - num(ps.bottom) : 0;
      x += num(ps.marginLeft);
      y += num(ps.marginTop);
      return { left: x, top: y, width: w, height: h };
    }
    const contentLeft = rect.left + num(cs.borderLeftWidth) + num(cs.paddingLeft);
    const contentRight = rect.right - num(cs.borderRightWidth) - num(cs.paddingRight);
    const contentTop = rect.top + num(cs.borderTopWidth) + num(cs.paddingTop);
    const contentBottom = rect.bottom - num(cs.borderBottomWidth) - num(cs.paddingBottom);
    const w = auto(ps.width) ? contentRight - contentLeft - num(ps.marginLeft) - num(ps.marginRight) : num(ps.width);
    const h = auto(ps.height) ? 0 : num(ps.height);
    const left = contentLeft + num(ps.marginLeft);
    const top = which === '::before' ? contentTop + num(ps.marginTop) : contentBottom - num(ps.marginBottom) - h;
    return { left, top, width: w, height: h };
  }

  // Emits the box of a block pseudo-element: a picture when it shows an image, a rectangle
  // when it has a fill or a border.
  function emitPseudoBlock(el, which, pseudo) {
    const box = pseudoBox(el, which, pseudo);
    const ps = pseudo.ps;
    const { part, frame } = frameOf(box);
    if (pseudo.image) {
      emitBackgroundImage(frame, part, pseudo.image, ps, pseudo.opacity, box);
      return;
    }
    const fill = fillOf(ps, pseudo.opacity);
    const line = lineOf(ps, pseudo.opacity);
    if (fill || line) emit({ kind: 'rect', frame, fill, line, radius: radiusOf(ps) }, part);
    if (pseudo.text && pseudo.text.trim()) {
      const style = runStyle(ps, pseudo.opacity, {});
      emit({
        kind: 'text', frame, insets: { l: 0, t: 0, r: 0, b: 0 }, fill: null, line: null, radius: null,
        wrap: true, title: false,
        paragraphs: [{ align: alignOf(ps), level: 0, bullet: null, marginLeft: 0, indent: 0,
          lineHeight: lineHeightOf(ps), spaceBefore: 0, runs: [{ ...style, text: pseudo.text }] }],
      }, part);
    }
  }

  // A CSS background image (`url(…)`, sized by `background-size`, centred by default) of a
  // box as a picture shape.
  function emitBackgroundImage(frame, part, url, cs, opacity, box) {
    const ref = images.push(url) - 1;
    const sizing = cs.backgroundSize || 'auto';
    const position = cs.backgroundPosition || '50% 50%';
    emit({
      kind: 'picture', frame, imageRef: ref, shot: shotOf(box), alt: '', alpha: opacity < 1 ? opacity : null,
      sizing, position, crop: null, fit: null,
    }, part);
  }

  // --- Text -----------------------------------------------------------------------------

  function alignOf(cs) {
    switch (cs.textAlign) {
      case 'center': return 'ctr';
      case 'right': return 'r';
      case 'end': return cs.direction === 'rtl' ? 'l' : 'r';
      case 'justify': return 'just';
      default: return cs.direction === 'rtl' ? 'r' : 'l';
    }
  }

  function lineHeightOf(cs) {
    const lh = cs.lineHeight;
    if (!lh || lh === 'normal') return null;
    const n = num(lh);
    return n > 0 ? round(n) : null;
  }

  // The run style of text rendered with the computed style `cs` (`inherit` carries the
  // decorations and the link of enclosing inline elements). A translucent colour (dimmed
  // code lines, muted text) is composited over `backdrop` when that is known: an opaque
  // colour looks the same everywhere, while not every reader honours alpha on text.
  function runStyle(cs, opacity, inherit, backdrop) {
    const decoration = (cs.textDecorationLine || '') + ' ' + (inherit.decoration || '');
    const weight = cs.fontWeight === 'bold' || cs.fontWeight === 'bolder' ? 700 : num(cs.fontWeight);
    let baseline = inherit.baseline || 0;
    const va = cs.verticalAlign;
    if (va === 'super' || va === 'text-top') baseline = 30000;
    else if (va === 'sub' || va === 'text-bottom') baseline = -25000;
    else if (/px$/.test(va)) {
      const px = num(va);
      if (px > 1) baseline = 30000;
      else if (px < -1) baseline = -25000;
    }
    let caps = null;
    if (cs.textTransform === 'uppercase') caps = 'all';
    else if (cs.fontVariantCaps === 'small-caps' || cs.fontVariantCaps === 'all-small-caps') caps = 'small';
    const spacing = cs.letterSpacing && cs.letterSpacing !== 'normal' ? round(num(cs.letterSpacing)) : 0;
    return {
      font: fontOf(cs.fontFamily),
      size: round(num(cs.fontSize)),
      bold: weight >= 600,
      italic: cs.fontStyle === 'italic' || cs.fontStyle === 'oblique',
      underline: /underline/.test(decoration),
      strike: /line-through/.test(decoration),
      color: solid(withOpacity(color(cs.color), opacity) || { r: 0, g: 0, b: 0, a: 1 }, backdrop),
      highlight: inherit.highlight || null,
      link: inherit.link || null,
      caps,
      baseline,
      spacing,
    };
  }

  // `c` made opaque over `backdrop` when that is given (else as it is).
  function solid(c, backdrop) {
    return c && backdrop && c.a < 1 ? over(c, backdrop) : c;
  }

  function sameStyle(a, b) {
    return a.font === b.font && a.size === b.size && a.bold === b.bold && a.italic === b.italic &&
      a.underline === b.underline && a.strike === b.strike && a.caps === b.caps && a.baseline === b.baseline &&
      a.spacing === b.spacing && a.link === b.link && JSON.stringify(a.color) === JSON.stringify(b.color) &&
      JSON.stringify(a.highlight) === JSON.stringify(b.highlight);
  }

  // Applies `text-transform` values PowerPoint has no attribute for.
  function transformed(text, cs) {
    if (cs.textTransform === 'lowercase') return text.toLowerCase();
    if (cs.textTransform === 'capitalize') return text.replace(/(^|\s)(\S)/g, (m, s, c) => s + c.toUpperCase());
    return text;
  }

  // Collects the runs of the inline content of `container` into `out` (pictures inside it
  // become shapes of their own). `ctx`: { opacity, pre, inherit, backdrop }.
  function collectRuns(container, out, ctx) {
    for (const node of container.childNodes) {
      if (node.nodeType === TEXT) {
        const parent = node.parentElement;
        if (!parent) continue;
        const cs = getComputedStyle(parent);
        let text = node.data;
        if (!ctx.pre) {
          text = text.replace(/\s+/g, ' ');
          if (!text) continue;
        } else if (!text) continue;
        out.push({ ...runStyle(cs, ctx.opacity, ctx.inherit, ctx.backdrop), text: transformed(text, cs) });
        continue;
      }
      if (node.nodeType !== ELEMENT) continue;
      const el = node;
      const cs = getComputedStyle(el);
      if (hidden(el, cs)) continue;
      const opacity = ctx.opacity * num(cs.opacity);
      if (opacity <= 0.001) continue;
      if (el.matches('br')) {
        out.push({ br: true });
        continue;
      }
      if (el.matches('img')) {
        emitImage(el, cs, opacity);
        continue;
      }
      if (el.matches('.katex')) {
        mathRuns(el, out, ctx);
        continue;
      }
      if (el.matches(RASTER)) {
        emitShot(el, cs, opacity);
        continue;
      }
      if (el.matches('input[type="checkbox"]')) {
        const style = runStyle(cs, opacity, ctx.inherit, ctx.backdrop);
        const c = withOpacity(color(cs.borderTopColor), opacity);
        if (c) style.color = solid(c, ctx.backdrop);
        out.push({ ...style, text: (el.checked ? '☑' : '☐') + ' ' });
        continue;
      }
      const inherit = { ...ctx.inherit };
      const decoration = cs.textDecorationLine;
      if (decoration && decoration !== 'none') inherit.decoration = (inherit.decoration || '') + ' ' + decoration;
      if (el.matches('a[href]')) {
        const href = el.getAttribute('href') || '';
        if (/^(https?:|mailto:|ftp:)/i.test(href)) inherit.link = href;
      }
      const bg = withOpacity(color(cs.backgroundColor), opacity);
      if (bg) inherit.highlight = over(bg, ctx.backdrop);
      const inner = { ...ctx, opacity, inherit, pre: ctx.pre || /^pre/.test(cs.whiteSpace) };
      const before = pseudoOf(el, '::before', opacity);
      if (before && before.text && !before.block) {
        out.push({ ...runStyle(before.ps, opacity, inherit, ctx.backdrop), text: spaced(before.text, before.ps) });
      }
      collectRuns(el, out, inner);
      const after = pseudoOf(el, '::after', opacity);
      if (after && after.text && !after.block) {
        out.push({ ...runStyle(after.ps, opacity, inherit, ctx.backdrop), text: spaced(after.text, after.ps) });
      }
    }
  }

  // Pseudo text with its horizontal margins as spaces.
  function spaced(text, ps) {
    const left = num(ps.marginLeft) > 2 ? ' ' : '';
    const right = num(ps.marginRight) > 2 ? ' ' : '';
    return left + text + right;
  }

  // Runs merged where neighbours share a style, with blanks collapsed at run boundaries.
  function mergeRuns(runs, pre) {
    const out = [];
    for (const run of runs) {
      if (run.br) {
        out.push(run);
        continue;
      }
      const last = out[out.length - 1];
      if (last && !last.br && sameStyle(last, run)) {
        last.text += run.text;
      } else {
        out.push({ ...run });
      }
    }
    if (pre) return out;
    // Collapse the doubled spaces where runs meet, and trim at line boundaries.
    for (let i = 0; i < out.length; i++) {
      const run = out[i];
      if (run.br) continue;
      const prev = out[i - 1];
      if ((!prev || prev.br) && run.text) run.text = run.text.replace(/^\s+/, '');
      if (prev && !prev.br && prev.text.endsWith(' ') && run.text.startsWith(' ')) run.text = run.text.replace(/^\s+/, '');
      const next = out[i + 1];
      if ((!next || next.br) && run.text) run.text = run.text.replace(/\s+$/, '');
    }
    return out.filter((run) => run.br || run.text);
  }

  // Splits runs into paragraphs at new lines (`white-space: pre`).
  function splitLines(runs) {
    const paragraphs = [[]];
    for (const run of runs) {
      if (run.br) {
        paragraphs.push([]);
        continue;
      }
      const lines = run.text.split('\n');
      lines.forEach((line, i) => {
        if (i > 0) paragraphs.push([]);
        if (line) paragraphs[paragraphs.length - 1].push({ ...run, text: line });
      });
    }
    // A trailing new line ends the last line; it is no empty line.
    if (paragraphs.length > 1 && paragraphs[paragraphs.length - 1].length === 0) paragraphs.pop();
    return paragraphs;
  }

  function paragraphOf(cs, runs, extra) {
    return {
      align: alignOf(cs),
      level: 0,
      bullet: null,
      marginLeft: 0,
      indent: round(num(cs.textIndent)),
      lineHeight: lineHeightOf(cs),
      spaceBefore: 0,
      runs,
      ...extra,
    };
  }

  // The text shape of a block element whose content is inline (a heading, a paragraph, a
  // code block, a table cell's content…), with its box's fill, border and padding.
  function textShape(el, cs, opacity, extra) {
    const pre = /^pre/.test(cs.whiteSpace);
    const ctx = { opacity, pre, inherit: {}, backdrop: backdropOf(el) };
    let paragraphs;
    const table = el.querySelector(':scope > table.hljs-ln, :scope > * > table.hljs-ln');
    if (table) {
      paragraphs = [];
      for (const row of table.querySelectorAll('tr')) {
        const rs = getComputedStyle(row);
        if (hidden(row, rs)) continue;
        const rowOpacity = opacity * num(rs.opacity);
        const runs = [];
        const number = row.querySelector('.hljs-ln-numbers .hljs-ln-n, .hljs-ln-numbers');
        if (number) {
          const ns = getComputedStyle(number);
          const text = (number.getAttribute('data-line-number') || contentText(getComputedStyle(number, '::before').content, number) || '').trim();
          if (text) runs.push({ ...runStyle(ns, rowOpacity * num(ns.opacity), {}, ctx.backdrop), text: text.padStart(3) + '  ' });
        }
        const code = row.querySelector('.hljs-ln-code') || row;
        collectRuns(code, runs, { ...ctx, opacity: rowOpacity, pre: true });
        paragraphs.push(paragraphOf(cs, mergeRuns(runs, true), { indent: 0 }));
      }
    } else {
      const runs = [];
      collectRuns(el, runs, ctx);
      const merged = mergeRuns(runs, pre);
      paragraphs = pre ? splitLines(merged).map((line) => paragraphOf(cs, line, { indent: 0 })) : [paragraphOf(cs, merged)];
    }
    if (paragraphs.every((p) => p.runs.length === 0)) return null;
    const rect = el.getBoundingClientRect();
    const { part, frame } = frameOf(rect);
    return {
      shape: {
        kind: 'text',
        frame,
        insets: {
          l: round(num(cs.paddingLeft) + num(cs.borderLeftWidth)),
          t: round(num(cs.paddingTop) + num(cs.borderTopWidth)),
          r: round(num(cs.paddingRight) + num(cs.borderRightWidth)),
          b: round(num(cs.paddingBottom) + num(cs.borderBottomWidth)),
        },
        fill: fillOf(cs, opacity),
        line: lineOf(cs, opacity),
        radius: radiusOf(cs),
        wrap: !(cs.whiteSpace === 'pre' || cs.whiteSpace === 'nowrap'),
        title: false,
        paragraphs,
        ...extra,
      },
      part,
    };
  }

  function emitTextBlock(el, cs, opacity) {
    const made = textShape(el, cs, opacity, {});
    if (!made) return;
    if (!titleTaken && el.matches('h1, h2') && made.part === 0) {
      made.shape.title = true;
      titleTaken = true;
    }
    emitBoxBorders(el, cs, opacity);
    emit(made.shape, made.part);
  }

  // --- Math -----------------------------------------------------------------------------

  // Inline KaTeX math as runs: its MathML, linearised (superscripts and subscripts as
  // baseline shifts, fractions as `a/b`, roots as `√`, accents as combining characters).
  function mathRuns(katex, out, ctx) {
    const math = katex.querySelector('.katex-mathml math');
    const parent = katex.parentElement || katex;
    const base = runStyle(getComputedStyle(parent), ctx.opacity, ctx.inherit, ctx.backdrop);
    if (!math) {
      const text = (katex.textContent || '').trim();
      if (text) out.push({ ...base, text });
      return;
    }
    const emitText = (text, style) => {
      text = text.replace(/[⁡-⁤​]/g, '');
      if (text) out.push({ ...base, ...style, text });
    };
    const walk = (node, style) => {
      if (node.nodeType === TEXT) {
        emitText(node.data, style);
        return;
      }
      if (node.nodeType !== ELEMENT) return;
      const name = node.localName;
      const kids = Array.from(node.childNodes).filter((n) => n.nodeType === ELEMENT || (n.nodeType === TEXT && n.data.trim()));
      switch (name) {
        case 'annotation':
        case 'annotation-xml':
          return;
        case 'mi': {
          const text = node.textContent;
          const italic = node.getAttribute('mathvariant') !== 'normal' && [...text].length === 1;
          emitText(text, { ...style, italic });
          return;
        }
        case 'mn':
        case 'mo':
        case 'mtext':
        case 'ms':
          emitText(node.textContent, { ...style, italic: false });
          return;
        case 'mspace':
          emitText(' ', style);
          return;
        case 'msup':
          walk(kids[0], style);
          if (kids[1]) walk(kids[1], { ...style, baseline: 30000 });
          return;
        case 'msub':
          walk(kids[0], style);
          if (kids[1]) walk(kids[1], { ...style, baseline: -25000 });
          return;
        case 'msubsup':
          walk(kids[0], style);
          if (kids[1]) walk(kids[1], { ...style, baseline: -25000 });
          if (kids[2]) walk(kids[2], { ...style, baseline: 30000 });
          return;
        case 'mfrac': {
          const simple = (k) => k && (k.localName === 'mi' || k.localName === 'mn') && k.textContent.length <= 2;
          if (!simple(kids[0])) emitText('(', style);
          walk(kids[0], style);
          if (!simple(kids[0])) emitText(')', style);
          emitText('/', style);
          if (!simple(kids[1])) emitText('(', style);
          walk(kids[1], style);
          if (!simple(kids[1])) emitText(')', style);
          return;
        }
        case 'msqrt':
          emitText('√(', style);
          kids.forEach((k) => walk(k, style));
          emitText(')', style);
          return;
        case 'mroot':
          if (kids[1]) walk(kids[1], { ...style, baseline: 30000 });
          emitText('√(', style);
          walk(kids[0], style);
          emitText(')', style);
          return;
        case 'mover':
        case 'munder':
        case 'munderover': {
          const accent = kids[1] ? kids[1].textContent.trim() : '';
          const combining = ACCENTS[accent];
          if (name === 'mover' && combining && kids[0] && [...kids[0].textContent.trim()].length === 1) {
            emitText(kids[0].textContent.trim() + combining, { ...style, italic: kids[0].localName === 'mi' });
            return;
          }
          walk(kids[0], style);
          for (const k of kids.slice(1)) walk(k, { ...style, baseline: name === 'munder' ? -25000 : 30000 });
          return;
        }
        case 'mtable':
          for (const row of kids) {
            if (row !== kids[0]) emitText('; ', style);
            walk(row, style);
          }
          return;
        case 'mtr':
          kids.forEach((k, i) => {
            if (i > 0) emitText(', ', style);
            walk(k, style);
          });
          return;
        case 'mphantom':
          return;
        default:
          kids.forEach((k) => walk(k, style));
      }
    };
    walk(math, {});
  }

  // --- Pictures -------------------------------------------------------------------------

  // How a replaced element's content sits in its box (`object-fit`), as a crop (the part of
  // the image shown, fractions of its size) or a fit (margins inside the box, fractions of
  // the box).
  function objectFit(cs, boxW, boxH, naturalW, naturalH) {
    const fit = cs.objectFit || 'fill';
    if (!naturalW || !naturalH || fit === 'fill' || boxW <= 0 || boxH <= 0) return {};
    const scaleContain = Math.min(boxW / naturalW, boxH / naturalH);
    const scaleCover = Math.max(boxW / naturalW, boxH / naturalH);
    const some = (e) => Object.values(e).some((v) => v > 0.0005);
    if (fit === 'contain' || (fit === 'scale-down' && scaleContain < 1) || (fit === 'none' && scaleContain >= 1)) {
      const s = fit === 'none' ? 1 : scaleContain;
      const w = naturalW * s;
      const h = naturalH * s;
      const edges = { l: (boxW - w) / 2 / boxW, r: (boxW - w) / 2 / boxW, t: (boxH - h) / 2 / boxH, b: (boxH - h) / 2 / boxH };
      return some(edges) ? { fit: edges } : {};
    }
    const s = fit === 'none' ? 1 : scaleCover;
    const shownW = boxW / s;
    const shownH = boxH / s;
    const edges = { l: (naturalW - shownW) / 2 / naturalW, r: (naturalW - shownW) / 2 / naturalW, t: (naturalH - shownH) / 2 / naturalH, b: (naturalH - shownH) / 2 / naturalH };
    return some(edges) ? { crop: edges } : {};
  }

  function emitImage(img, cs, opacity) {
    const rect = img.getBoundingClientRect();
    const { part, frame } = frameOf(rect);
    const ref = images.push(img) - 1;
    const fit = objectFit(cs, rect.width, rect.height, img.naturalWidth, img.naturalHeight);
    emit({
      kind: 'picture', frame, imageRef: ref, shot: shotOf(rect), alt: img.getAttribute('alt') || '',
      alpha: opacity < 1 ? opacity : null, crop: fit.crop || null, fit: fit.fit || null, sizing: null, position: null,
    }, part);
  }

  // A photograph of `el` (what PowerPoint cannot hold natively).
  function emitShot(el, cs, opacity) {
    const rect = el.getBoundingClientRect();
    const { part, frame } = frameOf(rect);
    emit({
      kind: 'picture', frame, imageRef: null, shot: shotOf(rect), alt: el.getAttribute('alt') || el.getAttribute('title') || '',
      alpha: opacity < 1 ? opacity : null, crop: null, fit: null, sizing: null, position: null,
    }, part);
  }

  // --- Boxes ----------------------------------------------------------------------------

  // The decoration of an element's own box: its background (colour, gradient or image) and
  // border, as a rectangle or a picture behind its content. Borders that differ per side
  // become a thin rectangle per side.
  function emitBox(el, cs, opacity) {
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const { part, frame } = frameOf(rect);
    const image = urlOf(cs.backgroundImage);
    const fill = image ? (withOpacity(color(cs.backgroundColor), opacity) ? { color: withOpacity(color(cs.backgroundColor), opacity) } : null) : fillOf(cs, opacity);
    const line = lineOf(cs, opacity);
    if (fill || line) emit({ kind: 'rect', frame, fill, line, radius: radiusOf(cs) }, part);
    if (image) emitBackgroundImage(frame, part, image, cs, opacity, rect);
    if (!line) emitBoxBorders(el, cs, opacity);
  }

  // The sides of a border that is not the same on every side, each as a rectangle.
  function emitBoxBorders(el, cs, opacity) {
    if (lineOf(cs, opacity)) return;
    const rect = el.getBoundingClientRect();
    const sides = {
      Top: (b) => ({ left: rect.left, top: rect.top, width: rect.width, height: b.width }),
      Bottom: (b) => ({ left: rect.left, top: rect.bottom - b.width, width: rect.width, height: b.width }),
      Left: (b) => ({ left: rect.left, top: rect.top, width: b.width, height: rect.height }),
      Right: (b) => ({ left: rect.right - b.width, top: rect.top, width: b.width, height: rect.height }),
    };
    for (const [side, box] of Object.entries(sides)) {
      const b = borderSide(cs, side, opacity);
      if (!b) continue;
      const { part, frame } = frameOf(box(b));
      emit({ kind: 'rect', frame, fill: { color: b.color }, line: null, radius: null }, part);
    }
  }

  // --- Lists ----------------------------------------------------------------------------

  function bulletOf(list, li, lcs) {
    const type = lcs.listStyleType;
    if (li.querySelector(':scope > input[type="checkbox"]') || type === 'none') return null;
    const marker = getComputedStyle(li, '::marker');
    const c = color(marker.color) || color(lcs.color);
    const custom = contentText(marker.content, li);
    if (custom !== null && custom.trim()) return { char: custom.trim(), color: c };
    if (list.matches('ol')) {
      const start = parseInt(list.getAttribute('start') || '1', 10);
      return { scheme: NUMBERING[type] || 'arabicPeriod', start: Number.isFinite(start) ? start : 1, color: c };
    }
    const quoted = type.match(/^["'](.*)["']$/);
    return { char: quoted ? quoted[1] : BULLETS[type] || '•', color: c };
  }

  // Where a list's markers sit: inside its left padding, else in its left margin (outside
  // its box, as the built-in theme has them), else about 1.1 em before its items.
  function markerHang(ucs) {
    const padding = num(ucs.paddingLeft) + num(ucs.borderLeftWidth);
    if (padding > 0) return { inside: padding, outside: 0 };
    const margin = num(ucs.marginLeft);
    if (margin > 0) return { inside: 0, outside: margin };
    return { inside: 0, outside: num(ucs.fontSize) * 1.1 };
  }

  // One shape for a whole list (nested lists included), widened to the left by the markers
  // that hang outside it: one paragraph per item, with the item's bullet or number and the
  // indentation the browser gave it, and a paragraph for each further block of text inside
  // an item. Other blocks inside items (code, pictures, tables) become shapes of their own.
  function emitList(list, cs, opacity) {
    const rect = list.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const extend = markerHang(cs).outside;
    const left = rect.left - extend;
    const { part, frame } = frameOf({ left, top: rect.top, width: rect.width + extend, height: rect.height });
    const paragraphs = [];
    let previousBottom = null;
    // Adds a paragraph that occupies `top` to `bottom` (viewport pixels): the gap to the
    // paragraph before becomes its space before.
    const push = (top, bottom, paragraph) => {
      paragraph.spaceBefore = previousBottom === null ? 0 : round(Math.max(0, top - previousBottom));
      paragraphs.push(paragraph);
      previousBottom = Math.max(previousBottom === null ? bottom : previousBottom, bottom);
    };
    const addItems = (ul, level, ctxOpacity) => {
      const ucs = getComputedStyle(ul);
      const hang = markerHang(ucs);
      const room = hang.inside || hang.outside;
      for (const li of ul.children) {
        if (!li.matches('li')) continue;
        const lcs = getComputedStyle(li);
        if (hidden(li, lcs)) continue;
        const liOpacity = ctxOpacity * num(lcs.opacity);
        if (liOpacity <= 0.001) continue;
        const liRect = li.getBoundingClientRect();
        const textLeft = liRect.left + num(lcs.borderLeftWidth) + num(lcs.paddingLeft);
        const marginLeft = round(Math.max(0, textLeft - left));
        const bullet = bulletOf(ul, li, lcs);
        // The marker ends a little before the text: a bullet needs about 0.65 em, a
        // number about 1 em, within the room the list leaves for it.
        const indent = round(-Math.min(room, num(lcs.fontSize) * (bullet && bullet.scheme ? 1.0 : 0.65)));
        const ctx = { opacity: liOpacity, pre: false, inherit: {}, backdrop: backdropOf(li) };
        const itemParagraph = (ccs, runs, first) => paragraphOf(ccs, runs, {
          level: Math.min(8, level), bullet: first ? bullet : null, marginLeft, indent: first && bullet ? indent : 0,
        });
        let first = true;
        let inline = [];
        const flushInline = () => {
          if (inline.length === 0) return;
          const runs = [];
          // An anonymous box has no element of its own to style: a text node takes the
          // style of its parent, the item.
          for (const node of inline) {
            if (node.nodeType === TEXT) {
              const text = node.data.replace(/\s+/g, ' ');
              if (text) runs.push({ ...runStyle(lcs, liOpacity, {}, ctx.backdrop), text: transformed(text, lcs) });
            } else {
              collectRuns({ childNodes: [node] }, runs, ctx);
            }
          }
          const range = document.createRange();
          range.setStartBefore(inline[0]);
          range.setEndAfter(inline[inline.length - 1]);
          const box = range.getBoundingClientRect();
          inline = [];
          const merged = mergeRuns(runs, false);
          if (merged.length === 0 && !first) return;
          push(box.height > 0 ? box.top : liRect.top, box.height > 0 ? box.bottom : liRect.top, itemParagraph(lcs, merged, first));
          first = false;
        };
        for (const node of li.childNodes) {
          if (node.nodeType === TEXT) {
            if (node.data.trim() || inline.length) inline.push(node);
            continue;
          }
          if (node.nodeType !== ELEMENT) continue;
          const ccs = getComputedStyle(node);
          if (hidden(node, ccs)) continue;
          if (isInline(ccs) && !node.matches('img, svg, canvas, video, iframe, object, embed, .katex-display')) {
            inline.push(node);
            continue;
          }
          flushInline();
          if (node.matches('ul, ol')) {
            if (first) {
              // An item whose own text is empty still shows its bullet.
              push(liRect.top, liRect.top, itemParagraph(lcs, [], true));
              first = false;
            }
            addItems(node, level + 1, liOpacity);
            continue;
          }
          if (node.matches('p, h1, h2, h3, h4, h5, h6, figcaption, dt, dd') && !node.querySelector('img, svg, table, pre, ul, ol')) {
            const runs = [];
            collectRuns(node, runs, { ...ctx, pre: /^pre/.test(ccs.whiteSpace) });
            const nrect = node.getBoundingClientRect();
            push(nrect.top, nrect.bottom, itemParagraph(ccs, mergeRuns(runs, false), first));
            first = false;
            continue;
          }
          walk(node, { opacity: liOpacity });
        }
        flushInline();
        if (first) push(liRect.top, liRect.bottom, itemParagraph(lcs, [], true));
      }
    };
    addItems(list, 0, opacity);
    if (paragraphs.length === 0) return;
    paragraphs[0].spaceBefore = 0;
    emit({
      kind: 'text', frame, insets: { l: 0, t: round(num(cs.paddingTop)), r: 0, b: 0 }, fill: fillOf(cs, opacity),
      line: lineOf(cs, opacity), radius: radiusOf(cs), wrap: true, title: false, paragraphs,
    }, part);
  }

  // --- Tables ---------------------------------------------------------------------------

  function emitTable(table, cs, opacity) {
    const rect = table.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const rowsIn = Array.from(table.querySelectorAll(':scope > thead > tr, :scope > tbody > tr, :scope > tfoot > tr, :scope > tr'));
    if (rowsIn.length === 0) return;
    // The grid: which cell covers each (row, column), spans included.
    const grid = [];
    const spans = new Map();
    rowsIn.forEach((tr, r) => {
      grid[r] = grid[r] || [];
      let c = 0;
      for (const cell of tr.children) {
        if (!cell.matches('td, th')) continue;
        while (grid[r][c]) c++;
        const colSpan = Math.max(1, parseInt(cell.getAttribute('colspan') || '1', 10) || 1);
        const rowSpan = Math.max(1, parseInt(cell.getAttribute('rowspan') || '1', 10) || 1);
        spans.set(cell, { r, c, colSpan, rowSpan });
        for (let i = 0; i < rowSpan; i++) {
          grid[r + i] = grid[r + i] || [];
          for (let j = 0; j < colSpan; j++) grid[r + i][c + j] = cell;
        }
        c += colSpan;
      }
    });
    const columnCount = Math.max(...grid.map((row) => row.length));
    if (!Number.isFinite(columnCount) || columnCount === 0) return;
    // Column widths from the cells that span one column, else shared evenly.
    const widths = new Array(columnCount).fill(0);
    for (const [cell, span] of spans) {
      if (span.colSpan === 1) widths[span.c] = Math.max(widths[span.c], cell.getBoundingClientRect().width);
    }
    const unknown = widths.filter((w) => w === 0).length;
    if (unknown) {
      const known = widths.reduce((a, b) => a + b, 0);
      const share = Math.max(10, (rect.width - known) / unknown);
      for (let i = 0; i < widths.length; i++) if (widths[i] === 0) widths[i] = share;
    }
    const rows = [];
    const backdrop = backdropOf(table);
    const solidFill = (fill) => (fill && fill.color ? { color: over(fill.color, backdrop) } : fill);
    const solidLine = (line) => (line ? { ...line, color: over(line.color, backdrop) } : null);
    for (let r = 0; r < grid.length; r++) {
      const tr = rowsIn[r];
      const trRect = tr.getBoundingClientRect();
      const trs = getComputedStyle(tr);
      const rowOpacity = opacity * num(trs.opacity);
      const cells = [];
      for (let c = 0; c < columnCount; c++) {
        const cell = grid[r][c];
        if (!cell) {
          cells.push({ paragraphs: [], insets: { l: 0, t: 0, r: 0, b: 0 }, fill: null, borders: {}, gridSpan: 1, rowSpan: 1, hMerge: false, vMerge: false });
          continue;
        }
        const span = spans.get(cell);
        const ccs = getComputedStyle(cell);
        const cellOpacity = rowOpacity * num(ccs.opacity);
        const base = { gridSpan: 1, rowSpan: 1, hMerge: false, vMerge: false };
        if (span.r !== r || span.c !== c) {
          cells.push({ paragraphs: [], insets: { l: 0, t: 0, r: 0, b: 0 }, fill: null, borders: {}, ...base, hMerge: span.c !== c, vMerge: span.r !== r });
          continue;
        }
        const runs = [];
        collectRuns(cell, runs, { opacity: cellOpacity, pre: /^pre/.test(ccs.whiteSpace), inherit: {}, backdrop });
        const merged = mergeRuns(runs, /^pre/.test(ccs.whiteSpace));
        const paragraphs = /^pre/.test(ccs.whiteSpace) ? splitLines(merged).map((line) => paragraphOf(ccs, line, { indent: 0 })) : [paragraphOf(ccs, merged)];
        // Row fills (zebra stripes) show through transparent cells.
        const fill = solidFill(fillOf(ccs, cellOpacity) || fillOf(trs, rowOpacity));
        cells.push({
          paragraphs,
          insets: { l: round(num(ccs.paddingLeft)), t: round(num(ccs.paddingTop)), r: round(num(ccs.paddingRight)), b: round(num(ccs.paddingBottom)) },
          fill,
          borders: {
            l: solidLine(borderSide(ccs, 'Left', cellOpacity)), t: solidLine(borderSide(ccs, 'Top', cellOpacity)),
            r: solidLine(borderSide(ccs, 'Right', cellOpacity)), b: solidLine(borderSide(ccs, 'Bottom', cellOpacity)),
          },
          anchor: ccs.verticalAlign === 'middle' ? 'ctr' : ccs.verticalAlign === 'bottom' ? 'b' : 't',
          ...base,
          gridSpan: span.colSpan,
          rowSpan: span.rowSpan,
        });
      }
      rows.push({ height: round(trRect.height), cells });
    }
    const { part, frame } = frameOf(rect);
    emit({ kind: 'table', frame, columns: widths.map(round), rows }, part);
  }

  // --- The walk -------------------------------------------------------------------------

  function isInline(cs) {
    return /^inline/.test(cs.display) || cs.display === 'contents';
  }

  // Whether `el` holds only inline content (text, spans, pictures) and so becomes one text
  // shape.
  function isTextBlock(el, cs) {
    if (isInline(cs)) return false;
    if (el.matches('ul, ol, table, pre:has(> code), figure, blockquote, details')) return false;
    if (el.querySelector(':scope > table.hljs-ln, :scope > code > table.hljs-ln')) return true;
    let text = false;
    for (const node of el.childNodes) {
      if (node.nodeType === TEXT) {
        if (node.data.trim()) text = true;
        continue;
      }
      if (node.nodeType !== ELEMENT) continue;
      const ccs = getComputedStyle(node);
      if (ccs.display === 'none') continue;
      if (node.matches('img, br, svg, canvas, input, .katex') || isInline(ccs)) {
        text = true;
        continue;
      }
      return false;
    }
    return text || el.matches('h1, h2, h3, h4, h5, h6, p, pre, code, figcaption, dt, dd, summary, caption, legend, label');
  }

  // Emits a text shape for a run of inline siblings inside a container that also holds
  // blocks (mixed content), from the box the run occupies.
  function emitInlineRun(container, nodes, opacity) {
    const range = document.createRange();
    range.setStartBefore(nodes[0]);
    range.setEndAfter(nodes[nodes.length - 1]);
    const rect = range.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const cs = getComputedStyle(container);
    const ctx = { opacity, pre: /^pre/.test(cs.whiteSpace), inherit: {}, backdrop: backdropOf(container) };
    const runs = [];
    for (const node of nodes) {
      if (node.nodeType === TEXT) {
        const text = ctx.pre ? node.data : node.data.replace(/\s+/g, ' ');
        if (text) runs.push({ ...runStyle(cs, opacity, {}, ctx.backdrop), text: transformed(text, cs) });
      } else {
        collectRuns({ childNodes: [node] }, runs, ctx);
      }
    }
    const merged = mergeRuns(runs, ctx.pre);
    const paragraphs = ctx.pre ? splitLines(merged).map((line) => paragraphOf(cs, line, { indent: 0 })) : [paragraphOf(cs, merged)];
    if (paragraphs.every((p) => p.runs.length === 0)) return;
    const { part, frame } = frameOf(rect);
    emit({ kind: 'text', frame, insets: { l: 0, t: 0, r: 0, b: 0 }, fill: null, line: null, radius: null, wrap: true, title: false, paragraphs }, part);
  }

  function walk(el, ctx) {
    const cs = getComputedStyle(el);
    if (hidden(el, cs)) return;
    const opacity = ctx.opacity * num(cs.opacity);
    if (opacity <= 0.001) return;
    if (el.matches('img')) {
      emitImage(el, cs, opacity);
      return;
    }
    if (el.matches(RASTER)) {
      emitShot(el, cs, opacity);
      return;
    }
    if (el.matches('.katex')) {
      // Inline math outside a paragraph: as text.
      const runs = [];
      mathRuns(el, runs, { opacity, pre: false, inherit: {}, backdrop: backdropOf(el) });
      const { part, frame } = frameOf(el.getBoundingClientRect());
      emit({ kind: 'text', frame, insets: { l: 0, t: 0, r: 0, b: 0 }, fill: null, line: null, radius: null, wrap: true, title: false, paragraphs: [paragraphOf(cs, mergeRuns(runs, false))] }, part);
      return;
    }
    if (el.matches('table') && !el.matches('.hljs-ln')) {
      emitTable(el, cs, opacity);
      return;
    }
    if (el.matches('ul, ol')) {
      emitList(el, cs, opacity);
      return;
    }
    const before = pseudoOf(el, '::before', opacity);
    const after = pseudoOf(el, '::after', opacity);
    if (isTextBlock(el, cs)) {
      if (before && before.block) emitPseudoBlock(el, '::before', before);
      emitTextBlock(el, cs, opacity);
      if (after && after.block) emitPseudoBlock(el, '::after', after);
      return;
    }
    emitBox(el, cs, opacity);
    if (before && before.block) emitPseudoBlock(el, '::before', before);
    else if (before && before.text && before.text.trim()) emitPseudoInline(el, before, '::before');
    let inline = [];
    const flush = () => {
      if (inline.length) emitInlineRun(el, inline, opacity);
      inline = [];
    };
    for (const node of el.childNodes) {
      if (node.nodeType === TEXT) {
        if (node.data.trim() || inline.length) inline.push(node);
        continue;
      }
      if (node.nodeType !== ELEMENT) continue;
      const ccs = getComputedStyle(node);
      if (hidden(node, ccs)) continue;
      if (isInline(ccs) && !node.matches('img, svg, canvas, video, iframe, object, embed, .katex-display')) {
        inline.push(node);
        continue;
      }
      flush();
      walk(node, { opacity });
    }
    flush();
    if (after && after.block) emitPseudoBlock(el, '::after', after);
    else if (after && after.text && after.text.trim()) emitPseudoInline(el, after, '::after');
  }

  // Inline pseudo text of a container that holds blocks: a small text shape at the start
  // or the end of the container's content box.
  function emitPseudoInline(el, pseudo, which) {
    const rect = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    const style = runStyle(pseudo.ps, pseudo.opacity, {});
    const height = lineHeightOf(pseudo.ps) || style.size * 1.3;
    const top = which === '::before' ? rect.top + num(cs.paddingTop) : rect.bottom - num(cs.paddingBottom) - height;
    const { part, frame } = frameOf({ left: rect.left + num(cs.paddingLeft), top, width: rect.width - num(cs.paddingLeft) - num(cs.paddingRight), height });
    emit({ kind: 'text', frame, insets: { l: 0, t: 0, r: 0, b: 0 }, fill: null, line: null, radius: null, wrap: true, title: false,
      paragraphs: [paragraphOf(pseudo.ps, [{ ...style, text: pseudo.text.trim() }])] }, part);
  }

  // --- Background -----------------------------------------------------------------------

  function background() {
    const bg = { color: null, imageRef: null, shot: null, alpha: null, sizing: null, position: null, shotOnly: false };
    // The slide area of the page's first printed page, in document pixels.
    const slideArea = {
      x: round(pageRect.left + scrollX + marginX),
      y: round(pageRect.top + scrollY + marginY),
      w: round(size.width),
      h: round(size.height),
    };
    const pageColor = color(getComputedStyle(page).backgroundColor);
    if (pageColor) bg.color = over(pageColor, { r: 255, g: 255, b: 255, a: 1 });
    const el = page.querySelector(':scope > .slide-background');
    if (!el) return bg;
    const cs = getComputedStyle(el);
    const own = color(cs.backgroundColor);
    if (own) bg.color = over(own, bg.color || { r: 255, g: 255, b: 255, a: 1 });
    const layers = [el, ...el.querySelectorAll('.slide-background-content')];
    for (const layer of layers) {
      const ls = getComputedStyle(layer);
      if (ls.display === 'none') continue;
      const opacity = num(ls.opacity);
      if (layer !== el) {
        const c = color(ls.backgroundColor);
        if (c) bg.color = over(withOpacity(c, opacity), bg.color || { r: 255, g: 255, b: 255, a: 1 });
      }
      const url = urlOf(ls.backgroundImage);
      if (url) {
        bg.imageRef = images.push(url) - 1;
        bg.alpha = opacity < 1 ? opacity : null;
        bg.sizing = ls.backgroundSize || 'auto';
        bg.position = ls.backgroundPosition || '50% 50%';
        bg.shot = slideArea;
        break;
      }
      if (gradient(ls.backgroundImage) || layer.querySelector('video, iframe')) {
        // A gradient or a medium: a photograph of the background alone.
        bg.shot = slideArea;
        bg.alpha = null;
        bg.shotOnly = true;
        break;
      }
    }
    return bg;
  }

  scene.background = background();
  if (section) walk(section, { opacity: 1 });
  scene.images = images.length;
  return scene;
})
