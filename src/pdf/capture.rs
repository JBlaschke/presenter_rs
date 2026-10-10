//! A deck's pages as PowerPoint shapes (`presenter export pptx`, `docs/spec/cli.md` export).
//!
//! [`html_to_pages`] opens a standalone deck in reveal.js' print view with headless
//! Chrome, exactly as the PDF export does (module docs of [`crate::pdf`], steps 1–3 and 5),
//! and instead of printing converts every page of that view into shapes: a slide, a
//! fragment step (with reveal's default `pdfSeparateFragments`, reveal clones a slide's
//! page once per step, so the steps build the slide up one page at a time) or one page of
//! a slide taller than a page (`pdfMaxPagesPerSlide`). The pages are the PDF's pages, as
//! editable slides. With each comes its place in the deck (which slide, step and part),
//! the slide's heading and its speaker notes as text ([`Page`]).
//!
//! # How a page becomes shapes
//!
//! Once reveal.js has laid the pages out ([`crate::pdf::READY_SCRIPT`]), [`PAGES_SCRIPT`]
//! lists them: it computes the page size as reveal's print view does (the slide size, plus
//! the `margin` on each side), sets every `.pdf-page` to whole pages tall (reveal makes them
//! `pdfPageHeightOffset`, one pixel by default, short of that, which suits printing), and
//! records where each starts, how many pages tall it is, whether it is a slide's own page
//! or a clone of it for a fragment step (reveal sets a `slideBackgroundElement` property on
//! the slide element itself, and `cloneNode` copies no properties), the slide's first
//! heading, and its notes (reveal's `getSlideNotes`, as `innerText` of an element off
//! screen, KaTeX's hidden MathML removed, so that a line of text is a line of notes).
//!
//! Then, page by page, the scene extractor (`scene.js`, embedded as [`SCENE_SCRIPT`]) walks
//! the page's DOM and returns its shapes in slide coordinates: text boxes for headings,
//! paragraphs, lists (one box per list, a paragraph per item with its bullet or number and
//! indentation), code blocks (a paragraph per line, with the highlighting colours) and any
//! other block of text, with the font family, size, weight, style, decorations, colour,
//! highlight, hyperlink and letter spacing of every run; rectangles for the backgrounds and
//! borders of boxes (quotes, callouts, code blocks) and for the decorations themes draw as
//! `::before`/`::after` pseudo-elements; tables for tables; pictures for images (their own
//! bytes, when PNG, JPEG or GIF) and for what PowerPoint cannot hold as native content
//! (SVG, canvases, videos, iframes, display math, gradients), which are photographed with
//! `Page.captureScreenshot` at `scale` device pixels per CSS pixel; and the slide's
//! background colour and picture. Inline math becomes text (its MathML linearised).
//! Elements reveal hides on a page, fragments that are not shown yet among them, are left
//! out, and so are reveal's slide number and the speaker notes (they go to the notes pane).
//!
//! Photographs are taken with the viewport one page large, scrolled to the page: an
//! element's with the backgrounds hidden and the page transparent (so the picture carries
//! no background), the slide background's with the slide's content hidden.
//!
//! The export's `timeout` covers starting Chrome and laying the deck out; once the pages
//! are known, the deadline moves by [`PER_PAGE`] for each page to convert.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Cdp, Failure, evaluate, in_print_view};
use crate::error::Result;
use crate::pptx::{self, Background, Frame, Image, ImageKind, Shape};

/// The `--scale` of `presenter export pptx` when none is given: device pixels per CSS pixel
/// of the pictures taken of elements.
pub const DEFAULT_SCALE: u8 = 2;

/// The largest `--scale`.
pub const MAX_SCALE: u8 = 4;

/// What every page adds to the deadline (module docs).
pub const PER_PAGE: Duration = Duration::from_secs(3);

/// The scene extractor (module docs), a JavaScript function `(pageIndex) => scene`.
pub const SCENE_SCRIPT: &str = include_str!("scene.js");

