//! T48 end to end: `:::include` composes a deck from Markdown and notebook files
//! (markdown-dialect §4.1, ir.md §1 `Deck.includes` / `SourceSpan.file`, live-reload §5, cli.md
//! `check`). The composed fixture is `tests/fixtures/include/master.md`; misuses are written
//! into temporary directories.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use presenter::error::{Error, Level};
use presenter::ir::{Block, Deck, Slide};
use presenter::pipeline::{self, BuildOptions};
use presenter::serve::watch::{canonical, try_build_watched, watch_set_for};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/fixtures/include").join(name)
}

/// A temporary directory with `files` (path, contents) written into it.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    dir
}

/// The deck at `path` as `dump` and `check` parse it.
fn parse(path: &Path) -> presenter::Result<presenter::ir::Parsed> {
    pipeline::parse_only(&BuildOptions::new(path)).map(|(parsed, ..)| parsed)
}

/// `(path, line, message)` of the parse error that parsing `deck` (in `dir`) fails with.
fn parse_error(dir: &Path, deck: &str) -> (PathBuf, Option<usize>, String) {
    match parse(&dir.join(deck)) {
        Err(Error::Parse {
            path,
            line,
            message,
        }) => (path, line, message),
        other => panic!("expected a parse error for {deck}, got {other:?}"),
    }
}

/// The titles of the deck's slides, each with its children's.
fn titles(deck: &Deck) -> Vec<(String, Vec<String>)> {
    let title = |slide: &Slide| slide.title.clone().unwrap_or_default();
    deck.slides
        .iter()
        .map(|slide| (title(slide), slide.children.iter().map(title).collect()))
        .collect()
}

/// The file every slide's span names (`None`: the deck's own), with the children's.
fn span_files(deck: &Deck) -> Vec<Option<String>> {
    let file = |slide: &Slide| {
        let span = slide.span.as_ref().expect("every slide has a span");
        span.file
            .as_ref()
            .map(|file| file.file_name().unwrap().to_string_lossy().into_owned())
    };
    deck.slides
        .iter()
        .flat_map(|slide| std::iter::once(slide).chain(&slide.children))
        .map(file)
        .collect()
}

fn html(slide: &Slide) -> String {
    slide
        .blocks
        .iter()
        .map(|block| match block {
            Block::Html { html } => html.clone(),
            Block::Cell(cell) => format!("<cell {}>", cell.index),
        })
        .collect()
}

#[test]
fn a_markdown_deck_includes_a_notebook_and_markdown_parts() {
    let parsed = parse(&fixture("master.md")).unwrap();
    let deck = &parsed.deck;
    let owned = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        titles(deck),
        [
            ("Composed deck".into(), vec![]),
            ("Notebook slide".into(), owned(&["Notebook sub-slide"])),
            ("Second notebook slide".into(), vec![]),
            ("Between".into(), owned(&["Below between"])),
            ("Part one".into(), owned(&["Part one, below"])),
            ("Nested".into(), vec![]),
        ]
    );
    let nb = Some("analysis.ipynb".to_string());
    let part = Some("part.md".to_string());
    assert_eq!(
        span_files(deck),
        [
            None,
            nb.clone(),
            nb.clone(),
            nb,
            None,
            None,
            part.clone(),
            part,
            Some("nested.md".into()),
        ]
    );
    // Spans keep their own file's lines (and cells).
    let notebook_slide = deck.slides[1].span.as_ref().unwrap();
    assert_eq!(
        (notebook_slide.cell, notebook_slide.start_line),
        (Some(0), 1)
    );
    let part_slide = deck.slides[4].span.as_ref().unwrap();
    assert_eq!((part_slide.cell, part_slide.start_line), (None, 8));
    assert_eq!(deck.slides[3].id.as_deref(), Some("between"));
    assert_eq!(deck.slides[3].span.as_ref().unwrap().start_line, 24);
    // Transitive, absolute, in first-use order.
    assert_eq!(
        deck.includes,
        [
            fixture("notebooks/analysis.ipynb"),
            fixture("parts/part.md"),
            fixture("parts/nested.md"),
        ]
    );
    assert_eq!(deck.invariant_violations(), Vec::<String>::new());
    // Features of every file: math (master and notebook), code (nested.md, cells), notebook.
    assert!(deck.features.math && deck.features.code && deck.features.notebook);
    assert!(!deck.features.notes);
    assert!(matches!(deck.slides[1].blocks[1], Block::Cell(_)));
    // The deck's metadata is the master's; part.md's title and theme are ignored.
    assert_eq!(deck.meta.title.as_deref(), Some("Composed deck"));
    assert_eq!(deck.meta.theme.as_deref(), Some("default"));
    assert_eq!(deck.source.path, fixture("master.md"));
    let found: Vec<_> = parsed
        .diagnostics
        .iter()
        .map(|d| {
            (
                d.code,
                d.path.clone(),
                d.span.as_ref().map(|s| s.start_line),
            )
        })
        .collect();
    assert_eq!(
        found,
        [("include-meta-ignored", Some(fixture("master.md")), Some(36))]
    );
    let message = &parsed.diagnostics.iter().next().unwrap().message;
    assert!(
        message.starts_with("`parts/part.md` sets `title` and `theme`;"),
        "{message}"
    );
    // part.md's own `slides.incremental_lists` applies to its own slides only.
    assert!(html(&deck.slides[4]).contains("<li class=\"fragment\">"));
    assert!(!html(&deck.slides[0]).contains("fragment"));
}

