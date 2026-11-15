# T49 — PowerPoint export

| | |
|---|---|
| Milestone | M5 |
| Size | L |
| Depends on | T45 |
| Parallel-safe with | — (done on `main`, 2026-10-10) |
| Files you own | `src/pptx.rs`, `src/pdf/capture.rs`, `src/pdf/scene.js`, `src/cli/export.rs`, `tests/cli_export.rs` (the `export pptx` section) |

## Goal
`presenter export pptx deck.md` writes a `.pptx` that others can edit in PowerPoint, with
one slide per page of the PDF export: a slide, each of its fragment steps (so a slide with
fragments builds up step by step, as in the PDF), and each page of a slide taller than a
page. Every slide holds native, editable elements, text boxes, tables, pictures and
rectangles, placed where the browser laid the deck out; the speaker notes are text in the
notes pane.

## Read first
- `docs/spec/cli.md` (export); the module docs of `src/pdf.rs` (how the print view is
  driven), `src/pdf/capture.rs` (how a page becomes shapes), the header of
  `src/pdf/scene.js` (the scene JSON) and `src/pptx.rs` (which parts of the Office Open XML
  package are written, and why).

## Design
1. `src/pdf.rs`: the PDF export's Chrome driver is split into what both exports share
   (`in_print_view`: start Chrome, load the deck in `?print-pdf`, wait for reveal's layout,
   stop Chrome) and the print step (`print_pdf`). A job runs on the ready page; its name goes
   into the error (`Chrome failed to print|capture the deck: …`).
2. `src/pdf/scene.js`, run in the page per `.pdf-page`: walks the DOM and returns a scene of
   shapes in slide coordinates. Text blocks (headings, paragraphs, code, quotes, cells)
   become text boxes with paragraphs and runs carrying the computed styles; lists become
   one box with a paragraph per item (bullet or number, indentation, gaps); tables become
   tables; `img` elements become pictures (bytes fetched afterwards); box backgrounds,
   borders and `::before`/`::after` decorations become rectangles or pictures; SVG,
   canvases, videos, iframes, display math and gradients are photographed; inline math is
   linearised from its MathML. Fonts are the first family of the CSS stack that Chrome can
   use (generic families map to Arial, Times New Roman, Courier New); colours in any
   syntax are resolved to sRGB (OKLab/OKLCH converted, the rest through a canvas), and
   translucent text, table and border colours are composited over their backdrop, because
   not every reader honours alpha there.
3. `src/pdf/capture.rs`: `html_to_pages` lists the pages (`PAGES_SCRIPT`: page size,
   parts, original or fragment clone, heading, notes), runs the scene extractor per page,
   fetches the pictures' `data:` URLs, photographs what must be photographed (the viewport
   one page large, scrolled to the page; backgrounds hidden and the page transparent for
   an element, the slide's content hidden for a background), and returns `Page`s with
   `pptx::Shape`s. The deadline grows by three seconds per page.
4. `src/pptx.rs`: the model (`Presentation`, `Slide`, `Shape`, …, deserialised from the
   scene JSON with serde) and the writer: a ZIP of stored entries with a fixed timestamp
   (reproducible), `[Content_Types].xml`, the relationships, document properties (title,
   author, no dates), the presentation (slide list, slide size keeping the deck's aspect
   ratio at 7.5 in high), a master and layout with a title placeholder and a theme, a notes
   master with its own theme, and per slide the shapes (DrawingML text bodies with
   paragraph and run properties, bullets, hyperlink relationships; pictures with crop,
   inset and opacity; rectangles with solid or gradient fills and outlines; tables with
   merged cells, cell fills and borders), the background colour and picture, the media
   parts (identical pictures shared) and a notes slide when there are notes.
5. `src/cli/export.rs`: one flow for both formats (output path and input-overwrite checks,
   browser lookup, build, extension warning), differing in what Chrome produces.

## Acceptance criteria
- [x] `presenter export pptx examples/basic/slides.md` writes a file with exactly as many
  slides as `export pdf` writes pages; the title slide's heading is in the title
  placeholder and its notes in its notes slide; the list slide is one text box with five
  hanging bullets and bold runs; the first fragment step hides the first fragment and the
  second shows it; the figure slide embeds `figures/plot.png` byte for byte; the code slide
  has a paragraph per line in a monospace font without wrapping; inline math is text and
  display math a picture; the table has three columns and four rows; the footnote's links
  are hyperlink relationships; a `background=` colour is the slide background and a
  `data-background-image` at 20 % opacity a picture with that opacity
  (`tests/cli_export.rs`).