/// The pages of a deck ([`html_to_pages`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Pages {
    /// The slide width reveal.js laid the deck out with, in CSS pixels (`[reveal] width`,
    /// a percentage resolved): the width of every page's coordinate space.
    pub width: f64,
    /// The slide height, likewise.
    pub height: f64,
    /// How many slides the pages show.
    pub slides: usize,
    /// The pages, in page order.
    pub pages: Vec<Page>,
}

/// One page of the print view as shapes, and where it belongs.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// The slide, counted from 1 in page order (vertical stacks flattened, as in the PDF).
    pub slide: usize,
    /// The fragment step this page shows, from 1 (the slide before its first fragment) to
    /// `steps`.
    pub step: usize,
    /// How many steps the slide has: one, plus one per fragment group when reveal separates
    /// fragments.
    pub steps: usize,
    /// Which page of the slide this is, from 1 to `parts`.
    pub part: usize,
    /// How many pages the slide takes: more than one when it is taller than a page.
    pub parts: usize,
    /// The slide's first heading, as text on one line, if it has one.
    pub heading: Option<String>,
    /// The speaker notes, one entry per line (an empty entry for a blank line between
    /// paragraphs); empty without notes.
    pub notes: Vec<String>,
    /// The slide's background.
    pub background: Background,
    /// The shapes, back to front, in slide coordinates.
    pub shapes: Vec<Shape>,
}

/// Converts every page of the standalone deck `html` in reveal.js' print view into shapes,
/// with the Chrome executable `chrome` (module docs), photographing what must be
/// photographed at `scale` device pixels per CSS pixel (clamped to 1 to [`MAX_SCALE`]).
/// Starting Chrome and laying the deck out must finish within `timeout`, plus [`PER_PAGE`]
/// per page for converting them.
///
/// Errors (all [`crate::Error::Other`]): those of [`crate::pdf::html_to_pdf`]; and a print
/// view without pages, a page that cannot be scrolled to, a scene the extractor cannot
/// produce, or a photograph that is not a PNG, each reported like a failure of Chrome
/// (`Chrome failed to capture the deck: …`).
pub fn html_to_pages(chrome: &Path, html: &str, timeout: Duration, scale: u8) -> Result<Pages> {
    let scale = scale.clamp(1, MAX_SCALE);
    in_print_view(chrome, html, timeout, "capture", move |cdp, page| {
        capture(cdp, page, scale)
    })
}

/// Runs in the ready print view (module docs): makes every page whole pages tall, appends
/// a spacer so that the last page can scroll to the top of the viewport, and resolves to
/// `{slideWidth, slideHeight, pageWidth, pageHeight, pages: [{top, parts, original,
/// heading, notes}]}`.
pub const PAGES_SCRIPT: &str = r#"(() => {
  const config = Reveal.getConfig();
  const size = Reveal.getComputedSlideSize(window.innerWidth, window.innerHeight);
  const pageWidth = Math.floor(size.width * (1 + config.margin));
  const pageHeight = Math.floor(size.height * (1 + config.margin));
  const offset = Number(config.pdfPageHeightOffset) || 0;
  const elements = Array.from(document.querySelectorAll('.reveal .slides .pdf-page'));
  const notesText = (html) => {
    const box = document.createElement('div');
    box.style.cssText = 'position:absolute;left:-100000px;top:0;width:800px';
    box.innerHTML = html;
    box.querySelectorAll('.katex-mathml').forEach((e) => e.remove());
    document.body.appendChild(box);
    const text = box.innerText;
    box.remove();
    return text;
  };
  const pages = elements.map((page) => {
    const parts = Math.max(1, Math.round(page.offsetHeight / (pageHeight + offset)));
    page.style.height = (pageHeight * parts) + 'px';
    const section = page.querySelector(':scope > section');
    const heading = section ? section.querySelector('h1, h2, h3, h4, h5, h6') : null;
    const notes = section ? Reveal.getSlideNotes(section) : null;
    return {
      parts,
      original: !!(section && section.slideBackgroundElement),
      heading: heading ? heading.textContent : null,
      notes: notes ? notesText(notes) : null,
    };
  });
  const spacer = document.createElement('div');
  spacer.style.height = pageHeight + 'px';
  document.body.appendChild(spacer);
  elements.forEach((page, i) => {
    pages[i].top = page.getBoundingClientRect().top + window.scrollY;
  });
  return { slideWidth: size.width, slideHeight: size.height, pageWidth, pageHeight, pages };
})()"#;