#[test]
fn included_spans_and_includes_are_serialized_only_when_set() {
    let parsed = parse(&fixture("master.md")).unwrap();
    let json = serde_json::to_value(&parsed.deck).unwrap();
    assert_eq!(json["includes"].as_array().unwrap().len(), 3);
    assert!(json["slides"][0]["span"].get("file").is_none());
    assert_eq!(
        json["slides"][4]["span"]["file"].as_str(),
        Some(fixture("parts/part.md").to_str().unwrap())
    );
    let plain = parse(&fixture("parts/nested.md")).unwrap();
    let json = serde_json::to_value(&plain.deck).unwrap();
    assert!(json.get("includes").is_none());
    assert!(json["slides"][0]["span"].get("file").is_none());
    let back: Deck = serde_json::from_value(json).unwrap();
    assert_eq!(back, plain.deck);
}

/// ir.md §2 as `Deck::invariant_violations` checks it for includes: a span's `file` is one
/// of `Deck.includes`.
#[test]
fn a_span_file_must_be_an_include() {
    let mut deck = parse(&fixture("master.md")).unwrap().deck;
    deck.includes.retain(|path| !path.ends_with("nested.md"));
    let violations = deck.invariant_violations();
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(
        violations[0].starts_with("slides[5]: span.file ")
            && violations[0].ends_with("nested.md is not in includes"),
        "{violations:?}"
    );
}

#[test]
fn standalone_and_site_builds_resolve_the_assets_of_every_file() {
    let build = pipeline::build_deck(&BuildOptions::new(fixture("master.md"))).unwrap();
    let html = &build.document.html;
    let placeholder = regex::Regex::new("asset:[0-9a-f]{12}").unwrap();
    assert!(!placeholder.is_match(html), "an asset: placeholder is left");
    for image in ["images/master.png", "parts/img/part.png"] {
        let bytes = std::fs::read(fixture(image)).unwrap();
        let uri = format!("data:image/png;base64,{}", STANDARD.encode(bytes));
        assert!(html.contains(&uri), "{image} is not inlined");
    }
    // The notebook's plot, too.
    assert_eq!(html.matches("data:image/png;base64,").count(), 3);
    assert_eq!(build.document.slide_count, 9);
    let errors = build.diagnostics.iter().filter(|d| d.level == Level::Error);
    assert_eq!(errors.count(), 0);

    let out = tempfile::tempdir().unwrap();
    let site = BuildOptions {
        output: pipeline::output_mode(false, out.path()),
        ..BuildOptions::new(fixture("master.md"))
    };
    let build = pipeline::build_deck(&site).unwrap();
    let pngs = build
        .document
        .files
        .iter()
        .filter(|file| file.path.extension().is_some_and(|ext| ext == "png"))
        .count();
    assert_eq!(pngs, 3);
    assert!(!placeholder.is_match(&build.document.html));
    // The included files are inputs that an output must never overwrite.
    let inputs = pipeline::input_files(&build);
    for file in [
        "parts/part.md",
        "parts/nested.md",
        "notebooks/analysis.ipynb",
    ] {
        assert!(
            inputs
                .iter()
                .any(|(path, role)| *path == fixture(file) && *role == pipeline::InputRole::Include),
            "{file}"
        );
    }
}

