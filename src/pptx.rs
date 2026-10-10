//! PowerPoint files (`presenter export pptx`, `docs/spec/cli.md` export): a `.pptx` whose
//! slides hold editable shapes, text boxes, pictures, rectangles and tables, placed where
//! the browser laid the deck out, with the speaker notes in the notes pane.
//!
//! [`write()`] takes a [`Presentation`]: the slides ([`Slide`]) with their background and
//! shapes ([`Shape`]), in slide pixels (the deck's slide size, `width` × `height`), and
//! returns the file's bytes. `pdf::capture` builds the model from the deck as reveal.js'
//! print view renders it. The file is an Office Open XML package (ECMA-376), written here
//! without a library:
//!
//! - the ZIP container (`Zip`): entries are stored, not deflated (the pictures are
//!   compressed already and the XML parts are small), with a fixed modification time, so
//!   the file depends on nothing but its contents;
//! - the parts: `[Content_Types].xml`, the package relationships, `docProps/core.xml` (the
//!   deck's title and author) and `app.xml`, `ppt/presentation.xml` (the slide list and the
//!   slide size), one slide master with one blank layout (both with a title placeholder,
//!   which the first heading of a slide fills) and a theme, a notes master with its own
//!   theme (PowerPoint gives every master one), and per slide a slide part
//!   (`ppt/slides/slideN.xml`), the media parts of its pictures (`ppt/media/imageK.png`,
//!   `.jpeg`, `.gif`; identical pictures share one) and, when there are notes, a notes
//!   slide (`ppt/notesSlides/notesSlideN.xml`, one paragraph per line).
//!
//! The slide size ([`slide_size_emu`]) keeps the deck's aspect ratio at PowerPoint's usual
//! height of 7.5 inches: a 16:9 deck gives PowerPoint's "Widescreen" size (13.333 × 7.5 in),
//! a 4:3 deck its "Standard" size (10 × 7.5 in). Lengths are converted from slide pixels at
//! that scale, font sizes with them (a 1280 px wide slide makes one pixel three quarters of
//! a point, as on a 96 dpi screen).
//!
//! Text that goes into the XML (names, text, notes, the deck's title and author) is
//! escaped, and the characters XML 1.0 cannot hold (control characters other than tab, line
//! feed and carriage return) are dropped: PowerPoint would otherwise offer to repair the
//! file.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::Deserialize;

use crate::error::{Error, Result};

/// English Metric Units per inch, the unit of every length in a `.pptx`.
pub const EMU_PER_INCH: u64 = 914_400;

/// English Metric Units per point.
pub const EMU_PER_POINT: u64 = 12_700;

/// The height of every slide: 7.5 inches, PowerPoint's height for its standard sizes.
pub const SLIDE_HEIGHT_EMU: u64 = 7_500 * EMU_PER_INCH / 1_000;

/// PowerPoint's largest slide dimension, 56 inches.
const MAX_SLIDE_EMU: u64 = 56 * EMU_PER_INCH;

/// PowerPoint's smallest slide dimension, 1 inch.
const MIN_SLIDE_EMU: u64 = EMU_PER_INCH;

/// The notes page: 7.5 × 10 inches, as PowerPoint makes it.
const NOTES_WIDTH_EMU: u64 = SLIDE_HEIGHT_EMU;
const NOTES_HEIGHT_EMU: u64 = 10 * EMU_PER_INCH;

/// The longest slide name written (cut after this many characters).
const MAX_NAME_CHARS: usize = 80;

/// The longest alt text written.
const MAX_DESCRIPTION_CHARS: usize = 400;

/// The smallest font size written, in hundredths of a point (PowerPoint's minimum is 1 pt).
const MIN_FONT_SIZE: i64 = 100;

// --- The model ----------------------------------------------------------------------------

/// What goes into the file.
#[derive(Debug, Clone, Default)]
pub struct Presentation {
    /// The deck's title (`docProps/core.xml`), if any.
    pub title: Option<String>,
    /// The deck's author (`docProps/core.xml`), if any.
    pub author: Option<String>,
    /// The deck's slide width in slide pixels (reveal.js' `width`).
    pub width: f64,
    /// The deck's slide height in slide pixels.
    pub height: f64,
    /// The slides, in order.
    pub slides: Vec<Slide>,
}

/// One slide of the file.
#[derive(Debug, Clone, Default)]
pub struct Slide {
    /// The slide's name (PowerPoint's selection pane, `Slide.Name`): the heading of the
    /// slide in the deck with its step and part, or `Slide N`.
    pub name: String,
    /// The slide's background.
    pub background: Background,
    /// The shapes, back to front.
    pub shapes: Vec<Shape>,
    /// The speaker notes, one entry per paragraph (an empty entry is an empty line). No
    /// notes slide is written when this is empty.
    pub notes: Vec<String>,
}

/// A colour with opacity (`a`, 0 to 1).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Rgba {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Opacity, 0 to 1.
    #[serde(default = "one")]
    pub a: f64,
}

fn one() -> f64 {
    1.0
}

fn yes() -> bool {
    true
}

fn one_u32() -> u32 {
    1
}

/// A box in slide pixels: its top-left corner and size.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
pub struct Frame {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// Distances from the four edges, in slide pixels (text insets) or as fractions (crops).
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
pub struct Edges {
    /// Left.
    #[serde(default)]
    pub l: f64,
    /// Top.
    #[serde(default)]
    pub t: f64,
    /// Right.
    #[serde(default)]
    pub r: f64,
    /// Bottom.
    #[serde(default)]
    pub b: f64,
}

/// How a box is filled.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Fill {
    /// One colour.
    Color {
        /// The colour.
        color: Rgba,
    },
    /// A linear gradient.
    Gradient {
        /// The gradient.
        gradient: Gradient,
    },
}

/// A linear gradient, as CSS describes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Gradient {
    /// The direction in degrees, CSS's way: 0 is towards the top, 90 towards the right.
    pub angle: f64,
    /// The colour stops, in order.
    pub stops: Vec<Stop>,
}

/// A colour stop of a gradient.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Stop {
    /// The colour.
    pub color: Rgba,
    /// Where along the gradient, 0 to 1.
    pub pos: f64,
}

/// A border or outline.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Line {
    /// The colour.
    pub color: Rgba,
    /// The width in slide pixels.
    pub width: f64,
    /// Dashed (or dotted) rather than solid.
    #[serde(default)]
    pub dash: bool,
}

/// Horizontal alignment of a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    /// Left.
    #[default]
    L,
    /// Centred.
    Ctr,
    /// Right.
    R,
    /// Justified.
    Just,
}

/// A paragraph's bullet or number.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Bullet {
    /// A character (`•`, `◦`, `▪`, or whatever the list's marker shows).
    Char {
        /// The character(s).
        #[serde(rename = "char")]
        glyph: String,
        /// Its colour; the text's when `None`.
        color: Option<Rgba>,
    },
    /// Automatic numbering.
    Auto {
        /// PowerPoint's scheme name (`arabicPeriod`, `alphaLcPeriod`, `romanLcPeriod`, …).
        scheme: String,
        /// The first number.
        #[serde(default = "one_u32")]
        start: u32,
        /// Its colour; the text's when `None`.
        color: Option<Rgba>,
    },
}

/// One paragraph of a text box or table cell.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paragraph {
    /// Alignment.
    #[serde(default)]
    pub align: Align,
    /// The list level, 0 to 8.
    #[serde(default)]
    pub level: u8,
    /// The bullet, if any.
    #[serde(default)]
    pub bullet: Option<Bullet>,
    /// The left margin of the text in slide pixels (where the text of a list item starts).
    #[serde(default)]
    pub margin_left: f64,
    /// The first line's indent relative to the margin, in slide pixels; negative for a
    /// hanging bullet.
    #[serde(default)]
    pub indent: f64,
    /// The line height in slide pixels; the font's own when `None`.
    #[serde(default)]
    pub line_height: Option<f64>,
    /// Space above the paragraph in slide pixels.
    #[serde(default)]
    pub space_before: f64,
    /// The runs.
    #[serde(default)]
    pub runs: Vec<Run>,
}

/// A run of a paragraph.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Run {
    /// A line break.
    Break {
        /// Always true.
        br: bool,
    },
    /// Text in one style.
    Text(TextRun),
}

/// Text in one style.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TextRun {
    /// The text.
    pub text: String,
    /// The font family.
    pub font: String,
    /// The font size in slide pixels.
    pub size: f64,
    /// Bold.
    #[serde(default)]
    pub bold: bool,
    /// Italic.
    #[serde(default)]
    pub italic: bool,
    /// Underlined.
    #[serde(default)]
    pub underline: bool,
    /// Struck through.
    #[serde(default)]
    pub strike: bool,
    /// The text colour.
    pub color: Rgba,
    /// A highlight colour behind the text (inline code, `kbd`, `mark`).
    #[serde(default)]
    pub highlight: Option<Rgba>,
    /// A hyperlink.
    #[serde(default)]
    pub link: Option<String>,
    /// `all` or `small` capitals.
    #[serde(default)]
    pub caps: Option<String>,
    /// Baseline shift in thousandths of a percent of the font size (30000 for a
    /// superscript, -25000 for a subscript).
    #[serde(default)]
    pub baseline: i32,
    /// Letter spacing in slide pixels.
    #[serde(default)]
    pub spacing: f64,
}

/// A text box.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextShape {
    /// The box.
    pub frame: Frame,
    /// The insets from the box to the text, in slide pixels.
    #[serde(default)]
    pub insets: Edges,
    /// The box's fill.
    #[serde(default)]
    pub fill: Option<Fill>,
    /// The box's outline.
    #[serde(default)]
    pub line: Option<Line>,
    /// Rounded corners, the radius in slide pixels.
    #[serde(default)]
    pub radius: Option<f64>,
    /// Whether lines wrap at the box's edge.
    #[serde(default = "yes")]
    pub wrap: bool,
    /// Whether this is the slide's title (the title placeholder).
    #[serde(default)]
    pub title: bool,
    /// The paragraphs.
    #[serde(default)]
    pub paragraphs: Vec<Paragraph>,
}

/// A picture.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PictureShape {
    /// The box.
    pub frame: Frame,
    /// The picture itself; the shape is skipped when there is none.
    #[serde(skip)]
    pub image: Option<Image>,
    /// Where `pdf::capture` gets the picture from: an entry of the page's image list.
    #[serde(default)]
    pub image_ref: Option<usize>,
    /// Where `pdf::capture` gets the picture from instead: a photograph of this area of
    /// the document (document pixels).
    #[serde(default)]
    pub shot: Option<Frame>,
    /// Alt text.
    #[serde(default)]
    pub alt: String,
    /// Opacity, when below 1.
    #[serde(default)]
    pub alpha: Option<f64>,
    /// The part of the picture shown, as fractions of its size cut off each edge.
    #[serde(default)]
    pub crop: Option<Edges>,
    /// Margins inside the box, as fractions of the box, that the picture leaves free.
    #[serde(default)]
    pub fit: Option<Edges>,
    /// For a CSS background image: its `background-size`; `crop` and `fit` are then
    /// computed from the picture's own size ([`PictureShape::place_background`]).
    #[serde(default)]
    pub sizing: Option<String>,
    /// For a CSS background image: its `background-position`.
    #[serde(default)]
    pub position: Option<String>,
}

/// A rectangle (a box's background or border).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct RectShape {
    /// The box.
    pub frame: Frame,
    /// The fill.
    #[serde(default)]
    pub fill: Option<Fill>,
    /// The outline.
    #[serde(default)]
    pub line: Option<Line>,
    /// Rounded corners, the radius in slide pixels.
    #[serde(default)]
    pub radius: Option<f64>,
}

/// A table.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct TableShape {
    /// The box.
    pub frame: Frame,
    /// The column widths in slide pixels.
    pub columns: Vec<f64>,
    /// The rows.
    pub rows: Vec<Row>,
}