/// Installs the stylesheet of the photograph modes (module docs) and switches to `mode`:
/// `content` (backgrounds hidden, page transparent), `background` (the slide's content
/// hidden) or `none`.
fn shot_mode_script(mode: &str) -> String {
    format!(
        r#"(() => {{
  let style = document.getElementById('presenter-shot-style');
  if (!style) {{
    style = document.createElement('style');
    style.id = 'presenter-shot-style';
    style.textContent = `
html.presenter-shot-content, html.presenter-shot-content body {{ background: transparent !important; }}
html.presenter-shot-content .reveal .slides .pdf-page {{ background: transparent !important; }}
html.presenter-shot-content .reveal .slide-background {{ visibility: hidden !important; }}
html.presenter-shot-content .reveal .slides section {{ background: transparent !important; }}
html.presenter-shot-background .reveal .slides .pdf-page > section {{ visibility: hidden !important; }}
html.presenter-shot-background .reveal .slides .pdf-page > .slide-number-pdf {{ visibility: hidden !important; }}
html.presenter-shot-background .reveal .slides .pdf-page > .speaker-notes-pdf {{ visibility: hidden !important; }}`;
    document.head.appendChild(style);
  }}
  const html = document.documentElement;
  html.classList.toggle('presenter-shot-content', {content});
  html.classList.toggle('presenter-shot-background', {background});
  return new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
}})()"#,
        content = mode == "content",
        background = mode == "background",
    )
}

/// Resolves to the `src` of the image list entry `index` of the current page (a `data:`
/// URL for the pictures of a standalone deck), or null.
fn image_script(index: usize) -> String {
    format!(
        r#"(() => {{
  const entry = (window.__presenterImages || [])[{index}];
  if (!entry) return null;
  if (typeof entry === 'string') return entry;
  return entry.currentSrc || entry.src || null;
}})()"#
    )
}

/// The print view as [`PAGES_SCRIPT`] reports it.
#[derive(Debug, Clone, PartialEq)]
struct Layout {
    /// The slide size reveal.js used.
    slide_width: f64,
    slide_height: f64,
    /// The page size: the viewport for the photographs.
    page_width: f64,
    page_height: f64,
    /// The `.pdf-page` elements, in order.
    pages: Vec<PageInfo>,
}

/// One `.pdf-page` element.
#[derive(Debug, Clone, PartialEq)]
struct PageInfo {
    /// Its top, in CSS pixels from the top of the document.
    top: f64,
    /// How many pages tall it is.
    parts: usize,
    /// Whether it is the slide's own page (else a clone for a fragment step).
    original: bool,
    /// The slide's first heading, if any.
    heading: Option<String>,
    /// The notes, by line.
    notes: Vec<String>,
}

impl Layout {
    /// Reads what [`PAGES_SCRIPT`] returned.
    fn from_json(value: &Value) -> std::result::Result<Layout, Failure> {
        let number = |field: &str| {
            value
                .get(field)
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite() && *n > 0.0)
                .ok_or_else(|| {
                    Failure::Other(format!("the print view reports no usable {field}: {value}"))
                })
        };
        let pages = value
            .get("pages")
            .and_then(Value::as_array)
            .ok_or_else(|| Failure::Other(format!("the print view lists no pages: {value}")))?;
        let pages = pages
            .iter()
            .map(|page| {
                let top = page.get("top").and_then(Value::as_f64).ok_or_else(|| {
                    Failure::Other(format!("a page of the print view has no top: {page}"))
                })?;
                let parts = page
                    .get("parts")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .max(1);
                Ok(PageInfo {
                    top,
                    parts: usize::try_from(parts).unwrap_or(usize::MAX),
                    original: page
                        .get("original")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                    heading: page
                        .get("heading")
                        .and_then(Value::as_str)
                        .map(single_line)
                        .filter(|heading| !heading.is_empty()),
                    notes: page
                        .get("notes")
                        .and_then(Value::as_str)
                        .map(notes_lines)
                        .unwrap_or_default(),
                })
            })
            .collect::<std::result::Result<Vec<_>, Failure>>()?;
        Ok(Layout {
            slide_width: number("slideWidth")?,
            slide_height: number("slideHeight")?,
            page_width: number("pageWidth")?,
            page_height: number("pageHeight")?,
            pages,
        })
    }
}