#[test]
fn included_files_are_watched_also_when_the_build_fails() {
    let build = pipeline::build_deck(&BuildOptions {
        output: presenter::assets::emit::OutputMode::Serve,
        missing_asset_is_error: false,
        ..BuildOptions::new(fixture("master.md"))
    })
    .unwrap();
    let set = watch_set_for(&build);
    for file in [
        "master.md",
        "notebooks/analysis.ipynb",
        "parts/part.md",
        "parts/nested.md",
    ] {
        assert!(set.files.contains(&canonical(&fixture(file))), "{file}");
    }
    // An error inside an included file: the deck does not parse, and the files it includes
    // are still watched, so fixing them rebuilds; so is an included file that is missing.
    let dir = project(&[
        ("deck.md", "# A\n\n---\n\n:::include parts/p.md\n:::\n"),
        (
            "parts/p.md",
            "# P\n\n--- {title=\"open}\n\n:::include more.md\n:::\n",
        ),
    ]);
    let opts = BuildOptions {
        output: presenter::assets::emit::OutputMode::Serve,
        ..BuildOptions::new(dir.path().join("deck.md"))
    };
    let (result, set) = try_build_watched(&opts);
    let error = result.unwrap_err().error;
    assert_eq!(error.path(), Some(dir.path().join("parts/p.md").as_path()));
    for file in ["deck.md", "parts/p.md", "parts/more.md"] {
        assert!(
            set.files.contains(&canonical(&dir.path().join(file))),
            "{file}: {:?}",
            set.files
        );
    }
}

#[test]
fn every_misuse_is_an_error_at_the_directive() {
    let nested_deep: Vec<(String, String)> = (0..10)
        .map(|i| {
            let text = if i == 9 {
                "# Deepest\n".to_string()
            } else {
                format!(":::include d{}.md\n:::\n", i + 1)
            };
            (format!("d{i}.md"), text)
        })
        .collect();
    let mut files: Vec<(&str, &str)> = vec![
        ("content.md", "# Title\n\n:::include b.md\n:::\n"),
        ("vertical.md", "# A\n\n--\n\n:::include b.md\n:::\n"),
        ("children.md", ":::include b.md\n:::\n\n--\n\n## Below\n"),
        (
            "list.md",
            "# A\n\n---\n\n- item\n\n  :::include b.md\n  :::\n",
        ),
        ("quote.md", "> :::include b.md\n> :::\n"),
        ("directive.md", "::::center\n:::include b.md\n:::\n::::\n"),
        ("body.md", ":::include b.md\n# Not allowed\n:::\n"),
        ("unclosed.md", ":::include b.md\n\n---\n\n# Swallowed\n"),
        ("missing.md", "# A\n\n---\n\n:::include nothere.md\n:::\n"),
        ("extension.md", ":::include notes.txt\n:::\n"),
        ("cycle-a.md", "# A\n\n---\n\n:::include cycle-b.md\n:::\n"),
        ("cycle-b.md", "# B\n\n---\n\n:::include cycle-a.md\n:::\n"),
        ("b.md", "# B\n"),
        ("notes.txt", "text"),
    ];
    files.extend(nested_deep.iter().map(|(n, t)| (n.as_str(), t.as_str())));
    let dir = project(&files);
    let at = |name: &str| dir.path().join(name);
    let cases: [(&str, usize, &str); 10] = [
        (
            "content.md",
            3,
            "must be the only content of its slide, but line 1",
        ),
        (
            "vertical.md",
            5,
            "cannot be in a vertical slide (the `--` on line 3",
        ),
        (
            "children.md",
            1,
            "cannot have vertical slides (the `--` on line 4)",
        ),
        ("list.md", 7, "not inside a list"),
        ("quote.md", 1, "not inside a block quote"),
        ("directive.md", 2, "not inside the `:::center` directive"),
        ("body.md", 1, "takes no content, but line 2 is inside it"),
        ("unclosed.md", 1, "is not closed"),
        (
            "missing.md",
            5,
            "cannot include `nothere.md`: file not found (looked for ",
        ),
        (
            "extension.md",
            1,
            "cannot include `notes.txt`: only Markdown",
        ),
    ];
    for (deck, line, part) in cases {
        let (path, found, message) = parse_error(dir.path(), deck);
        assert_eq!(path, at(deck), "{deck}: {message}");
        assert_eq!(found, Some(line), "{deck}: {message}");
        assert!(message.contains(part), "{deck}: {message}");
    }
    // A cycle is reported in the file that closes it, naming the chain.
    let (path, line, message) = parse_error(dir.path(), "cycle-a.md");
    assert_eq!((path, line), (at("cycle-b.md"), Some(5)));
    assert_eq!(
        message,
        "include cycle: cycle-a.md → cycle-b.md → cycle-a.md"
    );
    // Eight levels of includes work; a ninth is an error in the eighth file.
    assert_eq!(titles(&parse(&at("d1.md")).unwrap().deck)[0].0, "Deepest");
    let (path, line, message) = parse_error(dir.path(), "d0.md");
    assert_eq!((path, line), (at("d8.md"), Some(1)));
    assert!(
        message.starts_with("includes nest more than 8 levels deep: d0.md → d1.md → "),
        "{message}"
    );
}