/// A table row.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Row {
    /// The height in slide pixels.
    pub height: f64,
    /// One cell per column of the table (merged cells included, marked).
    pub cells: Vec<Cell>,
}

/// A table cell.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    /// The paragraphs.
    #[serde(default)]
    pub paragraphs: Vec<Paragraph>,
    /// The padding in slide pixels.
    #[serde(default)]
    pub insets: Edges,
    /// The fill.
    #[serde(default)]
    pub fill: Option<Fill>,
    /// The borders.
    #[serde(default)]
    pub borders: Borders,
    /// How many columns the cell spans.
    #[serde(default = "one_u32")]
    pub grid_span: u32,
    /// How many rows the cell spans.
    #[serde(default = "one_u32")]
    pub row_span: u32,
    /// Whether this is the continuation of a cell to its left.
    #[serde(default)]
    pub h_merge: bool,
    /// Whether this is the continuation of a cell above.
    #[serde(default)]
    pub v_merge: bool,
    /// Vertical anchoring: `t`, `ctr` or `b`.
    #[serde(default)]
    pub anchor: Option<String>,
}

/// The borders of a table cell.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Borders {
    /// Left.
    #[serde(default)]
    pub l: Option<Line>,
    /// Top.
    #[serde(default)]
    pub t: Option<Line>,
    /// Right.
    #[serde(default)]
    pub r: Option<Line>,
    /// Bottom.
    #[serde(default)]
    pub b: Option<Line>,
}

/// A shape of a slide.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Shape {
    /// A text box.
    Text(TextShape),
    /// A picture.
    Picture(PictureShape),
    /// A rectangle.
    Rect(RectShape),
    /// A table.
    Table(TableShape),
}

/// A slide's background: a colour, and a picture over it covering the whole slide.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Background {
    /// The colour.
    #[serde(default)]
    pub color: Option<Rgba>,
    /// The picture, placed by `sizing` and `position` ([`PictureShape::place_background`]).
    #[serde(skip)]
    pub image: Option<Image>,
    /// Where `pdf::capture` gets the picture from (see [`PictureShape::image_ref`]).
    #[serde(default)]
    pub image_ref: Option<usize>,
    /// The area to photograph instead (see [`PictureShape::shot`]).
    #[serde(default)]
    pub shot: Option<Frame>,
    /// Whether the picture can only be a photograph (a gradient, a video).
    #[serde(default)]
    pub shot_only: bool,
    /// The picture's opacity, when below 1.
    #[serde(default)]
    pub alpha: Option<f64>,
    /// The picture's `background-size`.
    #[serde(default)]
    pub sizing: Option<String>,
    /// The picture's `background-position`.
    #[serde(default)]
    pub position: Option<String>,
}

/// A picture's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// The format.
    pub kind: ImageKind,
}

/// The picture formats written (what PowerPoint shows everywhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageKind {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
}

impl ImageKind {
    /// The file extension of the media part.
    pub fn extension(self) -> &'static str {
        match self {
            ImageKind::Png => "png",
            ImageKind::Jpeg => "jpeg",
            ImageKind::Gif => "gif",
        }
    }

    /// The media type.
    pub fn mime(self) -> &'static str {
        match self {
            ImageKind::Png => "image/png",
            ImageKind::Jpeg => "image/jpeg",
            ImageKind::Gif => "image/gif",
        }
    }

    /// The format of a `data:` URL's media type, when it is one written here.
    pub fn from_mime(mime: &str) -> Option<ImageKind> {
        match mime.trim().to_ascii_lowercase().as_str() {
            "image/png" => Some(ImageKind::Png),
            "image/jpeg" | "image/jpg" => Some(ImageKind::Jpeg),
            "image/gif" => Some(ImageKind::Gif),
            _ => None,
        }
    }
}

impl Image {
    /// The picture's size in pixels, read from its header; `None` when it cannot be read.
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        let b = &self.bytes;
        match self.kind {
            ImageKind::Png => {
                if b.len() < 24 || !b.starts_with(b"\x89PNG\r\n\x1a\n") {
                    return None;
                }
                let n = |at: usize| -> Option<u32> {
                    Some(u32::from_be_bytes(b[at..at + 4].try_into().ok()?))
                };
                Some((n(16)?, n(20)?))
            }
            ImageKind::Gif => {
                if b.len() < 10 || !(b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
                    return None;
                }
                let w = u16::from_le_bytes([b[6], b[7]]);
                let h = u16::from_le_bytes([b[8], b[9]]);
                Some((u32::from(w), u32::from(h)))
            }
            ImageKind::Jpeg => jpeg_dimensions(b),
        }
    }
}

/// The size of a JPEG: its first frame header (`SOFn` marker).
fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    while at + 4 <= b.len() {
        if b[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = b[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
            at += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([b[at + 2], b[at + 3]]));
        let frame = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if frame {
            if at + 9 > b.len() {
                return None;
            }
            let h = u16::from_be_bytes([b[at + 5], b[at + 6]]);
            let w = u16::from_be_bytes([b[at + 7], b[at + 8]]);
            return Some((u32::from(w), u32::from(h)));
        }
        if marker == 0xD9 || marker == 0xDA {
            return None;
        }
        at += 2 + len;
    }
    None
}

/// The picture of a `data:` URL, when it is a PNG, JPEG or GIF.
pub fn image_from_data_url(url: &str) -> Option<Image> {
    use base64::Engine as _;
    let rest = url.strip_prefix("data:")?;
    let (header, data) = rest.split_once(',')?;
    let mut parts = header.split(';');
    let kind = ImageKind::from_mime(parts.next()?)?;
    if !parts.any(|part| part.trim().eq_ignore_ascii_case("base64")) {
        return None;
    }
    let cleaned: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(cleaned)
        .ok()?;
    Some(Image { bytes, kind })
}

/// Where a CSS background image is drawn in a box: `(crop, fit)` for `background-size`
/// `sizing` and `background-position` `position` (as computed styles spell them), a
/// picture of `natural` pixels in a box of `frame` pixels. The picture is cropped where it
/// overflows the box and inset where it leaves the box free.
pub fn place_background(
    sizing: &str,
    position: &str,
    natural: (u32, u32),
    box_w: f64,
    box_h: f64,
) -> (Option<Edges>, Option<Edges>) {
    let (nw, nh) = (f64::from(natural.0), f64::from(natural.1));
    if nw <= 0.0 || nh <= 0.0 || box_w <= 0.0 || box_h <= 0.0 {
        return (None, None);
    }
    let words: Vec<&str> = sizing.split_whitespace().collect();
    let length = |word: &str, total: f64| -> Option<f64> {
        if let Some(pct) = word.strip_suffix('%') {
            pct.parse::<f64>().ok().map(|p| total * p / 100.0)
        } else if let Some(px) = word.strip_suffix("px") {
            px.parse::<f64>().ok()
        } else {
            None
        }
    };
    let (dw, dh) = match words.as_slice() {
        ["cover"] => {
            let s = (box_w / nw).max(box_h / nh);
            (nw * s, nh * s)
        }
        ["contain"] => {
            let s = (box_w / nw).min(box_h / nh);
            (nw * s, nh * s)
        }
        [] | ["auto"] | ["auto", "auto"] => (nw, nh),
        [w] | [w, "auto"] => match length(w, box_w) {
            Some(w) => (w, w * nh / nw),
            None => (nw, nh),
        },
        ["auto", h] => match length(h, box_h) {
            Some(h) => (h * nw / nh, h),
            None => (nw, nh),
        },
        [w, h, ..] => match (length(w, box_w), length(h, box_h)) {
            (Some(w), Some(h)) => (w, h),
            _ => (nw, nh),
        },
    };
    let offset = |word: Option<&str>, total: f64, drawn: f64| -> f64 {
        match word {
            Some(w) if w.ends_with('%') => {
                let p = w.trim_end_matches('%').parse::<f64>().unwrap_or(50.0);
                (total - drawn) * p / 100.0
            }
            Some(w) => length(w, total).unwrap_or((total - drawn) / 2.0),
            None => (total - drawn) / 2.0,
        }
    };
    let mut pos = position.split_whitespace();
    let ox = offset(pos.next(), box_w, dw);
    let oy = offset(pos.next(), box_h, dh);
    let crop = Edges {
        l: (-ox).max(0.0) / dw,
        t: (-oy).max(0.0) / dh,
        r: (ox + dw - box_w).max(0.0) / dw,
        b: (oy + dh - box_h).max(0.0) / dh,
    };
    let fit = Edges {
        l: ox.max(0.0) / box_w,
        t: oy.max(0.0) / box_h,
        r: (box_w - ox - dw).max(0.0) / box_w,
        b: (box_h - oy - dh).max(0.0) / box_h,
    };
    let any = |e: &Edges| e.l > 1e-6 || e.t > 1e-6 || e.r > 1e-6 || e.b > 1e-6;
    (any(&crop).then_some(crop), any(&fit).then_some(fit))
}

impl PictureShape {
    /// Fills `crop` and `fit` for a CSS background image from its `sizing`, `position` and
    /// the picture's own size ([`place_background`]). Nothing happens without a picture or
    /// a `sizing`.
    pub fn place_background(&mut self) {
        let (Some(sizing), Some(image)) = (self.sizing.as_deref(), &self.image) else {
            return;
        };
        let Some(natural) = image.dimensions() else {
            return;
        };
        let position = self.position.as_deref().unwrap_or("50% 50%");
        let (crop, fit) = place_background(sizing, position, natural, self.frame.w, self.frame.h);
        self.crop = crop;
        self.fit = fit;
    }
}

// --- Writing ------------------------------------------------------------------------------

/// Writes the `.pptx` for `presentation` (module docs).
///
/// Errors ([`Error::Other`]): no slides; a file larger than a ZIP without ZIP64 can hold
/// (4 GiB, or more than 65 535 entries); a slide size reveal.js cannot have (a width or
/// height that is not a positive number).
pub fn write(presentation: &Presentation) -> Result<Vec<u8>> {
    if presentation.slides.is_empty() {
        return Err(Error::Other(
            "nothing to export: the deck has no slides".into(),
        ));
    }
    let (width_emu, height_emu) = slide_size_emu(presentation.width, presentation.height)?;
    let scale = Scale::new(
        presentation.width,
        presentation.height,
        width_emu,
        height_emu,
    );
    let mut media = Media::default();
    let mut zip = Zip::default();
    zip.add(
        "[Content_Types].xml",
        content_types(presentation).as_bytes(),
    )?;
    zip.add("_rels/.rels", package_rels().as_bytes())?;
    zip.add(
        "docProps/core.xml",
        core_properties(presentation).as_bytes(),
    )?;
    zip.add(
        "docProps/app.xml",
        app_properties(presentation, width_emu, height_emu).as_bytes(),
    )?;
    zip.add(
        "ppt/presentation.xml",
        presentation_xml(presentation, width_emu, height_emu).as_bytes(),
    )?;
    zip.add(
        "ppt/_rels/presentation.xml.rels",
        presentation_rels(presentation).as_bytes(),
    )?;
    zip.add(
        "ppt/slideMasters/slideMaster1.xml",
        slide_master(width_emu, height_emu).as_bytes(),
    )?;
    zip.add(
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        SLIDE_MASTER_RELS.as_bytes(),
    )?;
    zip.add(
        "ppt/slideLayouts/slideLayout1.xml",
        slide_layout(width_emu, height_emu).as_bytes(),
    )?;
    zip.add(
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
        SLIDE_LAYOUT_RELS.as_bytes(),
    )?;
    zip.add("ppt/theme/theme1.xml", THEME.as_bytes())?;
    zip.add("ppt/theme/theme2.xml", THEME.as_bytes())?;
    zip.add(
        "ppt/notesMasters/notesMaster1.xml",
        notes_master(width_emu, height_emu).as_bytes(),
    )?;
    zip.add(
        "ppt/notesMasters/_rels/notesMaster1.xml.rels",
        NOTES_MASTER_RELS.as_bytes(),
    )?;
    for (index, slide) in presentation.slides.iter().enumerate() {
        let number = index + 1;
        let with_notes = !slide.notes.is_empty();
        let written = slide_xml(slide, number, &scale, &mut media);
        zip.add(
            &format!("ppt/slides/slide{number}.xml"),
            written.xml.as_bytes(),
        )?;
        zip.add(
            &format!("ppt/slides/_rels/slide{number}.xml.rels"),
            slide_rels(number, &written.rels, with_notes).as_bytes(),
        )?;
        if with_notes {
            zip.add(
                &format!("ppt/notesSlides/notesSlide{number}.xml"),
                notes_slide_xml(slide).as_bytes(),
            )?;
            zip.add(
                &format!("ppt/notesSlides/_rels/notesSlide{number}.xml.rels"),
                notes_slide_rels(number).as_bytes(),
            )?;
        }
    }
    for (number, image) in media.parts() {
        zip.add(
            &format!("ppt/media/image{number}.{}", image.kind.extension()),
            &image.bytes,
        )?;
    }
    zip.finish()
}