/// What the scene extractor returns for one page.
#[derive(Debug, Deserialize)]
struct Scene {
    /// The background.
    background: Background,
    /// The shapes of each printed page of it.
    #[serde(default)]
    parts: Vec<ScenePart>,
}

/// The shapes of one printed page.
#[derive(Debug, Default, Deserialize)]
struct ScenePart {
    #[serde(default)]
    shapes: Vec<Shape>,
}

/// Step 4 for shapes (module docs): lists the pages, sizes the viewport and converts every
/// page of the page `session`.
fn capture(cdp: &mut Cdp<'_>, session: &str, scale: u8) -> std::result::Result<Pages, Failure> {
    let page = Some(session);
    cdp.stage = "listing the pages";
    let layout = Layout::from_json(&evaluate(cdp, page, PAGES_SCRIPT)?)?;
    let total: usize = layout.pages.iter().map(|info| info.parts).sum();
    if total == 0 {
        return Err(Failure::Other(
            "reveal.js laid out no pages (the deck has no slides)".into(),
        ));
    }
    let budget = PER_PAGE.saturating_mul(u32::try_from(total).unwrap_or(u32::MAX));
    cdp.deadline += budget;
    cdp.timeout += budget;
    tracing::debug!(
        pages = layout.pages.len(),
        printed = total,
        width = layout.page_width,
        height = layout.page_height,
        scale,
        "converting the pages"
    );

    cdp.stage = "sizing the viewport";
    let metrics = json!({
        "width": layout.page_width,
        "height": layout.page_height,
        "deviceScaleFactor": 1,
        "mobile": false,
    });
    cdp.call(page, "Emulation.setDeviceMetricsOverride", metrics)?;

    cdp.stage = "converting the slides";
    let positions = positions(
        &layout
            .pages
            .iter()
            .map(|info| info.original)
            .collect::<Vec<_>>(),
    );
    let mut pages = Vec::with_capacity(total);
    for (index, info) in layout.pages.iter().enumerate() {
        let (slide, step, steps) = positions[index];
        let scene = scene_of(cdp, page, index)?;
        let mut background = scene.background;
        let mut parts = scene.parts;
        parts.resize_with(info.parts, ScenePart::default);
        let mut images = ImageCache::default();
        images.resolve_background(cdp, page, &mut background)?;
        for part in &mut parts {
            for shape in &mut part.shapes {
                if let Shape::Picture(picture) = shape {
                    images.resolve_picture(cdp, page, picture)?;
                }
            }
        }
        for (part_index, part) in parts.into_iter().enumerate() {
            let top = info.top + part_index as f64 * layout.page_height;
            scroll_to(cdp, page, top, index + 1)?;
            let shapes = photograph(cdp, page, part.shapes, scale)?;
            if part_index == 0 {
                photograph_background(cdp, page, &mut background, scale)?;
            }
            tracing::trace!(
                "page {}/{total}: slide {slide}, step {step}/{steps}, part {}/{}, {} shapes",
                pages.len() + 1,
                part_index + 1,
                info.parts,
                shapes.len()
            );
            pages.push(Page {
                slide,
                step,
                steps,
                part: part_index + 1,
                parts: info.parts,
                heading: info.heading.clone(),
                notes: info.notes.clone(),
                background: background.clone(),
                shapes,
            });
        }
    }
    evaluate(cdp, page, &shot_mode_script("none"))?;
    Ok(Pages {
        width: layout.slide_width,
        height: layout.slide_height,
        slides: positions.last().map_or(0, |(slide, ..)| *slide),
        pages,
    })
}