- [x] `[reveal] pdfSeparateFragments = false` collapses the steps; the default gives one slide
  per step, each showing the fragments of its step; the default output is `dist/<stem>.pptx`.
- [x] `--scale` sizes the photographs (an inline SVG at 1 and 2); two exports of the same
  deck are byte-identical.
- [x] Exit 3 without Chrome (install hints), exit 2 for a `-o` that is a directory or an input,
  a warning for a `-o` without `.pptx`, build errors before the browser starts, a failed
  capture leaves no file and no browser.
- [x] `export pptx --help` says what the file holds; the guide's help blocks are regenerated.
- [x] The file is read by python-pptx and rendered by macOS's QuickLook with the slides
  looking like the deck (the example decks, the notebook example and the private EIT theme
  with its photo background, translucent band, gradient rule and logo; manual checks,
  2026-10-10). Opened in PowerPoint itself, with the EIT template deck, by the author on the
  pull request (2026-10-10): looks good. That check stays manual: PowerPoint and Keynote
  cannot be scripted into opening a file from here (both stop answering AppleScript on
  `open`, as sandboxed apps do when asked for a file outside their granted access).
  (2026-11-14: PowerPoint does open a file copied into its own container,
  `~/Library/Containers/com.microsoft.Powerpoint/Data/tmp/`, and saves it as PDF over
  AppleScript; the text fitting below was measured and checked that way, line by line
  against Chrome's PDF, on the example deck and a talk.)

## Known limits
- The scene is the browser's screen layout, as the HTML shows it, not the print layout of
  `export pdf`: where the theme's print rules differ (code lines print 5 % closer), the
  slides follow the screen.
- Text boxes keep the browser's geometry and the browser's line breaks (soft wraps become
  `<a:br>`; a box with a fill or outline and one line of text does not wrap, one with more
  lines wraps with the room taken from its padding on the side its text does not align to,
  and one without fill or outline wraps with a little room to spare, so a slightly wider
  line keeps whole). Each run's letter spacing makes up
  the width difference between the page's font and the one PowerPoint draws with,
  measured on a canvas (Arial with kerning off matches PowerPoint to ~0.25 %), plus the
  room inline elements keep around their text (inline code's and key caps' padding,
  borders and margins, an inline block's width, `::before`/`::after` margins). A run's
  letter spacing follows each character, so a key cap's letter sits at the left of its
  highlight rather than centred. A viewer without the font gets a substitute and lines run
  wider or narrower.
- PowerPoint draws a run with a baseline shift at two thirds of its size: a raised or
  lowered run (`vertical-align: super`, `sub` or a length of at least a fifth of the font
  size; smaller nudges are dropped) is written half as large again, its shift a share of
  that size, so it shows at the browser's size and height. Inline math is linearised
  text: its scripts are 0.7 of the size and raised or lowered as KaTeX sets them, and the
  whole is fitted to the math's width on the page (smaller where the text runs much wider,
  as a fraction written out on one line does).
- A task list item's checkbox is the item's bullet: Wingdings' empty or ticked box (Office
  has that font everywhere; PowerPoint draws no `☑` in Arial), where the checkbox is.
- PowerPoint (measured on Mac) rounds exact line spacing and space before to whole points,
  places the first baseline 0.75 of the line pitch down (or `line − 0.23 × size` for tight
  lines), insets a rounded rectangle's text by 0.29 of its corner radius, and centres an
  outline on the shape's edge. The writer compensates each (`text_layout`, `outlined`,
  `rounded_text_inset`). Each paragraph's first baseline is the one the browser drew
  (measured on its first glyph, so Chrome's whole-pixel leading and line boxes that a
  larger inline box made taller are matched), and a paragraph of one line among others
  (a code block's line) gets its spacing rounded down, so PowerPoint never runs ahead and
  a whole point of space before takes up what it falls behind. What is left is about 1 px
  per line, plus up to half a point per line inside a paragraph whose line height is not a
  whole number of points (centred over the paragraph: a 7-line paragraph at 25.5 pt is
  ~2 px off at its first and last lines). Lines inside a paragraph share one spacing: a
  line the browser makes taller (a subscript, larger inline text) is not taller in
  PowerPoint, so the lines after it sit higher.
- Transforms, shadows, rotations, radial gradients and CSS filters are not carried over;
  inline math loses fraction and root layout (`a/b`, `√(x)`); reveal's slide number is left
  out (PowerPoint has its own).