#[test]
fn errors_inside_an_included_file_keep_its_path_and_line() {
    let dir = project(&[
        ("deck.md", "# A\n\n---\n\n:::include part.md\n:::\n"),
        ("part.md", "# P\n\nText\n\n--- {title=\"open}\n\n# Q\n"),
        ("fm.md", ":::include bad-front.md\n:::\n"),
        ("bad-front.md", "---\ntitle: [unclosed\n---\n\n# X\n"),
        ("nb.md", ":::include broken.ipynb\n:::\n"),
        ("broken.ipynb", "{\"cells\": [\n"),
    ]);
    let (path, line, message) = parse_error(dir.path(), "deck.md");
    assert_eq!((path, line), (dir.path().join("part.md"), Some(5)));
    assert!(message.contains("unbalanced"), "{message}");
    // A front matter error names the included file (not the deck it is included in).
    let err = parse(&dir.path().join("fm.md")).unwrap_err();
    assert_eq!(err.path(), Some(dir.path().join("bad-front.md").as_path()));
    assert!(err.span().is_some_and(|span| span.start_line >= 2), "{err}");
    assert!(err.message().starts_with("front matter: "), "{err}");
    let (path, _, _) = parse_error(dir.path(), "nb.md");
    assert_eq!(path, dir.path().join("broken.ipynb"));
}