/// Runs the scene extractor for page `index`.
fn scene_of(
    cdp: &mut Cdp<'_>,
    page: Option<&str>,
    index: usize,
) -> std::result::Result<Scene, Failure> {
    let value = evaluate(cdp, page, &format!("({SCENE_SCRIPT})({index})"))?;
    serde_json::from_value(value).map_err(|e| {
        Failure::Other(format!(
            "the scene of page {} cannot be read: {e}",
            index + 1
        ))
    })
}

/// The pictures of one page, fetched by their image list entry.
#[derive(Default)]
struct ImageCache {
    fetched: HashMap<usize, Option<Image>>,
}

impl ImageCache {
    /// The picture of entry `index`, if it is a PNG, JPEG or GIF.
    fn get(
        &mut self,
        cdp: &mut Cdp<'_>,
        page: Option<&str>,
        index: usize,
    ) -> std::result::Result<Option<Image>, Failure> {
        if let Some(image) = self.fetched.get(&index) {
            return Ok(image.clone());
        }
        let value = evaluate(cdp, page, &image_script(index))?;
        let image = value.as_str().and_then(pptx::image_from_data_url);
        self.fetched.insert(index, image.clone());
        Ok(image)
    }

    /// Gives `picture` its bytes when its image list entry holds a usable picture; its
    /// `shot` stays for the others.
    fn resolve_picture(
        &mut self,
        cdp: &mut Cdp<'_>,
        page: Option<&str>,
        picture: &mut pptx::PictureShape,
    ) -> std::result::Result<(), Failure> {
        if let Some(index) = picture.image_ref {
            picture.image = self.get(cdp, page, index)?;
            if picture.image.is_some() {
                picture.place_background();
                picture.shot = None;
            }
        }
        Ok(())
    }

    /// Likewise for the background picture.
    fn resolve_background(
        &mut self,
        cdp: &mut Cdp<'_>,
        page: Option<&str>,
        background: &mut Background,
    ) -> std::result::Result<(), Failure> {
        if background.shot_only {
            return Ok(());
        }
        if let Some(index) = background.image_ref {
            background.image = self.get(cdp, page, index)?;
            if background.image.is_some() {
                background.shot = None;
            }
        }
        Ok(())
    }
}

/// Photographs the pictures of `shapes` that have no bytes yet (module docs), in the
/// content mode; the ones that cannot be photographed are dropped.
fn photograph(
    cdp: &mut Cdp<'_>,
    page: Option<&str>,
    shapes: Vec<Shape>,
    scale: u8,
) -> std::result::Result<Vec<Shape>, Failure> {
    let needed = shapes.iter().any(|shape| {
        matches!(shape, Shape::Picture(picture) if picture.image.is_none() && picture.shot.is_some())
    });
    if !needed {
        return Ok(shapes);
    }
    evaluate(cdp, page, &shot_mode_script("content"))?;
    let mut out = Vec::with_capacity(shapes.len());
    for shape in shapes {
        match shape {
            Shape::Picture(mut picture) if picture.image.is_none() => {
                let Some(shot) = picture.shot else {
                    continue;
                };
                picture.image = screenshot(cdp, page, &shot, scale, true)?;
                picture.sizing = None;
                picture.position = None;
                if picture.image.is_some() {
                    out.push(Shape::Picture(picture));
                }
            }
            other => out.push(other),
        }
    }
    evaluate(cdp, page, &shot_mode_script("none"))?;
    Ok(out)
}

/// Photographs the slide's background when it must be (module docs), in the background
/// mode.
fn photograph_background(
    cdp: &mut Cdp<'_>,
    page: Option<&str>,
    background: &mut Background,
    scale: u8,
) -> std::result::Result<(), Failure> {
    if background.image.is_some() {
        return Ok(());
    }
    let Some(shot) = background.shot else {
        return Ok(());
    };
    if !background.shot_only && background.image_ref.is_none() {
        return Ok(());
    }
    evaluate(cdp, page, &shot_mode_script("background"))?;
    background.image = screenshot(cdp, page, &shot, scale, false)?;
    background.sizing = None;
    background.position = None;
    background.alpha = None;
    evaluate(cdp, page, &shot_mode_script("none"))?;
    Ok(())
}