/// The slide size in EMUs for a deck of `width` × `height` pixels (module docs): the
/// height is [`SLIDE_HEIGHT_EMU`] and the width keeps the ratio, unless that leaves
/// PowerPoint's range (1 to 56 inches); then the width is 56 inches and the height follows
/// (at least 1 inch), or the width is 1 inch.
///
/// Errors: a width or height that is not a positive finite number.
pub fn slide_size_emu(width: f64, height: f64) -> Result<(u64, u64)> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err(Error::Other(format!(
            "cannot size the slides: reveal.js reports a slide size of {width} × {height}"
        )));
    }
    let ratio = width / height;
    let mut h = SLIDE_HEIGHT_EMU as f64;
    let mut w = h * ratio;
    if w > MAX_SLIDE_EMU as f64 {
        w = MAX_SLIDE_EMU as f64;
        h = (w / ratio).max(MIN_SLIDE_EMU as f64);
    }
    w = w.max(MIN_SLIDE_EMU as f64);
    // Both are within [1, 56] inches now, so the conversion cannot overflow or truncate.
    Ok((w.round() as u64, h.round() as u64))
}

/// Slide pixels to EMUs and points.
#[derive(Debug, Clone, Copy)]
struct Scale {
    /// EMUs per slide pixel.
    emu_per_px: f64,
    /// Hundredths of a point per slide pixel.
    pt100_per_px: f64,
    /// The slide's width and height in EMUs.
    width_emu: u64,
    height_emu: u64,
}

impl Scale {
    fn new(width_px: f64, height_px: f64, width_emu: u64, height_emu: u64) -> Scale {
        let emu_per_px = width_emu as f64 / width_px;
        let _ = height_px;
        Scale {
            emu_per_px,
            pt100_per_px: emu_per_px / EMU_PER_POINT as f64 * 100.0,
            width_emu,
            height_emu,
        }
    }

    /// A length in EMUs (never negative).
    fn emu(&self, px: f64) -> i64 {
        (px * self.emu_per_px).round().max(0.0) as i64
    }

    /// A position in EMUs (may be negative: a shape partly outside the slide).
    fn pos(&self, px: f64) -> i64 {
        (px * self.emu_per_px).round() as i64
    }

    /// A length in hundredths of a point (for font sizes: at least 1 pt).
    fn pt100(&self, px: f64) -> i64 {
        ((px * self.pt100_per_px).round() as i64).max(MIN_FONT_SIZE)
    }

    /// A signed length in hundredths of a point.
    fn pt100_signed(&self, px: f64) -> i64 {
        (px * self.pt100_per_px).round() as i64
    }
}

/// The media parts: one per distinct picture.
#[derive(Default)]
struct Media {
    /// The part number of each picture, by the BLAKE3 hash of its bytes.
    numbers: HashMap<blake3::Hash, usize>,
    /// The pictures, in part order (part `i + 1` is `parts[i]`).
    parts: Vec<Image>,
}

impl Media {
    /// The part number (from 1) and extension of `image`, adding it when it is new.
    fn number_of(&mut self, image: &Image) -> usize {
        let hash = blake3::hash(&image.bytes);
        if let Some(number) = self.numbers.get(&hash) {
            return *number;
        }
        self.parts.push(image.clone());
        let number = self.parts.len();
        self.numbers.insert(hash, number);
        number
    }

    /// The parts with their numbers.
    fn parts(&self) -> impl Iterator<Item = (usize, &Image)> {
        self.parts
            .iter()
            .enumerate()
            .map(|(index, image)| (index + 1, image))
    }
}

// --- Parts -------------------------------------------------------------------------------

const XML_HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;

const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const NS_RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

const REL_OFFICE_DOCUMENT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const REL_CORE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const REL_APP: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties";
const REL_SLIDE_MASTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster";
const REL_SLIDE_LAYOUT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout";
const REL_THEME: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";
const REL_SLIDE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
const REL_NOTES_MASTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesMaster";
const REL_NOTES_SLIDE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";
const REL_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const REL_HYPERLINK: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

const CT_PRESENTATION: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
const CT_SLIDE_MASTER: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml";
const CT_SLIDE_LAYOUT: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml";
const CT_SLIDE: &str = "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";
const CT_NOTES_MASTER: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.notesMaster+xml";
const CT_NOTES_SLIDE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml";
const CT_THEME: &str = "application/vnd.openxmlformats-officedocument.theme+xml";
const CT_CORE: &str = "application/vnd.openxmlformats-package.core-properties+xml";
const CT_APP: &str = "application/vnd.openxmlformats-officedocument.extended-properties+xml";
const CT_RELS: &str = "application/vnd.openxmlformats-package.relationships+xml";

/// The empty shape-tree header every slide-like part starts with.
const SP_TREE_HEAD: &str = r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#;

/// The colour map every master uses.
const CLR_MAP: &str = r#"bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink""#;

/// `[Content_Types].xml`: the fixed parts, every picture format, and the slide and notes
/// slide of each slide.
fn content_types(presentation: &Presentation) -> String {
    let mut xml = format!(
        r#"{XML_HEAD}
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="{CT_RELS}"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Default Extension="gif" ContentType="image/gif"/><Override PartName="/ppt/presentation.xml" ContentType="{CT_PRESENTATION}"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="{CT_SLIDE_MASTER}"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="{CT_SLIDE_LAYOUT}"/><Override PartName="/ppt/theme/theme1.xml" ContentType="{CT_THEME}"/><Override PartName="/ppt/theme/theme2.xml" ContentType="{CT_THEME}"/><Override PartName="/ppt/notesMasters/notesMaster1.xml" ContentType="{CT_NOTES_MASTER}"/><Override PartName="/docProps/core.xml" ContentType="{CT_CORE}"/><Override PartName="/docProps/app.xml" ContentType="{CT_APP}"/>"#
    );
    for (index, slide) in presentation.slides.iter().enumerate() {
        let number = index + 1;
        let _ = write!(
            xml,
            r#"<Override PartName="/ppt/slides/slide{number}.xml" ContentType="{CT_SLIDE}"/>"#
        );
        if !slide.notes.is_empty() {
            let _ = write!(
                xml,
                r#"<Override PartName="/ppt/notesSlides/notesSlide{number}.xml" ContentType="{CT_NOTES_SLIDE}"/>"#
            );
        }
    }
    xml.push_str("</Types>");
    xml
}

/// `_rels/.rels`: the presentation and the document properties.
fn package_rels() -> String {
    format!(
        r#"{XML_HEAD}
<Relationships xmlns="{NS_RELS}"><Relationship Id="rId1" Type="{REL_OFFICE_DOCUMENT}" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="{REL_CORE}" Target="docProps/core.xml"/><Relationship Id="rId3" Type="{REL_APP}" Target="docProps/app.xml"/></Relationships>"#
    )
}

/// `docProps/core.xml`: the deck's title and author. No dates: the file must not depend
/// on when it was written.
fn core_properties(presentation: &Presentation) -> String {
    let mut xml = format!(
        r#"{XML_HEAD}
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">"#
    );
    if let Some(title) = presentation.title.as_deref().map(single_line) {
        let _ = write!(xml, "<dc:title>{}</dc:title>", escape(&title));
    }
    if let Some(author) = presentation.author.as_deref().map(single_line) {
        let _ = write!(xml, "<dc:creator>{}</dc:creator>", escape(&author));
    }
    xml.push_str("</cp:coreProperties>");
    xml
}

/// `docProps/app.xml`: what wrote the file and how many slides and notes it has.
fn app_properties(presentation: &Presentation, width_emu: u64, height_emu: u64) -> String {
    let notes = presentation
        .slides
        .iter()
        .filter(|slide| !slide.notes.is_empty())
        .count();
    format!(
        r#"{XML_HEAD}
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>presenter</Application><PresentationFormat>{}</PresentationFormat><Slides>{}</Slides><Notes>{notes}</Notes></Properties>"#,
        presentation_format(width_emu, height_emu),
        presentation.slides.len()
    )
}

/// PowerPoint's name for a slide size: its two standard sizes by name, else `Custom`.
fn presentation_format(width_emu: u64, height_emu: u64) -> &'static str {
    match (width_emu, height_emu) {
        (12_192_000, 6_858_000) => "Widescreen",
        (9_144_000, 6_858_000) => "On-screen Show (4:3)",
        _ => "Custom",
    }
}

/// `ppt/presentation.xml`: the masters, the slide list and the sizes.
fn presentation_xml(presentation: &Presentation, width_emu: u64, height_emu: u64) -> String {
    let mut xml = format!(
        r#"{XML_HEAD}
<p:presentation xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}" saveSubsetFonts="1"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:notesMasterIdLst><p:notesMasterId r:id="rId2"/></p:notesMasterIdLst><p:sldIdLst>"#
    );
    for index in 0..presentation.slides.len() {
        let _ = write!(
            xml,
            r#"<p:sldId id="{}" r:id="rId{}"/>"#,
            256 + index,
            slide_rel_id(index + 1)
        );
    }
    let _ = write!(
        xml,
        r#"</p:sldIdLst><p:sldSz cx="{width_emu}" cy="{height_emu}"/><p:notesSz cx="{NOTES_WIDTH_EMU}" cy="{NOTES_HEIGHT_EMU}"/></p:presentation>"#
    );
    xml
}

/// The relationship id of slide `number` in `ppt/_rels/presentation.xml.rels` (`rId1` to
/// `rId3` are the masters and the theme).
fn slide_rel_id(number: usize) -> usize {
    number + 3
}

/// `ppt/_rels/presentation.xml.rels`.
fn presentation_rels(presentation: &Presentation) -> String {
    let mut xml = format!(
        r#"{XML_HEAD}
<Relationships xmlns="{NS_RELS}"><Relationship Id="rId1" Type="{REL_SLIDE_MASTER}" Target="slideMasters/slideMaster1.xml"/><Relationship Id="rId2" Type="{REL_NOTES_MASTER}" Target="notesMasters/notesMaster1.xml"/><Relationship Id="rId3" Type="{REL_THEME}" Target="theme/theme1.xml"/>"#
    );
    for index in 0..presentation.slides.len() {
        let number = index + 1;
        let _ = write!(
            xml,
            r#"<Relationship Id="rId{}" Type="{REL_SLIDE}" Target="slides/slide{number}.xml"/>"#,
            slide_rel_id(number)
        );
    }
    xml.push_str("</Relationships>");
    xml
}

/// The title placeholder of the master and the layout: across the top of the slide.
fn title_placeholder(width_emu: u64, height_emu: u64) -> String {
    let x = width_emu / 15;
    let y = height_emu / 20;
    let w = width_emu - 2 * x;
    let h = height_emu / 5;
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr vert="horz" lIns="0" tIns="0" rIns="0" bIns="0" rtlCol="0" anchor="t"><a:normAutofit/></a:bodyPr><a:lstStyle/><a:p><a:r><a:rPr lang="en-US"/><a:t>Click to edit Master title style</a:t></a:r></a:p></p:txBody></p:sp>"#
    )
}