#[test]
fn a_missing_image_in_an_included_file_is_reported_there() {
    let dir = project(&[
        ("deck.md", "# A\n\n---\n\n:::include sub/part.md\n:::\n"),
        ("sub/part.md", "# P\n\n![gone](gone.png)\n"),
    ]);
    let deck = dir.path().join("deck.md");
    // `build`: an error located in the included file.
    match pipeline::build_deck(&BuildOptions::new(&deck)) {
        Err(Error::Parse {
            path,
            line,
            message,
        }) => {
            assert_eq!((path, line), (dir.path().join("sub/part.md"), Some(3)));
            assert!(message.contains("`gone.png` not found"), "{message}");
            assert!(message.contains("sub/gone.png"), "{message}");
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
    // `serve`: a warning with the included file's path and span.
    let parsed = parse(&deck).unwrap();
    let warning = parsed.diagnostics.iter().next().unwrap();
    assert_eq!(warning.code, "missing-asset");
    assert_eq!(
        warning.path.as_deref(),
        Some(dir.path().join("sub/part.md").as_path())
    );
    let span = warning.span.as_ref().unwrap();
    assert_eq!(span.start_line, 3);
    assert_eq!(span.file.as_deref(), warning.path.as_deref());
}

#[test]
fn slide_attributes_and_notes_on_an_include_slide_are_ignored() {
    let dir = project(&[
        (
            "deck.md",
            "# A\n\n--- {.dark background=#000}\n\n:::include part.md\n:::\n\n:::notes\nN\n:::\n",
        ),
        ("part.md", "# P\n"),
    ]);
    let parsed = parse(&dir.path().join("deck.md")).unwrap();
    let slide = &parsed.deck.slides[1];
    assert_eq!(slide.title.as_deref(), Some("P"));
    assert!(slide.classes.is_empty() && slide.attrs.is_empty() && slide.notes.is_none());
    let found: Vec<_> = parsed
        .diagnostics
        .iter()
        .map(|d| (d.code, d.span.as_ref().map(|s| s.start_line)))
        .collect();
    assert_eq!(found, [("include-attrs-ignored", Some(5)); 2]);
}

/// Warnings about a slide from an included file (the renderer's and `check`'s) name that file.
#[test]
fn slide_warnings_name_the_included_file() {
    let dir = project(&[
        ("deck.md", "# A {#same}\n\n---\n\n:::include part.md\n:::\n"),
        ("part.md", "# P\n\n---\n\n## Q {layout=nope #same}\n"),
    ]);
    let deck = dir.path().join("deck.md");
    let part = dir.path().join("part.md");
    let build = pipeline::build_deck(&BuildOptions::new(&deck)).unwrap();
    let found: Vec<_> = build
        .diagnostics
        .iter()
        .map(|d| {
            (
                d.code,
                d.path.clone(),
                d.span.as_ref().map(|s| s.start_line),
            )
        })
        .collect();
    assert_eq!(found, [("unknown-layout", Some(part.clone()), Some(3))]);
    let checked = presenter::check::run(&BuildOptions::new(&deck)).unwrap();
    let found: Vec<_> = checked
        .iter()
        .map(|d| {
            (
                d.code,
                d.path.clone(),
                d.span.as_ref().map(|s| s.start_line),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("duplicate-slide-id", Some(part.clone()), Some(3)),
            ("unknown-layout", Some(part), Some(3)),
        ]
    );
    let message = &checked.iter().next().unwrap().message;
    assert!(
        message.starts_with("slide 3: id `same` is already the id of slide 1 (line 1)"),
        "{message}"
    );
}

#[test]
fn headings_mode_decks_include_too() {
    let dir = project(&[
        (
            "deck.md",
            "---\nslides: {split: headings}\n---\n\n# Intro\n\nText\n\n---\n\n\
             :::include part.md\n:::\n\n# After\n\n## Below after\n",
        ),
        ("part.md", "# Part\n\n## Part below\n"),
    ]);
    let parsed = parse(&dir.path().join("deck.md")).unwrap();
    let owned = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        titles(&parsed.deck),
        [
            ("Intro".into(), vec![]),
            ("Part".into(), owned(&["Part below"])),
            ("After".into(), owned(&["Below after"])),
        ]
    );
    // An include right after a heading shares its slide: an error with a hint.
    std::fs::write(
        dir.path().join("deck.md"),
        "---\nslides: {split: headings}\n---\n\n# Intro\n\n:::include part.md\n:::\n",
    )
    .unwrap();
    let (path, line, message) = parse_error(dir.path(), "deck.md");
    assert_eq!((path, line), (dir.path().join("deck.md"), Some(7)));
    assert!(
        message.contains("put `---` before the directive, and after it unless"),
        "{message}"
    );
}

/// The included file is parsed with the deck's effective config: `presenter.toml` and the
/// deck's front matter (its `notebook:` table for an included notebook); the included file's
/// own config tables apply to it only.
#[test]
fn included_files_are_parsed_with_the_decks_config() {
    let cell = |tag: &str| {
        serde_json::json!({
            "cell_type": "code", "execution_count": 1, "metadata": {"tags": [tag]},
            "outputs": [], "source": "x = 1"
        })
    };
    let notebook = serde_json::json!({
        "nbformat": 4, "nbformat_minor": 5, "metadata": {},
        "cells": [cell("from-toml"), cell("from-front-matter")]
    });
    let dir = project(&[
        (
            "presenter.toml",
            "[notebook]\nremove_cell_tags = [\"from-toml\"]\n",
        ),
        (
            "deck.md",
            "---\nnotebook:\n  hide_input_tags: [from-front-matter]\nslides:\n  \
             incremental_lists: true\n---\n\n:::include nb.ipynb\n:::\n\n---\n\n\
             :::include part.md\n:::\n",
        ),
        ("nb.ipynb", &notebook.to_string()),
        (
            "part.md",
            "---\nslides: {incremental_lists: false}\n---\n\n- a\n- b\n\n---\n\n\
             :::include nested.md\n:::\n",
        ),
        ("nested.md", "- c\n- d\n"),
    ]);
    let parsed = parse(&dir.path().join("deck.md")).unwrap();
    let slides = &parsed.deck.slides;
    let cells: Vec<_> = slides[0]
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Cell(cell) => Some((cell.index, cell.hide_input)),
            Block::Html { .. } => None,
        })
        .collect();
    assert_eq!(
        cells,
        [(1, true)],
        "removed by presenter.toml, hidden by the front matter"
    );
    // The deck turns incremental lists on, part.md turns them off for itself and for the
    // files it includes.
    assert!(
        !html(&slides[1]).contains("fragment"),
        "part.md's own setting"
    );
    assert!(
        !html(&slides[2]).contains("fragment"),
        "nested.md: part.md's settings"
    );
}