/// Scrolls the document so that the page at `top` fills the viewport, and checks that it
/// did (`number` names the page in the error).
fn scroll_to(
    cdp: &mut Cdp<'_>,
    page: Option<&str>,
    top: f64,
    number: usize,
) -> std::result::Result<(), Failure> {
    let script = format!(
        "(async () => {{\
           window.scrollTo({{ top: {top}, left: 0, behavior: 'instant' }});\
           await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));\
           return window.scrollY;\
         }})()"
    );
    let scrolled = evaluate(cdp, page, &script)?;
    let at = scrolled.as_f64().unwrap_or(f64::NAN);
    if (at - top).abs() > 1.0 {
        return Err(Failure::Other(format!(
            "cannot scroll to page {number}: it is at {top} px but the document scrolled \
             to {scrolled}"
        )));
    }
    Ok(())
}

/// A photograph of the document area `shot` (document pixels) as a PNG at `scale` device
/// pixels per CSS pixel; `None` for an area without size.
fn screenshot(
    cdp: &mut Cdp<'_>,
    page: Option<&str>,
    shot: &Frame,
    scale: u8,
    transparent: bool,
) -> std::result::Result<Option<Image>, Failure> {
    if !(shot.w >= 1.0 && shot.h >= 1.0 && shot.x.is_finite() && shot.y.is_finite()) {
        return Ok(None);
    }
    let params = json!({
        "format": "png",
        "clip": { "x": shot.x, "y": shot.y, "width": shot.w, "height": shot.h, "scale": scale },
        "captureBeyondViewport": true,
        "fromSurface": true,
        "omitBackground": transparent,
    });
    let taken = cdp.call(page, "Page.captureScreenshot", params)?;
    let data = taken.get("data").and_then(Value::as_str).unwrap_or("");
    let png = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| {
            Failure::Other(format!(
                "Page.captureScreenshot returned invalid base64: {e}"
            ))
        })?;
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(Failure::Other(format!(
            "Page.captureScreenshot returned {} bytes that are not a PNG",
            png.len()
        )));
    }
    Ok(Some(Image {
        bytes: png,
        kind: ImageKind::Png,
    }))
}

/// Where each page belongs, from whether it is a slide's own page (`original`) or a clone
/// for a fragment step: `(slide, step, steps)`, slides and steps counted from 1. A clone
/// belongs to the slide of the last own page before it; a first page that is a clone
/// (never, with reveal.js) starts slide 1 all the same.
fn positions(originals: &[bool]) -> Vec<(usize, usize, usize)> {
    let mut groups: Vec<usize> = Vec::new();
    for (index, &original) in originals.iter().enumerate() {
        if original || index == 0 {
            groups.push(1);
        } else if let Some(last) = groups.last_mut() {
            *last += 1;
        }
    }
    groups
        .iter()
        .enumerate()
        .flat_map(|(slide, &steps)| (1..=steps).map(move |step| (slide + 1, step, steps)))
        .collect()
}