/// `ppt/slideMasters/slideMaster1.xml`: a white master with a title placeholder.
fn slide_master(width_emu: u64, height_emu: u64) -> String {
    format!(
        r#"{XML_HEAD}
<p:sldMaster xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:cSld><p:bg><p:bgPr><a:solidFill><a:schemeClr val="bg1"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree>{SP_TREE_HEAD}{}</p:spTree></p:cSld><p:clrMap {CLR_MAP}/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle><a:lvl1pPr algn="l"><a:defRPr sz="4400"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill></a:defRPr></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr><a:defRPr sz="2800"/></a:lvl1pPr></p:bodyStyle><p:otherStyle><a:lvl1pPr><a:defRPr sz="1800"/></a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>"#,
        title_placeholder(width_emu, height_emu)
    )
}

/// `ppt/slideMasters/_rels/slideMaster1.xml.rels`.
const SLIDE_MASTER_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#;

/// `ppt/slideLayouts/slideLayout1.xml`: the layout every slide uses, with the title
/// placeholder.
fn slide_layout(width_emu: u64, height_emu: u64) -> String {
    format!(
        r#"{XML_HEAD}
<p:sldLayout xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}" type="titleOnly" preserve="1"><p:cSld name="Title Only"><p:spTree>{SP_TREE_HEAD}{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#,
        title_placeholder(width_emu, height_emu)
    )
}

/// `ppt/slideLayouts/_rels/slideLayout1.xml.rels`.
const SLIDE_LAYOUT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#;

/// `ppt/theme/theme1.xml` (and `theme2.xml`, the notes master's): the smallest theme
/// PowerPoint accepts, with its three fill, line, effect and background styles.
const THEME: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="presenter"><a:themeElements><a:clrScheme name="presenter"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="1F2A44"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="2F5BD6"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="4472C4"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="presenter"><a:majorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="presenter"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="6350" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln><a:ln w="12700" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln><a:ln w="19050" cap="flat" cmpd="sng" algn="ctr"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:prstDash val="solid"/></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"#;

/// `ppt/notesMasters/notesMaster1.xml`: the notes page, with the slide's picture (in the
/// slide's aspect ratio) above the notes text.
fn notes_master(width_emu: u64, height_emu: u64) -> String {
    let image_width = 5 * EMU_PER_INCH;
    // `height_emu / width_emu` is within [1/56, 56], so the product fits comfortably.
    let image_height = (image_width as f64 * height_emu as f64 / width_emu as f64).round() as u64;
    let image_x = (NOTES_WIDTH_EMU - image_width) / 2;
    let image_y = 685_800;
    let text_x = 685_800;
    let text_y = image_y + image_height + 457_200;
    let text_width = NOTES_WIDTH_EMU - 2 * text_x;
    let text_height = NOTES_HEIGHT_EMU.saturating_sub(text_y + 685_800);
    format!(
        r#"{XML_HEAD}
<p:notesMaster xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:cSld><p:bg><p:bgPr><a:solidFill><a:schemeClr val="bg1"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree>{SP_TREE_HEAD}<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder 1"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr><p:nvPr><p:ph type="sldImg" idx="2"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="{image_x}" y="{image_y}"/><a:ext cx="{image_width}" cy="{image_height}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln w="12700"><a:solidFill><a:prstClr val="black"/></a:solidFill></a:ln></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder 2"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="body" sz="quarter" idx="3"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="{text_x}" y="{text_y}"/><a:ext cx="{text_width}" cy="{text_height}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr vert="horz" lIns="91440" tIns="45720" rIns="91440" bIns="45720" rtlCol="0"/><a:lstStyle/><a:p><a:endParaRPr/></a:p></p:txBody></p:sp></p:spTree></p:cSld><p:clrMap {CLR_MAP}/><p:notesStyle><a:lvl1pPr marL="0" algn="l" defTabSz="914400" rtl="0" eaLnBrk="1" latinLnBrk="0" hangingPunct="1"><a:defRPr sz="1200" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/><a:cs typeface="+mn-cs"/></a:defRPr></a:lvl1pPr></p:notesStyle></p:notesMaster>"#
    )
}

/// `ppt/notesMasters/_rels/notesMaster1.xml.rels`.
const NOTES_MASTER_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme2.xml"/></Relationships>"#;

/// A slide part as written: its XML and the relationships its shapes need (pictures and
/// hyperlinks), `(id, type, target)`.
struct WrittenSlide {
    xml: String,
    rels: Vec<(String, &'static str, String)>,
}

/// The writer of one slide's shape tree.
struct SlideWriter<'a> {
    scale: &'a Scale,
    media: &'a mut Media,
    /// The relationships added so far (`rId1` is the layout).
    rels: Vec<(String, &'static str, String)>,
    /// Hyperlink targets already given a relationship.
    links: HashMap<String, String>,
    /// The next shape id.
    next_id: u32,
    xml: String,
}

impl SlideWriter<'_> {
    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The relationship id of a media part for `image`.
    fn image_rel(&mut self, image: &Image) -> String {
        let number = self.media.number_of(image);
        let target = format!("../media/image{number}.{}", image.kind.extension());
        if let Some((id, ..)) = self.rels.iter().find(|(_, _, t)| *t == target) {
            return id.clone();
        }
        let id = format!("rId{}", self.rels.len() + 2);
        self.rels.push((id.clone(), REL_IMAGE, target));
        id
    }

    /// The relationship id of the hyperlink `url`.
    fn link_rel(&mut self, url: &str) -> String {
        if let Some(id) = self.links.get(url) {
            return id.clone();
        }
        let id = format!("rId{}", self.rels.len() + 2);
        self.rels.push((id.clone(), REL_HYPERLINK, url.to_string()));
        self.links.insert(url.to_string(), id.clone());
        id
    }

    fn xfrm(&self, frame: &Frame) -> String {
        format!(
            r#"<a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm>"#,
            self.scale.pos(frame.x),
            self.scale.pos(frame.y),
            self.scale.emu(frame.w).max(1),
            self.scale.emu(frame.h).max(1)
        )
    }

    /// The geometry: a rectangle, rounded when `radius` is given.
    fn geometry(&self, frame: &Frame, radius: Option<f64>) -> String {
        match radius {
            Some(r) if r > 0.0 && frame.w > 0.0 && frame.h > 0.0 => {
                let adj = (r / frame.w.min(frame.h) * 50_000.0)
                    .round()
                    .clamp(0.0, 50_000.0);
                format!(
                    r#"<a:prstGeom prst="roundRect"><a:avLst><a:gd name="adj" fmla="val {}"/></a:avLst></a:prstGeom>"#,
                    adj as i64
                )
            }
            _ => r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>"#.to_string(),
        }
    }

    fn fill_xml(&self, fill: Option<&Fill>) -> String {
        match fill {
            None => "<a:noFill/>".to_string(),
            Some(Fill::Color { color }) => {
                format!("<a:solidFill>{}</a:solidFill>", color_xml(color))
            }
            Some(Fill::Gradient { gradient }) => gradient_xml(gradient),
        }
    }

    fn line_xml(&self, line: Option<&Line>) -> String {
        match line {
            None => "<a:ln><a:noFill/></a:ln>".to_string(),
            Some(line) => {
                let dash = if line.dash {
                    r#"<a:prstDash val="dash"/>"#
                } else {
                    ""
                };
                format!(
                    r#"<a:ln w="{}"><a:solidFill>{}</a:solidFill>{dash}</a:ln>"#,
                    self.scale.emu(line.width).max(1),
                    color_xml(&line.color)
                )
            }
        }
    }

    fn text(&mut self, shape: &TextShape) {
        let id = self.id();
        let name = escape(&cut(&single_line(&first_text(shape)), 40));
        let nv = if shape.title {
            format!(
                r#"<p:nvSpPr><p:cNvPr id="{id}" name="Title {id}"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>"#
            )
        } else {
            format!(
                r#"<p:nvSpPr><p:cNvPr id="{id}" name="TextBox {id}: {name}"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr>"#
            )
        };
        let wrap = if shape.wrap { "square" } else { "none" };
        let mut body = format!(
            r#"<p:txBody><a:bodyPr wrap="{wrap}" lIns="{}" tIns="{}" rIns="{}" bIns="{}" rtlCol="0" anchor="t"><a:spAutoFit/></a:bodyPr><a:lstStyle/>"#,
            self.scale.emu(shape.insets.l),
            self.scale.emu(shape.insets.t),
            self.scale.emu(shape.insets.r),
            self.scale.emu(shape.insets.b)
        );
        for paragraph in &shape.paragraphs {
            body.push_str(&self.paragraph_xml(paragraph));
        }
        if shape.paragraphs.is_empty() {
            body.push_str("<a:p><a:endParaRPr/></a:p>");
        }
        body.push_str("</p:txBody>");
        let _ = write!(
            self.xml,
            "<p:sp>{nv}<p:spPr>{}{}{}{}</p:spPr>{body}</p:sp>",
            self.xfrm(&shape.frame),
            self.geometry(&shape.frame, shape.radius),
            self.fill_xml(shape.fill.as_ref()),
            self.line_xml(shape.line.as_ref())
        );
    }