#[test]
fn a_notebook_cannot_include() {
    let notebook = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [{
            "cell_type": "markdown",
            "metadata": {},
            "source": "# Notebook\n\n:::include part.md\n:::\n\nAfter.\n"
        }]
    });
    let dir = project(&[("nb.ipynb", &notebook.to_string()), ("part.md", "# P\n")]);
    let parsed = parse(&dir.path().join("nb.ipynb")).unwrap();
    let found: Vec<_> = parsed
        .diagnostics
        .iter()
        .map(|d| {
            let span = d.span.as_ref().unwrap();
            (d.code, span.cell, span.start_line)
        })
        .collect();
    assert_eq!(found, [("include-in-notebook", Some(0), 3)]);
    assert!(parsed.deck.includes.is_empty());
    let body = html(&parsed.deck.slides[0]);
    assert_eq!(body, "<h1>Notebook</h1>\n<p>After.</p>\n");
}

#[test]
fn check_lints_included_files_with_their_own_paths() {
    let dir = project(&[
        ("deck.md", "# A\n\n---\n\n:::include sub/part.md\n:::\n"),
        ("sub/part.md", "# P\n\nSome text\n--\n\nmore\n"),
    ]);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_presenter"))
        .current_dir(dir.path())
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .args(["check", "deck.md"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let report = format!("{stdout}{stderr}");
    assert!(
        report.contains("sub/part.md:4: warning: `--` under a line of text"),
        "{report}"
    );
    assert!(!report.contains("deck.md:"), "{report}");
}

#[test]
fn the_mixed_example_builds_and_checks_clean() {
    let input = root().join("examples/mixed/slides.md");
    let diagnostics = presenter::check::run(&BuildOptions::new(&input)).unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let build = pipeline::build_deck(&BuildOptions::new(&input)).unwrap();
    assert!(build.diagnostics.is_empty(), "{:#?}", build.diagnostics);
    assert_eq!(
        build.deck.includes,
        [
            root().join("examples/notebook/analysis.ipynb"),
            root().join("examples/mixed/part2.md"),
        ]
    );
    assert_eq!(build.document.slide_count, 11);
    assert!(build.deck.features.math && build.deck.features.code && build.deck.features.notebook);
}