/// `text` on one line: its white space collapsed to single spaces, trimmed.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The lines of the notes `text`, each without trailing white space, without blank lines at
/// either end, and with runs of blank lines collapsed to one.
fn notes_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() && lines.last().is_none_or(String::is_empty) {
            continue;
        }
        lines.push(line.to_string());
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_numbered_by_slide_and_step() {
        assert_eq!(positions(&[]), []);
        assert_eq!(positions(&[true]), [(1, 1, 1)]);
        assert_eq!(
            positions(&[true, false, false, true, true, false]),
            [
                (1, 1, 3),
                (1, 2, 3),
                (1, 3, 3),
                (2, 1, 1),
                (3, 1, 2),
                (3, 2, 2)
            ]
        );
        // A first page without the original's mark still starts the first slide.
        assert_eq!(
            positions(&[false, false, true]),
            [(1, 1, 2), (1, 2, 2), (2, 1, 1)]
        );
    }

    #[test]
    fn notes_lines_are_trimmed_and_blank_runs_collapse() {
        assert_eq!(notes_lines(""), Vec::<String>::new());
        assert_eq!(notes_lines("\n\n  \n"), Vec::<String>::new());
        assert_eq!(
            notes_lines("\n\nFirst  \n\n\n\nSecond\r\nThird\n\n"),
            ["First", "", "Second", "Third"]
        );
        assert_eq!(single_line("  A\n  heading\t here "), "A heading here");
    }

    #[test]
    fn the_layout_is_read_from_the_script_result() {
        let value = json!({
            "slideWidth": 1280, "slideHeight": 720, "pageWidth": 1331, "pageHeight": 748,
            "pages": [
                { "top": 0, "parts": 1, "original": true, "heading": " Title ", "notes": "a\n\nb" },
                { "top": 748, "parts": 2, "original": false, "heading": null, "notes": null },
            ],
        });
        let layout = Layout::from_json(&value).unwrap();
        assert_eq!(layout.page_width, 1331.0);
        assert_eq!(layout.pages.len(), 2);
        assert_eq!(layout.pages[0].heading.as_deref(), Some("Title"));
        assert_eq!(layout.pages[0].notes, ["a", "", "b"]);
        assert_eq!(layout.pages[1].parts, 2);
        assert!(!layout.pages[1].original);
        assert_eq!(layout.pages[1].heading, None);
        assert!(layout.pages[1].notes.is_empty());
        for broken in [
            json!({ "slideWidth": 0, "slideHeight": 720, "pageWidth": 1, "pageHeight": 1, "pages": [] }),
            json!({ "slideWidth": 1280, "slideHeight": 720, "pageWidth": 1, "pageHeight": 1 }),
            json!({ "slideWidth": 1280, "slideHeight": 720, "pageWidth": 1, "pageHeight": 1, "pages": [{}] }),
        ] {
            assert!(Layout::from_json(&broken).is_err(), "{broken}");
        }
    }

    #[test]
    fn scenes_deserialize_with_their_parts_and_background() {
        let value = json!({
            "background": { "color": { "r": 1, "g": 2, "b": 3, "a": 1 }, "imageRef": 0, "shot": { "x": 0, "y": 0, "w": 10, "h": 10 }, "alpha": null, "sizing": "cover", "position": "50% 50%" },
            "parts": [ { "shapes": [ { "kind": "rect", "frame": { "x": 0, "y": 0, "w": 1, "h": 1 }, "fill": { "color": { "r": 0, "g": 0, "b": 0, "a": 1 } }, "line": null, "radius": null } ] }, { "shapes": [] } ],
            "images": 1,
        });
        let scene: Scene = serde_json::from_value(value).unwrap();
        assert_eq!(scene.background.image_ref, Some(0));
        assert_eq!(scene.background.sizing.as_deref(), Some("cover"));
        assert_eq!(scene.parts.len(), 2);
        assert!(matches!(scene.parts[0].shapes[0], Shape::Rect(_)));
    }

    #[test]
    fn the_scene_script_is_a_function_expression() {
        let script = SCENE_SCRIPT.trim();
        assert!(
            script.starts_with("// The scene extractor"),
            "{}",
            &script[..60]
        );
        let body = script
            .lines()
            .skip_while(|line| line.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.trim_start().starts_with("(function (pageIndex)"));
        assert!(body.trim_end().ends_with("})"));
        assert!(shot_mode_script("content").contains("'presenter-shot-content', true"));
        assert!(shot_mode_script("none").contains("'presenter-shot-background', false"));
        assert!(image_script(3).contains("[3]"));
    }

    #[test]
    fn scales_are_clamped() {
        // A missing executable fails before the scale is used, with the scale clamped.
        let err = html_to_pages(
            Path::new("/nonexistent/chrome"),
            "<p>x</p>",
            Duration::from_secs(1),
            9,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot start /nonexistent/chrome: not an executable file"
        );
        assert_eq!(9u8.clamp(1, MAX_SCALE), 4);
        assert_eq!(0u8.clamp(1, MAX_SCALE), 1);
    }
}