    fn rect(&mut self, shape: &RectShape) {
        let id = self.id();
        let _ = write!(
            self.xml,
            r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Rectangle {id}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}{}{}{}</p:spPr></p:sp>"#,
            self.xfrm(&shape.frame),
            self.geometry(&shape.frame, shape.radius),
            self.fill_xml(shape.fill.as_ref()),
            self.line_xml(shape.line.as_ref())
        );
    }

    fn picture(&mut self, shape: &PictureShape) {
        let Some(image) = &shape.image else {
            return;
        };
        let id = self.id();
        let rel = self.image_rel(image);
        let descr = escape(&cut(&single_line(&shape.alt), MAX_DESCRIPTION_CHARS));
        let _ = write!(
            self.xml,
            r#"<p:pic><p:nvPicPr><p:cNvPr id="{id}" name="Picture {id}" descr="{descr}"/><p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr>{}<p:spPr>{}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>"#,
            blip_fill(&rel, shape.alpha, shape.crop.as_ref(), shape.fit.as_ref()),
            self.xfrm(&shape.frame)
        );
    }

    fn table(&mut self, shape: &TableShape) {
        if shape.columns.is_empty() || shape.rows.is_empty() {
            return;
        }
        let id = self.id();
        let mut xml = format!(
            r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="{id}" name="Table {id}"/><p:cNvGraphicFramePr><a:graphicFrameLocks noGrp="1"/></p:cNvGraphicFramePr><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr/><a:tblGrid>"#,
            self.scale.pos(shape.frame.x),
            self.scale.pos(shape.frame.y),
            self.scale.emu(shape.frame.w).max(1),
            self.scale.emu(shape.frame.h).max(1)
        );
        for width in &shape.columns {
            let _ = write!(xml, r#"<a:gridCol w="{}"/>"#, self.scale.emu(*width).max(1));
        }
        xml.push_str("</a:tblGrid>");
        for row in &shape.rows {
            let _ = write!(xml, r#"<a:tr h="{}">"#, self.scale.emu(row.height).max(1));
            for column in 0..shape.columns.len() {
                let cell = row.cells.get(column).cloned().unwrap_or_default();
                let mut attrs = String::new();
                if cell.grid_span > 1 {
                    let _ = write!(attrs, r#" gridSpan="{}""#, cell.grid_span);
                }
                if cell.row_span > 1 {
                    let _ = write!(attrs, r#" rowSpan="{}""#, cell.row_span);
                }
                if cell.h_merge {
                    attrs.push_str(r#" hMerge="1""#);
                }
                if cell.v_merge {
                    attrs.push_str(r#" vMerge="1""#);
                }
                let mut body = String::from("<a:txBody><a:bodyPr/><a:lstStyle/>");
                for paragraph in &cell.paragraphs {
                    body.push_str(&self.paragraph_xml(paragraph));
                }
                if cell.paragraphs.is_empty() {
                    body.push_str("<a:p><a:endParaRPr/></a:p>");
                }
                body.push_str("</a:txBody>");
                let anchor = match cell.anchor.as_deref() {
                    Some("ctr") => "ctr",
                    Some("b") => "b",
                    _ => "t",
                };
                let mut pr = format!(
                    r#"<a:tcPr marL="{}" marR="{}" marT="{}" marB="{}" anchor="{anchor}">"#,
                    self.scale.emu(cell.insets.l),
                    self.scale.emu(cell.insets.r),
                    self.scale.emu(cell.insets.t),
                    self.scale.emu(cell.insets.b)
                );
                for (tag, line) in [
                    ("lnL", &cell.borders.l),
                    ("lnR", &cell.borders.r),
                    ("lnT", &cell.borders.t),
                    ("lnB", &cell.borders.b),
                ] {
                    match line {
                        Some(line) => {
                            let _ = write!(
                                pr,
                                r#"<a:{tag} w="{}"><a:solidFill>{}</a:solidFill></a:{tag}>"#,
                                self.scale.emu(line.width).max(1),
                                color_xml(&line.color)
                            );
                        }
                        None => {
                            let _ = write!(pr, r#"<a:{tag} w="0"><a:noFill/></a:{tag}>"#);
                        }
                    }
                }
                if let Some(fill) = &cell.fill {
                    pr.push_str(&self.fill_xml(Some(fill)));
                }
                pr.push_str("</a:tcPr>");
                let _ = write!(xml, "<a:tc{attrs}>{body}{pr}</a:tc>");
            }
            xml.push_str("</a:tr>");
        }
        xml.push_str("</a:tbl></a:graphicData></a:graphic></p:graphicFrame>");
        self.xml.push_str(&xml);
    }

    fn paragraph_xml(&mut self, paragraph: &Paragraph) -> String {
        let mut pr = format!(
            r#"<a:pPr marL="{}" indent="{}" lvl="{}" algn="{}">"#,
            self.scale.emu(paragraph.margin_left),
            self.scale.pos(paragraph.indent),
            paragraph.level.min(8),
            match paragraph.align {
                Align::L => "l",
                Align::Ctr => "ctr",
                Align::R => "r",
                Align::Just => "just",
            }
        );
        if let Some(height) = paragraph.line_height.filter(|h| *h > 0.0) {
            let _ = write!(
                pr,
                r#"<a:lnSpc><a:spcPts val="{}"/></a:lnSpc>"#,
                self.scale.pt100(height)
            );
        }
        if paragraph.space_before > 0.0 {
            let _ = write!(
                pr,
                r#"<a:spcBef><a:spcPts val="{}"/></a:spcBef>"#,
                self.scale.pt100_signed(paragraph.space_before).max(0)
            );
        }
        match &paragraph.bullet {
            Some(Bullet::Char { glyph, color }) => {
                if let Some(color) = color {
                    let _ = write!(pr, "<a:buClr>{}</a:buClr>", color_xml(color));
                }
                let _ = write!(
                    pr,
                    r#"<a:buFont typeface="Arial"/><a:buChar char="{}"/>"#,
                    escape(&cut(glyph, 4))
                );
            }
            Some(Bullet::Auto {
                scheme,
                start,
                color,
            }) => {
                if let Some(color) = color {
                    let _ = write!(pr, "<a:buClr>{}</a:buClr>", color_xml(color));
                }
                let scheme = if AUTO_NUMBER_SCHEMES.contains(&scheme.as_str()) {
                    scheme.as_str()
                } else {
                    "arabicPeriod"
                };
                let _ = write!(
                    pr,
                    r#"<a:buFont typeface="+mj-lt"/><a:buAutoNum type="{scheme}" startAt="{}"/>"#,
                    (*start).clamp(1, 32_767)
                );
            }
            None => pr.push_str("<a:buNone/>"),
        }
        pr.push_str("</a:pPr>");
        let mut xml = format!("<a:p>{pr}");
        let mut last_size = None;
        for run in &paragraph.runs {
            match run {
                Run::Break { .. } => {
                    let _ = write!(
                        xml,
                        r#"<a:br><a:rPr lang="en-US"{}/></a:br>"#,
                        last_size.map_or(String::new(), |sz| format!(r#" sz="{sz}""#))
                    );
                }
                Run::Text(run) => {
                    let text = clean(&run.text);
                    if text.is_empty() {
                        continue;
                    }
                    let sz = self.scale.pt100(run.size);
                    last_size = Some(sz);
                    xml.push_str(&self.run_xml(run, &text, sz));
                }
            }
        }
        let _ = write!(
            xml,
            r#"<a:endParaRPr lang="en-US"{}/></a:p>"#,
            last_size.map_or(String::new(), |sz| format!(r#" sz="{sz}""#))
        );
        xml
    }

    fn run_xml(&mut self, run: &TextRun, text: &str, sz: i64) -> String {
        let mut attrs = format!(r#" lang="en-US" sz="{sz}""#);
        if run.bold {
            attrs.push_str(r#" b="1""#);
        }
        if run.italic {
            attrs.push_str(r#" i="1""#);
        }
        if run.underline {
            attrs.push_str(r#" u="sng""#);
        }
        if run.strike {
            attrs.push_str(r#" strike="sngStrike""#);
        }
        match run.caps.as_deref() {
            Some("all") => attrs.push_str(r#" cap="all""#),
            Some("small") => attrs.push_str(r#" cap="small""#),
            _ => {}
        }
        if run.spacing != 0.0 {
            let _ = write!(
                attrs,
                r#" spc="{}""#,
                self.scale.pt100_signed(run.spacing).clamp(-40_000, 40_000)
            );
        }
        if run.baseline != 0 {
            let _ = write!(
                attrs,
                r#" baseline="{}""#,
                run.baseline.clamp(-100_000, 100_000)
            );
        }
        let mut inner = format!("<a:solidFill>{}</a:solidFill>", color_xml(&run.color));
        if let Some(highlight) = &run.highlight {
            let _ = write!(
                inner,
                r#"<a:highlight><a:srgbClr val="{}"/></a:highlight>"#,
                hex(highlight)
            );
        }
        let font = escape(&cut(&single_line(&run.font), 60));
        let _ = write!(
            inner,
            r#"<a:latin typeface="{font}"/><a:ea typeface="{font}"/><a:cs typeface="{font}"/>"#
        );
        if let Some(link) = run.link.as_deref().filter(|l| is_link(l)) {
            let id = self.link_rel(link);
            let _ = write!(inner, r#"<a:hlinkClick r:id="{id}"/>"#);
        }
        format!(
            "<a:r><a:rPr{attrs} dirty=\"0\">{inner}</a:rPr><a:t>{}</a:t></a:r>",
            escape(text)
        )
    }
}

/// The numbering schemes written as given (anything else becomes `arabicPeriod`).
const AUTO_NUMBER_SCHEMES: &[&str] = &[
    "arabicPeriod",
    "arabicParenR",
    "arabicParenBoth",
    "arabicPlain",
    "alphaLcPeriod",
    "alphaLcParenR",
    "alphaLcParenBoth",
    "alphaUcPeriod",
    "alphaUcParenR",
    "alphaUcParenBoth",
    "romanLcPeriod",
    "romanLcParenR",
    "romanLcParenBoth",
    "romanUcPeriod",
    "romanUcParenR",
    "romanUcParenBoth",
];

/// Whether `url` can be a hyperlink target PowerPoint opens.
fn is_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:"))
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && url.len() < 2048
}

/// The first words of a text box, for the shape's name.
fn first_text(shape: &TextShape) -> String {
    let mut text = String::new();
    for paragraph in &shape.paragraphs {
        for run in &paragraph.runs {
            if let Run::Text(run) = run {
                text.push_str(&run.text);
                if text.len() > 60 {
                    return text;
                }
            }
        }
        text.push(' ');
    }
    text
}

/// A `blipFill` for a picture: the media relationship, its opacity, crop and fit.
fn blip_fill(rel: &str, alpha: Option<f64>, crop: Option<&Edges>, fit: Option<&Edges>) -> String {
    let alpha = alpha
        .filter(|a| *a < 1.0)
        .map(|a| {
            format!(
                r#"<a:alphaModFix amt="{}"/>"#,
                (a.clamp(0.0, 1.0) * 100_000.0).round() as i64
            )
        })
        .unwrap_or_default();
    let pct = |f: f64| ((f.clamp(0.0, 1.0)) * 100_000.0).round() as i64;
    let src = crop
        .map(|c| {
            format!(
                r#"<a:srcRect l="{}" t="{}" r="{}" b="{}"/>"#,
                pct(c.l),
                pct(c.t),
                pct(c.r),
                pct(c.b)
            )
        })
        .unwrap_or_default();
    let fill_rect = fit
        .map(|f| {
            format!(
                r#"<a:fillRect l="{}" t="{}" r="{}" b="{}"/>"#,
                pct(f.l),
                pct(f.t),
                pct(f.r),
                pct(f.b)
            )
        })
        .unwrap_or_else(|| "<a:fillRect/>".to_string());
    format!(
        r#"<p:blipFill><a:blip r:embed="{rel}">{alpha}</a:blip>{src}<a:stretch>{fill_rect}</a:stretch></p:blipFill>"#
    )
}

fn hex(color: &Rgba) -> String {
    format!("{:02X}{:02X}{:02X}", color.r, color.g, color.b)
}

/// An `srgbClr` with its alpha when below 1.
fn color_xml(color: &Rgba) -> String {
    let alpha = color.a.clamp(0.0, 1.0);
    if alpha < 1.0 {
        format!(
            r#"<a:srgbClr val="{}"><a:alpha val="{}"/></a:srgbClr>"#,
            hex(color),
            (alpha * 100_000.0).round() as i64
        )
    } else {
        format!(r#"<a:srgbClr val="{}"/>"#, hex(color))
    }
}

/// A linear gradient fill. DrawingML's angle is measured from "to the right" clockwise;
/// CSS's from "to the top".
fn gradient_xml(gradient: &Gradient) -> String {
    let mut xml = String::from(r#"<a:gradFill rotWithShape="1"><a:gsLst>"#);
    let mut stops = gradient.stops.clone();
    if stops.len() < 2 {
        let only = stops.first().map(|s| s.color).unwrap_or(Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 1.0,
        });
        stops = vec![
            Stop {
                color: only,
                pos: 0.0,
            },
            Stop {
                color: only,
                pos: 1.0,
            },
        ];
    }
    for stop in &stops {
        let _ = write!(
            xml,
            r#"<a:gs pos="{}">{}</a:gs>"#,
            (stop.pos.clamp(0.0, 1.0) * 100_000.0).round() as i64,
            color_xml(&stop.color)
        );
    }
    let angle = ((gradient.angle - 90.0).rem_euclid(360.0) * 60_000.0).round() as i64;
    let _ = write!(
        xml,
        r#"</a:gsLst><a:lin ang="{angle}" scaled="0"/></a:gradFill>"#
    );
    xml
}

/// `ppt/slides/slideN.xml`: the background and the shapes.
fn slide_xml(slide: &Slide, number: usize, scale: &Scale, media: &mut Media) -> WrittenSlide {
    let mut writer = SlideWriter {
        scale,
        media,
        rels: Vec::new(),
        links: HashMap::new(),
        next_id: 2,
        xml: String::new(),
    };
    // The background picture covers the slide, behind everything.
    if let Some(image) = &slide.background.image {
        let (mut crop, mut fit) = (None, None);
        if let (Some(sizing), Some(natural)) =
            (slide.background.sizing.as_deref(), image.dimensions())
        {
            let position = slide.background.position.as_deref().unwrap_or("50% 50%");
            let width = scale.width_emu as f64 / scale.emu_per_px;
            let height = scale.height_emu as f64 / scale.emu_per_px;
            (crop, fit) = place_background(sizing, position, natural, width, height);
        }
        let picture = PictureShape {
            frame: Frame {
                x: 0.0,
                y: 0.0,
                w: scale.width_emu as f64 / scale.emu_per_px,
                h: scale.height_emu as f64 / scale.emu_per_px,
            },
            image: Some(image.clone()),
            alt: "Background".into(),
            alpha: slide.background.alpha,
            crop,
            fit,
            ..PictureShape::default()
        };
        writer.picture(&picture);
    }
    for shape in &slide.shapes {
        match shape {
            Shape::Text(text) => writer.text(text),
            Shape::Picture(picture) => writer.picture(picture),
            Shape::Rect(rect) => writer.rect(rect),
            Shape::Table(table) => writer.table(table),
        }
    }
    let background = match &slide.background.color {
        Some(color) => format!(
            "<p:bg><p:bgPr><a:solidFill>{}</a:solidFill><a:effectLst/></p:bgPr></p:bg>",
            color_xml(&Rgba { a: 1.0, ..*color })
        ),
        None => String::new(),
    };
    let xml = format!(
        r#"{XML_HEAD}
<p:sld xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:cSld name="{}">{background}<p:spTree>{SP_TREE_HEAD}{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#,
        slide_name(&slide.name, number),
        writer.xml
    );
    WrittenSlide {
        xml,
        rels: writer.rels,
    }
}

/// The slide's `name` attribute: `name` on one line, cut after [`MAX_NAME_CHARS`]
/// characters, escaped; `Slide <number>` when it is blank.
fn slide_name(name: &str, number: usize) -> String {
    let name = cut(&single_line(name), MAX_NAME_CHARS);
    if name.is_empty() {
        format!("Slide {number}")
    } else {
        escape(&name)
    }
}

/// `ppt/slides/_rels/slideN.xml.rels`: the layout, the pictures, the hyperlinks and the
/// notes slide.
fn slide_rels(number: usize, rels: &[(String, &'static str, String)], with_notes: bool) -> String {
    let mut xml = format!(
        r#"{XML_HEAD}
<Relationships xmlns="{NS_RELS}"><Relationship Id="rId1" Type="{REL_SLIDE_LAYOUT}" Target="../slideLayouts/slideLayout1.xml"/>"#
    );
    for (id, kind, target) in rels {
        let mode = if *kind == REL_HYPERLINK {
            r#" TargetMode="External""#
        } else {
            ""
        };
        let _ = write!(
            xml,
            r#"<Relationship Id="{id}" Type="{kind}" Target="{}"{mode}/>"#,
            escape(target)
        );
    }
    if with_notes {
        let _ = write!(
            xml,
            r#"<Relationship Id="rId{}" Type="{REL_NOTES_SLIDE}" Target="../notesSlides/notesSlide{number}.xml"/>"#,
            rels.len() + 2
        );
    }
    xml.push_str("</Relationships>");
    xml
}

/// `ppt/notesSlides/notesSlideN.xml`: the notes, one paragraph per entry.
fn notes_slide_xml(slide: &Slide) -> String {
    let mut paragraphs = String::new();
    for paragraph in &slide.notes {
        let text = escape(&clean(paragraph));
        if text.is_empty() {
            paragraphs.push_str("<a:p><a:endParaRPr/></a:p>");
        } else {
            let _ = write!(paragraphs, "<a:p><a:r><a:t>{text}</a:t></a:r></a:p>");
        }
    }
    format!(
        r#"{XML_HEAD}
<p:notes xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:cSld><p:spTree>{SP_TREE_HEAD}<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder 1"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder 2"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{paragraphs}</p:txBody></p:sp></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>"#
    )
}

/// `ppt/notesSlides/_rels/notesSlideN.xml.rels`: the notes master and the slide.
fn notes_slide_rels(number: usize) -> String {
    format!(
        r#"{XML_HEAD}
<Relationships xmlns="{NS_RELS}"><Relationship Id="rId1" Type="{REL_NOTES_MASTER}" Target="../notesMasters/notesMaster1.xml"/><Relationship Id="rId2" Type="{REL_SLIDE}" Target="../slides/slide{number}.xml"/></Relationships>"#
    )
}

// --- Text ----------------------------------------------------------------------------------

/// `text` with its XML special characters escaped (for element content and attribute
/// values alike). Characters XML cannot hold are dropped ([`clean`]).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in clean(text).chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// `text` without the characters XML 1.0 cannot hold: control characters other than tab,
/// line feed and carriage return, and the non-characters U+FFFE and U+FFFF.
fn clean(text: &str) -> String {
    text.chars().filter(|&c| is_xml_char(c)).collect()
}

/// Whether XML 1.0 can hold `c` (its `Char` production).
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

/// `text` on one line: its white space (line breaks included) collapsed to single spaces,
/// trimmed.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `max` characters of `text`, with an ellipsis when something was cut.
fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", text[..at].trim_end()),
        None => text.to_string(),
    }
}

// --- ZIP -------------------------------------------------------------------------------------

/// A ZIP archive being written (module docs): stored entries, fixed timestamps, no ZIP64.
#[derive(Default)]
struct Zip {
    /// The archive so far: the local headers and data of the entries added.
    bytes: Vec<u8>,
    /// The central directory records of the entries added.
    directory: Vec<u8>,
    /// How many entries were added.
    entries: u16,
}

/// The modification date of every entry: 1980-01-01, the earliest a ZIP can hold.
const DOS_DATE: u16 = (1 << 5) | 1;

/// Its time: midnight.
const DOS_TIME: u16 = 0;

impl Zip {
    /// Adds the entry `name` with `data`, stored.
    fn add(&mut self, name: &str, data: &[u8]) -> Result<()> {
        let too_big = |what: &str| {
            Error::Other(format!(
                "the presentation is too large for a .pptx file ({what}; 4 GiB or 65 535 \
                 parts at most)"
            ))
        };
        let size = u32::try_from(data.len()).map_err(|_| too_big("a part of 4 GiB or more"))?;
        let offset = u32::try_from(self.bytes.len()).map_err(|_| too_big("more than 4 GiB"))?;
        let name_len = u16::try_from(name.len()).map_err(|_| too_big("a part name too long"))?;
        if self.entries == u16::MAX {
            return Err(too_big("too many parts"));
        }
        let crc = crc32(data);
        let fixed = |out: &mut Vec<u8>| {
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed: 2.0
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            out.extend_from_slice(&DOS_TIME.to_le_bytes());
            out.extend_from_slice(&DOS_DATE.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes()); // compressed size
            out.extend_from_slice(&size.to_le_bytes()); // uncompressed size
            out.extend_from_slice(&name_len.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        };
        self.bytes.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        fixed(&mut self.bytes);
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.extend_from_slice(data);
        self.directory
            .extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        self.directory.extend_from_slice(&20u16.to_le_bytes()); // version made by: MS-DOS, 2.0
        fixed(&mut self.directory);
        self.directory.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.directory.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        self.directory.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        self.directory.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        self.directory.extend_from_slice(&offset.to_le_bytes());
        self.directory.extend_from_slice(name.as_bytes());
        self.entries += 1;
        Ok(())
    }

    /// Appends the central directory and the end record; the archive's bytes.
    fn finish(mut self) -> Result<Vec<u8>> {
        let too_big = || {
            Error::Other("the presentation is too large for a .pptx file (more than 4 GiB)".into())
        };
        let offset = u32::try_from(self.bytes.len()).map_err(|_| too_big())?;
        let size = u32::try_from(self.directory.len()).map_err(|_| too_big())?;
        u32::try_from(self.bytes.len() + self.directory.len() + 22).map_err(|_| too_big())?;
        self.bytes.append(&mut self.directory);
        self.bytes.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes()); // this disk
        self.bytes.extend_from_slice(&0u16.to_le_bytes()); // disk of the directory
        self.bytes.extend_from_slice(&self.entries.to_le_bytes()); // entries on this disk
        self.bytes.extend_from_slice(&self.entries.to_le_bytes()); // entries in all
        self.bytes.extend_from_slice(&size.to_le_bytes());
        self.bytes.extend_from_slice(&offset.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes()); // comment length
        Ok(self.bytes)
    }
}

/// The CRC-32 (IEEE 802.3, as ZIP uses it) of `data`.
fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = crc32_table();
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = TABLE[usize::from((crc as u8) ^ byte)] ^ (crc >> 8);
    }
    !crc
}

/// The byte table of [`crc32`] (reflected polynomial 0xEDB88320).
const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 PNG (transparent).
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x7F, 0x9B, 0x6B, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// The entries of a ZIP written by [`Zip`] (stored entries only), read back through its
    /// central directory: `(name, data)` in directory order.
    fn entries(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let u16_at = |at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(zip[at..at + 4].try_into().unwrap());
        let end = zip.len() - 22;
        assert_eq!(u32_at(end), 0x0605_4b50, "end of central directory");
        let count = usize::from(u16_at(end + 10));
        let directory_size = u32_at(end + 12) as usize;
        let mut at = u32_at(end + 16) as usize;
        assert_eq!(at + directory_size, end);
        let mut found = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(at), 0x0201_4b50, "central directory record");
            assert_eq!(u16_at(at + 10), 0, "stored");
            let crc = u32_at(at + 16);
            let size = u32_at(at + 24) as usize;
            let name_len = usize::from(u16_at(at + 28));
            let offset = u32_at(at + 42) as usize;
            let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
            at += 46 + name_len;
            assert_eq!(u32_at(offset), 0x0403_4b50, "local header of {name}");
            let local_name_len = usize::from(u16_at(offset + 26));
            let extra_len = usize::from(u16_at(offset + 28));
            assert_eq!(
                &zip[offset + 30..offset + 30 + local_name_len],
                name.as_bytes()
            );
            let start = offset + 30 + local_name_len + extra_len;
            let data = zip[start..start + size].to_vec();
            assert_eq!(crc32(&data), crc, "crc of {name}");
            found.push((name, data));
        }
        found
    }

    fn text(entries: &[(String, Vec<u8>)], name: &str) -> String {
        let (_, data) = entries
            .iter()
            .find(|(entry, _)| entry == name)
            .unwrap_or_else(|| panic!("no entry {name}"));
        String::from_utf8(data.clone()).unwrap()
    }

    fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r, g, b, a: 1.0 }
    }

    fn run(text: &str) -> Run {
        Run::Text(TextRun {
            text: text.to_string(),
            font: "Arial".into(),
            size: 24.0,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            color: rgb(0, 0, 0),
            highlight: None,
            link: None,
            caps: None,
            baseline: 0,
            spacing: 0.0,
        })
    }

    fn text_shape(text: &str, title: bool) -> Shape {
        Shape::Text(TextShape {
            frame: Frame {
                x: 60.0,
                y: 40.0,
                w: 1160.0,
                h: 80.0,
            },
            title,
            paragraphs: vec![Paragraph {
                runs: vec![run(text)],
                ..Paragraph::default()
            }],
            ..TextShape::default()
        })
    }

    fn slide(name: &str, shapes: Vec<Shape>, notes: &[&str]) -> Slide {
        Slide {
            name: name.to_string(),
            background: Background::default(),
            shapes,
            notes: notes.iter().map(|line| line.to_string()).collect(),
        }
    }

    fn presentation(slides: Vec<Slide>) -> Presentation {
        Presentation {
            title: Some("A <deck>".into()),
            author: Some("Ada & Bob".into()),
            width: 1280.0,
            height: 720.0,
            slides,
        }
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn the_archive_holds_every_part_with_the_content_types_first() {
        let picture = Shape::Picture(PictureShape {
            frame: Frame {
                x: 100.0,
                y: 200.0,
                w: 300.0,
                h: 150.0,
            },
            image: Some(Image {
                bytes: PNG.to_vec(),
                kind: ImageKind::Png,
            }),
            alt: "a dot".into(),
            ..PictureShape::default()
        });
        let file = write(&presentation(vec![
            slide(
                "One",
                vec![text_shape("Heading", true), picture.clone()],
                &["First line", "", "Third line"],
            ),
            slide("Two", vec![picture], &[]),
        ]))
        .unwrap();
        assert!(file.starts_with(b"PK\x03\x04"));
        let entries = entries(&file);
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names[0], "[Content_Types].xml");
        for expected in [
            "_rels/.rels",
            "docProps/core.xml",
            "docProps/app.xml",
            "ppt/presentation.xml",
            "ppt/_rels/presentation.xml.rels",
            "ppt/slideMasters/slideMaster1.xml",
            "ppt/slideMasters/_rels/slideMaster1.xml.rels",
            "ppt/slideLayouts/slideLayout1.xml",
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            "ppt/theme/theme1.xml",
            "ppt/theme/theme2.xml",
            "ppt/notesMasters/notesMaster1.xml",
            "ppt/notesMasters/_rels/notesMaster1.xml.rels",
            "ppt/slides/slide1.xml",
            "ppt/slides/_rels/slide1.xml.rels",
            "ppt/notesSlides/notesSlide1.xml",
            "ppt/notesSlides/_rels/notesSlide1.xml.rels",
            "ppt/slides/slide2.xml",
            "ppt/slides/_rels/slide2.xml.rels",
            "ppt/media/image1.png",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
        // The second slide has no notes: no notes slide, and no relationship to one.
        assert!(!names.contains(&"ppt/notesSlides/notesSlide2.xml"));
        assert!(!text(&entries, "ppt/slides/_rels/slide2.xml.rels").contains("notesSlide"));
        let rels1 = text(&entries, "ppt/slides/_rels/slide1.xml.rels");
        assert!(rels1.contains("notesSlide1.xml"), "{rels1}");
        assert!(rels1.contains(r#"Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png""#), "{rels1}");
        // Identical pictures share one media part.
        assert_eq!(
            names.iter().filter(|n| n.starts_with("ppt/media/")).count(),
            1
        );
        // The content types name every slide and notes slide, and the picture formats.
        let types = text(&entries, "[Content_Types].xml");
        assert!(types.contains("/ppt/slides/slide2.xml"));
        assert!(types.contains("/ppt/notesSlides/notesSlide1.xml"));
        assert!(!types.contains("/ppt/notesSlides/notesSlide2.xml"));
        for ext in ["png", "jpeg", "gif"] {
            assert!(types.contains(&format!(r#"Extension="{ext}""#)), "{types}");
        }
        // The first slide: a title placeholder with the heading, then the picture.
        let slide1 = text(&entries, "ppt/slides/slide1.xml");
        assert!(
            slide1.contains(r#"<p:nvPr><p:ph type="title"/></p:nvPr>"#),
            "{slide1}"
        );
        assert!(slide1.contains("<a:t>Heading</a:t>"), "{slide1}");
        assert!(
            slide1.contains(r#"<p:cNvPr id="3" name="Picture 3" descr="a dot"/>"#),
            "{slide1}"
        );
        // 100 px at 1280 px = 12192000 EMU: 952500 EMU.
        assert!(
            slide1.contains(r#"<a:off x="952500" y="1905000"/><a:ext cx="2857500" cy="1428750"/>"#),
            "{slide1}"
        );
    }

    #[test]
    fn text_runs_carry_their_formatting() {
        let mut shape = TextShape {
            frame: Frame {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 100.0,
            },
            insets: Edges {
                l: 10.0,
                t: 5.0,
                r: 10.0,
                b: 5.0,
            },
            fill: Some(Fill::Color {
                color: Rgba {
                    r: 240,
                    g: 240,
                    b: 240,
                    a: 0.5,
                },
            }),
            line: Some(Line {
                color: rgb(200, 0, 0),
                width: 2.0,
                dash: true,
            }),
            radius: Some(8.0),
            wrap: false,
            ..TextShape::default()
        };
        let Run::Text(mut fancy) = run("Q&A <b>") else {
            unreachable!()
        };
        fancy.bold = true;
        fancy.italic = true;
        fancy.underline = true;
        fancy.strike = true;
        fancy.caps = Some("all".into());
        fancy.baseline = 30000;
        fancy.spacing = -1.0;
        fancy.highlight = Some(rgb(255, 255, 0));
        fancy.link = Some("https://example.org/?a=1&b=2".into());
        fancy.font = "Menlo".into();
        fancy.color = Rgba {
            r: 1,
            g: 2,
            b: 3,
            a: 0.4,
        };
        shape.paragraphs = vec![
            Paragraph {
                align: Align::Ctr,
                level: 1,
                bullet: Some(Bullet::Char {
                    glyph: "•".into(),
                    color: Some(rgb(47, 91, 214)),
                }),
                margin_left: 40.0,
                indent: -20.0,
                line_height: Some(32.0),
                space_before: 8.0,
                runs: vec![
                    Run::Text(fancy),
                    Run::Break { br: true },
                    run("plain\u{0}text"),
                ],
            },
            Paragraph {
                bullet: Some(Bullet::Auto {
                    scheme: "alphaLcPeriod".into(),
                    start: 3,
                    color: None,
                }),
                runs: vec![],
                ..Paragraph::default()
            },
        ];
        let file = write(&presentation(vec![slide(
            "S",
            vec![Shape::Text(shape)],
            &[],
        )]))
        .unwrap();
        let entries = entries(&file);
        let xml = text(&entries, "ppt/slides/slide1.xml");
        assert!(xml.contains(r#"<a:bodyPr wrap="none" lIns="95250" tIns="47625" rIns="95250" bIns="47625" rtlCol="0" anchor="t"><a:spAutoFit/></a:bodyPr>"#), "{xml}");
        assert!(xml.contains(r#"<a:prstGeom prst="roundRect"><a:avLst><a:gd name="adj" fmla="val 4000"/></a:avLst></a:prstGeom><a:solidFill><a:srgbClr val="F0F0F0"><a:alpha val="50000"/></a:srgbClr></a:solidFill><a:ln w="19050"><a:solidFill><a:srgbClr val="C80000"/></a:solidFill><a:prstDash val="dash"/></a:ln>"#), "{xml}");
        // 40 px margin = 381000 EMU; 32 px line height = 24 pt; 8 px before = 6 pt.
        assert!(xml.contains(r#"<a:pPr marL="381000" indent="-190500" lvl="1" algn="ctr"><a:lnSpc><a:spcPts val="2400"/></a:lnSpc><a:spcBef><a:spcPts val="600"/></a:spcBef><a:buClr><a:srgbClr val="2F5BD6"/></a:buClr><a:buFont typeface="Arial"/><a:buChar char="•"/></a:pPr>"#), "{xml}");
        assert!(xml.contains(r#"<a:rPr lang="en-US" sz="1800" b="1" i="1" u="sng" strike="sngStrike" cap="all" spc="-75" baseline="30000" dirty="0"><a:solidFill><a:srgbClr val="010203"><a:alpha val="40000"/></a:srgbClr></a:solidFill><a:highlight><a:srgbClr val="FFFF00"/></a:highlight><a:latin typeface="Menlo"/><a:ea typeface="Menlo"/><a:cs typeface="Menlo"/><a:hlinkClick r:id="rId2"/></a:rPr><a:t>Q&amp;A &lt;b&gt;</a:t>"#), "{xml}");
        assert!(
            xml.contains(r#"<a:br><a:rPr lang="en-US" sz="1800"/></a:br>"#),
            "{xml}"
        );
        assert!(xml.contains("<a:t>plaintext</a:t>"), "{xml}");
        assert!(xml.contains(r#"<a:buFont typeface="+mj-lt"/><a:buAutoNum type="alphaLcPeriod" startAt="3"/></a:pPr><a:endParaRPr lang="en-US"/></a:p>"#), "{xml}");
        let rels = text(&entries, "ppt/slides/_rels/slide1.xml.rels");
        assert!(rels.contains(r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org/?a=1&amp;b=2" TargetMode="External"/>"#), "{rels}");
    }

    #[test]
    fn tables_backgrounds_and_gradients_are_written() {
        let cell = |text: &str| Cell {
            paragraphs: vec![Paragraph {
                runs: vec![run(text)],
                ..Paragraph::default()
            }],
            insets: Edges {
                l: 8.0,
                t: 4.0,
                r: 8.0,
                b: 4.0,
            },
            fill: Some(Fill::Color {
                color: rgb(250, 250, 250),
            }),
            borders: Borders {
                b: Some(Line {
                    color: rgb(0, 0, 0),
                    width: 1.0,
                    dash: false,
                }),
                ..Borders::default()
            },
            anchor: Some("ctr".into()),
            ..Cell::default()
        };
        let table = Shape::Table(TableShape {
            frame: Frame {
                x: 10.0,
                y: 10.0,
                w: 300.0,
                h: 60.0,
            },
            columns: vec![100.0, 200.0],
            rows: vec![
                Row {
                    height: 30.0,
                    cells: vec![
                        cell("a"),
                        Cell {
                            h_merge: true,
                            ..Cell::default()
                        },
                    ],
                },
                Row {
                    height: 30.0,
                    cells: vec![
                        Cell {
                            grid_span: 2,
                            ..cell("b")
                        },
                        Cell::default(),
                    ],
                },
            ],
        });
        let rect = Shape::Rect(RectShape {
            frame: Frame {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 4.0,
            },
            fill: Some(Fill::Gradient {
                gradient: Gradient {
                    angle: 90.0,
                    stops: vec![
                        Stop {
                            color: rgb(169, 109, 75),
                            pos: 0.0,
                        },
                        Stop {
                            color: Rgba {
                                r: 0,
                                g: 0,
                                b: 0,
                                a: 0.0,
                            },
                            pos: 1.0,
                        },
                    ],
                },
            }),
            line: None,
            radius: None,
        });
        let mut background = Background {
            color: Some(rgb(15, 23, 42)),
            image: Some(Image {
                bytes: PNG.to_vec(),
                kind: ImageKind::Png,
            }),
            alpha: Some(0.2),
            sizing: Some("contain".into()),
            position: Some("50% 50%".into()),
            ..Background::default()
        };
        background.shot_only = false;
        let file = write(&presentation(vec![Slide {
            name: "T".into(),
            background,
            shapes: vec![table, rect],
            notes: vec![],
        }]))
        .unwrap();
        let entries = entries(&file);
        let xml = text(&entries, "ppt/slides/slide1.xml");
        assert!(xml.contains(r#"<p:bg><p:bgPr><a:solidFill><a:srgbClr val="0F172A"/></a:solidFill><a:effectLst/></p:bgPr></p:bg>"#), "{xml}");
        // The 1×1 background picture, contained in the 16:9 slide: letterboxed left and
        // right, at 20 % opacity, first in the tree.
        assert!(
            xml.contains(
                r#"<p:pic><p:nvPicPr><p:cNvPr id="2" name="Picture 2" descr="Background"/>"#
            ),
            "{xml}"
        );
        assert!(xml.contains(r#"<a:alphaModFix amt="20000"/></a:blip><a:stretch><a:fillRect l="21875" t="0" r="21875" b="0"/></a:stretch>"#), "{xml}");
        assert!(
            xml.contains(
                r#"<a:tblGrid><a:gridCol w="952500"/><a:gridCol w="1905000"/></a:tblGrid>"#
            ),
            "{xml}"
        );
        assert!(xml.contains(r#"<a:tc hMerge="1">"#), "{xml}");
        assert!(xml.contains(r#"<a:tc gridSpan="2">"#), "{xml}");
        assert!(xml.contains(r#"<a:tcPr marL="76200" marR="76200" marT="38100" marB="38100" anchor="ctr"><a:lnL w="0"><a:noFill/></a:lnL><a:lnR w="0"><a:noFill/></a:lnR><a:lnT w="0"><a:noFill/></a:lnT><a:lnB w="9525"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:lnB><a:solidFill><a:srgbClr val="FAFAFA"/></a:solidFill></a:tcPr>"#), "{xml}");
        // CSS "to right" (90°) is DrawingML's 0°.
        assert!(xml.contains(r#"<a:gradFill rotWithShape="1"><a:gsLst><a:gs pos="0"><a:srgbClr val="A96D4B"/></a:gs><a:gs pos="100000"><a:srgbClr val="000000"><a:alpha val="0"/></a:srgbClr></a:gs></a:gsLst><a:lin ang="0" scaled="0"/></a:gradFill>"#), "{xml}");
    }

    #[test]
    fn the_model_deserializes_from_the_scene_json() {
        let json = r#"{
            "kind": "text", "frame": {"x": 1, "y": 2, "w": 3, "h": 4},
            "insets": {"l": 1, "t": 1, "r": 1, "b": 1}, "fill": {"gradient": {"angle": 90, "stops": [
                {"color": {"r": 1, "g": 2, "b": 3, "a": 1}, "pos": 0}, {"color": {"r": 1, "g": 2, "b": 3, "a": 0}, "pos": 1}]}},
            "line": null, "radius": null, "wrap": true, "title": false,
            "paragraphs": [{"align": "ctr", "level": 1, "bullet": {"scheme": "arabicPeriod", "start": 2, "color": null},
                "marginLeft": 20, "indent": -10, "lineHeight": 30, "spaceBefore": 4,
                "runs": [{"text": "x", "font": "Arial", "size": 20, "bold": true, "italic": false, "underline": false,
                          "strike": false, "color": {"r": 0, "g": 0, "b": 0, "a": 1}, "highlight": null, "link": null,
                          "caps": null, "baseline": 0, "spacing": 0}, {"br": true}]}]
        }"#;
        let shape: Shape = serde_json::from_str(json).unwrap();
        let Shape::Text(text) = shape else {
            panic!("not a text shape");
        };
        assert!(matches!(text.fill, Some(Fill::Gradient { .. })));
        let paragraph = &text.paragraphs[0];
        assert_eq!(paragraph.align, Align::Ctr);
        assert!(
            matches!(&paragraph.bullet, Some(Bullet::Auto { scheme, start: 2, .. }) if scheme == "arabicPeriod")
        );
        assert_eq!(paragraph.margin_left, 20.0);
        assert_eq!(paragraph.runs.len(), 2);
        assert!(matches!(paragraph.runs[1], Run::Break { br: true }));
        let picture: Shape = serde_json::from_str(r#"{"kind": "picture", "frame": {"x": 0, "y": 0, "w": 10, "h": 10}, "imageRef": 3, "shot": {"x": 5, "y": 6, "w": 10, "h": 10}, "alt": "", "alpha": null, "crop": null, "fit": {"l": 0.1, "t": 0, "r": 0.1, "b": 0}, "sizing": null, "position": null}"#).unwrap();
        let Shape::Picture(picture) = picture else {
            panic!("not a picture");
        };
        assert_eq!(picture.image_ref, Some(3));
        assert_eq!(picture.shot.map(|s| s.x), Some(5.0));
        assert_eq!(picture.fit.map(|f| f.l), Some(0.1));
        let bullet: Bullet =
            serde_json::from_str(r#"{"char": "•", "color": {"r": 1, "g": 1, "b": 1}}"#).unwrap();
        assert!(matches!(bullet, Bullet::Char { glyph, .. } if glyph == "•"));
        let background: Background = serde_json::from_str(r#"{"color": null, "imageRef": null, "shot": null, "alpha": null, "sizing": null, "position": null, "shotOnly": true}"#).unwrap();
        assert!(background.shot_only);
    }

    #[test]
    fn pictures_are_read_from_data_urls_with_their_sizes() {
        let url = format!(
            "data:image/png;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, PNG)
        );
        let image = image_from_data_url(&url).unwrap();
        assert_eq!(image.kind, ImageKind::Png);
        assert_eq!(image.dimensions(), Some((1, 1)));
        assert!(image_from_data_url("data:image/svg+xml;base64,PHN2Zz4=").is_none());
        assert!(image_from_data_url("data:image/png,raw").is_none());
        assert!(image_from_data_url("https://x/y.png").is_none());
        let gif = Image {
            bytes: b"GIF89a\x10\x00\x20\x00".to_vec(),
            kind: ImageKind::Gif,
        };
        assert_eq!(gif.dimensions(), Some((16, 32)));
        // A JPEG with an APP0 segment then a baseline frame header (SOF0) of 300 × 200.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0xC8, 0x01, 0x2C]);
        let jpeg = Image {
            bytes: jpeg,
            kind: ImageKind::Jpeg,
        };
        assert_eq!(jpeg.dimensions(), Some((300, 200)));
    }

    #[test]
    fn background_images_are_placed_like_css_backgrounds() {
        // A 200×100 picture covering a 100×100 box: the sides are cropped.
        let (crop, fit) = place_background("cover", "50% 50%", (200, 100), 100.0, 100.0);
        let crop = crop.unwrap();
        assert!(
            (crop.l - 0.25).abs() < 1e-9 && (crop.r - 0.25).abs() < 1e-9 && crop.t == 0.0,
            "{crop:?}"
        );
        assert!(fit.is_none());
        // Contained: letterboxed top and bottom.
        let (crop, fit) = place_background("contain", "50% 50%", (200, 100), 100.0, 100.0);
        assert!(crop.is_none());
        let fit = fit.unwrap();
        assert!(
            (fit.t - 0.25).abs() < 1e-9 && (fit.b - 0.25).abs() < 1e-9 && fit.l == 0.0,
            "{fit:?}"
        );
        // Natural size, centred: a 20×10 picture in the middle of the box.
        let (crop, fit) = place_background("auto", "50% 50%", (20, 10), 100.0, 100.0);
        assert!(crop.is_none());
        let fit = fit.unwrap();
        assert!(
            (fit.l - 0.4).abs() < 1e-9 && (fit.t - 0.45).abs() < 1e-9,
            "{fit:?}"
        );
        // Positioned at the top left, larger than the box: cropped right and bottom only.
        let (crop, fit) = place_background("auto", "0% 0%", (200, 200), 100.0, 100.0);
        let crop = crop.unwrap();
        assert!(
            crop.l == 0.0 && (crop.r - 0.5).abs() < 1e-9 && (crop.b - 0.5).abs() < 1e-9,
            "{crop:?}"
        );
        assert!(fit.is_none());
        assert_eq!(
            place_background("cover", "50% 50%", (0, 0), 100.0, 100.0),
            (None, None)
        );
    }

    #[test]
    fn slide_sizes_keep_the_ratio_at_powerpoints_height() {
        assert_eq!(
            slide_size_emu(1280.0, 720.0).unwrap(),
            (12_192_000, 6_858_000)
        );
        assert_eq!(
            slide_size_emu(1920.0, 1080.0).unwrap(),
            (12_192_000, 6_858_000)
        );
        assert_eq!(
            slide_size_emu(1024.0, 768.0).unwrap(),
            (9_144_000, 6_858_000)
        );
        assert_eq!(
            slide_size_emu(960.0, 700.0).unwrap(),
            (9_405_257, 6_858_000)
        );
        assert_eq!(slide_size_emu(7.0, 1.0).unwrap(), (48_006_000, 6_858_000));
        assert_eq!(slide_size_emu(10.0, 1.0).unwrap(), (51_206_400, 5_120_640));
        assert_eq!(slide_size_emu(100.0, 1.0).unwrap(), (51_206_400, 914_400));
        assert_eq!(slide_size_emu(1.0, 100.0).unwrap(), (914_400, 6_858_000));
        for (w, h) in [
            (0.0, 1.0),
            (1.0, -1.0),
            (f64::NAN, 1.0),
            (1.0, f64::INFINITY),
        ] {
            assert!(slide_size_emu(w, h).is_err(), "{w} × {h}");
        }
        assert_eq!(presentation_format(12_192_000, 6_858_000), "Widescreen");
        assert_eq!(
            presentation_format(9_144_000, 6_858_000),
            "On-screen Show (4:3)"
        );
        assert_eq!(presentation_format(9_405_257, 6_858_000), "Custom");
        let file = write(&Presentation {
            width: 1024.0,
            height: 768.0,
            ..presentation(vec![slide("One", vec![], &[])])
        })
        .unwrap();
        let entries = entries(&file);
        let xml = text(&entries, "ppt/presentation.xml");
        assert!(
            xml.contains(r#"<p:sldSz cx="9144000" cy="6858000"/>"#),
            "{xml}"
        );
        // The notes master shows the slide in its ratio: 5 inches wide, 3.75 high.
        let master = text(&entries, "ppt/notesMasters/notesMaster1.xml");
        assert!(
            master.contains(r#"<a:ext cx="4572000" cy="3429000"/>"#),
            "{master}"
        );
        // The master and the layout carry the title placeholder.
        assert!(
            text(&entries, "ppt/slideMasters/slideMaster1.xml").contains(r#"<p:ph type="title"/>"#)
        );
        assert!(
            text(&entries, "ppt/slideLayouts/slideLayout1.xml").contains(r#"<p:ph type="title"/>"#)
        );
    }

    #[test]
    fn names_and_properties_are_escaped_and_cut() {
        let file = write(&presentation(vec![
            slide(
                "Q&A <with> \"quotes\"\u{0}\tand\u{7}control",
                vec![],
                &["Line & one", "", "<b>", "\u{1}"],
            ),
            slide("  \n ", vec![], &[]),
        ]))
        .unwrap();
        let entries = entries(&file);
        let slide1 = text(&entries, "ppt/slides/slide1.xml");
        assert!(
            slide1
                .contains(r#"<p:cSld name="Q&amp;A &lt;with&gt; &quot;quotes&quot; andcontrol">"#),
            "{slide1}"
        );
        assert!(text(&entries, "ppt/slides/slide2.xml").contains(r#"<p:cSld name="Slide 2">"#));
        let notes = text(&entries, "ppt/notesSlides/notesSlide1.xml");
        assert!(notes.contains("<a:p><a:r><a:t>Line &amp; one</a:t></a:r></a:p><a:p><a:endParaRPr/></a:p><a:p><a:r><a:t>&lt;b&gt;</a:t></a:r></a:p><a:p><a:endParaRPr/></a:p>"), "{notes}");
        let core = text(&entries, "docProps/core.xml");
        assert!(
            core.contains("<dc:title>A &lt;deck&gt;</dc:title>"),
            "{core}"
        );
        assert!(
            core.contains("<dc:creator>Ada &amp; Bob</dc:creator>"),
            "{core}"
        );
        assert!(!core.contains("dcterms:created"), "{core}");
        let app = text(&entries, "docProps/app.xml");
        assert!(app.contains("<Slides>2</Slides><Notes>1</Notes>"), "{app}");
        assert!(
            app.contains("<PresentationFormat>Widescreen</PresentationFormat>"),
            "{app}"
        );
        assert_eq!(slide_name("Two\n words ", 1), "Two words");
        let name = slide_name(&"x".repeat(100), 1);
        assert_eq!(name.chars().count(), MAX_NAME_CHARS + 1);
        assert!(name.ends_with('…'));
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcd", 3), "abc…");
        assert_eq!(clean("a\tb\nc\rd"), "a\tb\nc\rd");
        assert_eq!(clean("a\u{0}b\u{8}c\u{B}d\u{1F}e\u{7F}f"), "abcde\u{7F}f");
        assert_eq!(escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e'f");
        assert!(is_link("https://example.org/a?b=c"));
        assert!(is_link("mailto:someone@example.org"));
        assert!(!is_link("#/2"));
        assert!(!is_link("javascript:alert(1)"));
        assert!(!is_link("https://example.org/a b"));
    }

    #[test]
    fn two_writes_give_the_same_bytes() {
        let presentation = presentation(vec![
            slide("One", vec![text_shape("x", false)], &["note"]),
            slide("Two", vec![], &[]),
        ]);
        assert_eq!(write(&presentation).unwrap(), write(&presentation).unwrap());
    }

    #[test]
    fn a_deck_without_slides_is_an_error() {
        let err = write(&presentation(Vec::new())).unwrap_err();
        assert_eq!(err.to_string(), "nothing to export: the deck has no slides");
    }
}
