//! T15: the HTML renderer (`presenter::render`) with the built-in default theme, in all
//! three output modes (docs/spec/theme-format.md §4–§5, config.md §4).
//!
//! The fixture deck is `tests/fixtures/ir/sample-deck.json`; its only asset
//! (`asset:32debefb8003`) is `tests/fixtures/md/images/ok.png` registered by bytes.
//! `writes_previews_for_manual_browser_checks` writes the standalone and site outputs to
//! `target/preview/render-*` for the manual browser check.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use presenter::assets::emit::OutputMode;
use presenter::assets::registry::AssetRegistry;
use presenter::config::{Config, MathMode};
use presenter::error::{Error, Level};
use presenter::ir::{Deck, DeckMeta};
use presenter::render::{
    self, BuildInfo, BuildMode, RenderInput, RenderedDocument, reveal::reveal_config_json,
};
use presenter::theme::{self, Theme};
use regex::Regex;
use serde_json::{Value, json};

const ASSET_ID: &str = "32debefb8003";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sample_deck() -> Deck {
    let text = std::fs::read_to_string(root().join("tests/fixtures/ir/sample-deck.json"))
        .expect("fixture exists");
    serde_json::from_str(&text).expect("fixture deserializes")
}

fn registry() -> AssetRegistry {
    let mut registry = AssetRegistry::new();
    let bytes = std::fs::read(root().join("tests/fixtures/md/images/ok.png")).expect("ok.png");
    let id = registry.register_bytes(bytes, "image/png", "ok.png");
    assert_eq!(id, ASSET_ID);
    registry
}

fn default_theme() -> Theme {
    theme::resolve("default", &[]).expect("built-in default theme")
}

fn site() -> OutputMode {
    OutputMode::Site {
        assets_dir: PathBuf::from("out/assets"),
        url_prefix: "assets/".into(),
    }
}

fn build_info(output: &OutputMode) -> BuildInfo {
    match output {
        OutputMode::Standalone => BuildInfo::build(true),
        OutputMode::Site { .. } => BuildInfo::build(false),
        OutputMode::Serve => BuildInfo::serve(0),
    }
}

fn try_render_with(
    deck: &Deck,
    theme: &Theme,
    config: &Config,
    output: OutputMode,
) -> presenter::Result<RenderedDocument> {
    let registry = registry();
    let build = build_info(&output);
    render::render(RenderInput {
        deck,
        assets: &registry,
        theme,
        config,
        output,
        build,
    })
}

fn render_with(
    deck: &Deck,
    theme: &Theme,
    config: &Config,
    output: OutputMode,
) -> RenderedDocument {
    try_render_with(deck, theme, config, output).expect("render succeeds")
}

fn render_sample(output: OutputMode) -> RenderedDocument {
    render_with(&sample_deck(), &default_theme(), &Config::default(), output)
}

/// The `<div class="slides">…</div>` element of a document.
fn slides_region(html: &str) -> &str {
    let start = html.find(r#"<div class="slides">"#).expect("slides div");
    let tags = Regex::new(r"<div\b|</div>").unwrap();
    let mut depth = 0usize;
    for tag in tags.find_iter(&html[start..]) {
        if tag.as_str() == "</div>" {
            depth -= 1;
            if depth == 0 {
                return &html[start..start + tag.end()];
            }
        } else {
            depth += 1;
        }
    }
    panic!("unbalanced <div class=\"slides\">");
}

/// `text` with every mode's URL for the fixture asset replaced by `[asset]`.
fn normalize_asset_urls(text: &str) -> String {
    let patterns = [
        r"data:image/png;base64,[A-Za-z0-9+/=]+".to_string(),
        format!(r"assets/ok-{ASSET_ID}\.png"),
        format!(r"/__presenter/deck-asset/{ASSET_ID}"),
    ];
    patterns.iter().fold(text.to_string(), |text, pattern| {
        Regex::new(pattern)
            .unwrap()
            .replace_all(&text, "[asset]")
            .into_owned()
    })
}

#[test]
fn slides_region_is_identical_across_modes_except_asset_urls() {
    let standalone = render_sample(OutputMode::Standalone);
    let site = render_sample(site());
    let serve = render_sample(OutputMode::Serve);
    let regions = [
        ("slides_standalone", slides_region(&standalone.html)),
        ("slides_site", slides_region(&site.html)),
        ("slides_serve", slides_region(&serve.html)),
    ];
    for (name, region) in regions {
        insta::assert_snapshot!(name, region);
    }
    assert!(regions[0].1.contains(r#"<img src="data:image/png;base64,"#));
    assert!(
        regions[1]
            .1
            .contains(&format!(r#"<img src="assets/ok-{ASSET_ID}.png""#))
    );
    assert!(
        regions[2]
            .1
            .contains(&format!(r#"<img src="/__presenter/deck-asset/{ASSET_ID}""#))
    );
    let normalized: Vec<String> = regions
        .iter()
        .map(|(_, region)| normalize_asset_urls(region))
        .collect();
    assert_eq!(normalized[0], normalized[1], "standalone vs site");
    assert_eq!(normalized[0], normalized[2], "standalone vs serve");
    for doc in [&standalone, &site, &serve] {
        assert_eq!(doc.slide_count, 10, "8 top-level slides + 2 children");
        assert!(doc.diagnostics.is_empty(), "{:?}", doc.diagnostics);
        assert!(
            !doc.html.contains("asset:"),
            "every placeholder is resolved"
        );
    }
}

/// Every URL that makes the browser load something from the network: `src`/`href`/
/// `poster`/`srcset`/`action` attribute values, CSS `url()` and `@import` targets that are
/// absolute (`http://`, `https://`) or protocol-relative (`//`).
fn network_references(html: &str) -> BTreeSet<String> {
    let reference = Regex::new(
        r#"(?i)(?:\b(?:src|href|poster|srcset|action)\s*=\s*["']?|url\(\s*["']?|@import\s+["'])\s*((?:https?:)?//[^"'\s)>]*)"#,
    )
    .unwrap();
    reference
        .captures_iter(html)
        .map(|caps| caps[1].to_string())
        .collect()
}

#[test]
fn standalone_output_is_self_contained() {
    let doc = render_sample(OutputMode::Standalone);
    let html = &doc.html;
    assert!(html.contains("<style id=\"presenter-theme\">\n/* @default/styles/theme.css */\n"));
    assert!(!html.contains("<link"), "no external stylesheets");
    assert!(!html.contains("<script src"), "no external scripts");
    assert!(!html.contains("/__presenter/"));
    assert_eq!(
        network_references(html),
        BTreeSet::from(["https://example.org/rust-data".to_string()]),
        "only the deck's own link points at the network"
    );
    assert!(doc.files.is_empty());
    // The theme stylesheet is the last stylesheet in <head> (§5).
    let head = &html[..html.find("</head>").unwrap()];
    assert!(head.rfind("<style").unwrap() == head.find("<style id=\"presenter-theme\">").unwrap());
    // reveal, plugins (code and notes present), KaTeX, math init, then Reveal.initialize.
    let init = html.find("Reveal.initialize(").unwrap();
    for marker in ["RevealHighlight", "window.__presenterRenderMath", "katex"] {
        let at = html
            .find(marker)
            .unwrap_or_else(|| panic!("{marker} inlined"));
        assert!(at < init, "{marker} before Reveal.initialize");
    }
    assert!(html.contains(
        r#"window.__presenterKatexOptions = {};
Reveal.initialize(Object.assign({"center":true,"controls":true,"hash":true,"height":720,"progress":true,"slideNumber":"c/t","transition":"fade","width":1280}, { plugins: [RevealHighlight, RevealNotes, RevealZoom, RevealSearch] }));</script>"#
    ));
    assert!(!html.contains("/__presenter/live-reload.js"));
}

#[test]
fn serve_output_uses_dev_server_urls() {
    let doc = render_sample(OutputMode::Serve);
    let html = &doc.html;
    assert!(html.contains(
        r#"<link id="presenter-theme" rel="stylesheet" href="/__presenter/theme.css?v=0">"#
    ));
    for path in [
        r#"<link rel="stylesheet" href="/__presenter/asset/vendor/reveal/dist/reveal.css">"#,
        r#"<link rel="stylesheet" href="/__presenter/asset/vendor/katex/katex.min.css">"#,
        r#"<script src="/__presenter/asset/vendor/reveal/dist/reveal.js"></script>"#,
        r#"<script src="/__presenter/asset/vendor/reveal/dist/plugin/highlight.js"></script>"#,
        r#"<script src="/__presenter/asset/presenter/math.js"></script>"#,
        r#"<script src="/__presenter/live-reload.js"></script>"#,
    ] {
        assert!(html.contains(path), "{path}");
    }
    assert!(!html.contains("<style"), "nothing inlined in serve mode");
    assert!(!html.contains("data:"));
    assert!(doc.files.is_empty());
    assert!(
        doc.theme_css
            .starts_with("/* @default/styles/theme.css */\n")
    );
    // The rebuild counter goes into the stylesheet URL.
    let deck = sample_deck();
    let registry = registry();
    let theme = default_theme();
    let doc = render::render(RenderInput {
        deck: &deck,
        assets: &registry,
        theme: &theme,
        config: &Config::default(),
        output: OutputMode::Serve,
        build: BuildInfo::serve(17),
    })
    .unwrap();
    assert!(doc.html.contains(r#"href="/__presenter/theme.css?v=17""#));
}

#[test]
fn site_output_lists_files() {
    let doc = render_sample(site());
    let html = &doc.html;
    let names: Vec<String> = doc
        .files
        .iter()
        .map(|file| {
            assert_eq!(file.path.parent(), Some(Path::new("out/assets")));
            file.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let named = |pattern: &str| {
        let re = Regex::new(&format!("^{pattern}$")).unwrap();
        names.iter().filter(|name| re.is_match(name)).count()
    };
    assert_eq!(named(r"theme-[0-9a-f]{12}\.css"), 1, "{names:?}");
    assert_eq!(named(&format!(r"ok-{ASSET_ID}\.png")), 1, "{names:?}");
    assert_eq!(named(r"reveal-[0-9a-f]{12}\.js"), 1, "{names:?}");
    assert_eq!(named(r"reveal-[0-9a-f]{12}\.css"), 1, "{names:?}");
    assert_eq!(named(r"white-[0-9a-f]{12}\.css"), 1, "{names:?}");
    assert_eq!(named(r"highlight-[0-9a-f]{12}\.js"), 1, "{names:?}");
    assert_eq!(named(r"monokai-[0-9a-f]{12}\.css"), 1, "{names:?}");
    assert_eq!(named(r"katex\.min-[0-9a-f]{12}\.(css|js)"), 2, "{names:?}");
    assert_eq!(named(r"math-[0-9a-f]{12}\.js"), 1, "{names:?}");
    assert_eq!(named(r"KaTeX_.*-[0-9a-f]{12}\.woff2"), 20, "{names:?}");
    assert_eq!(
        names.iter().collect::<BTreeSet<_>>().len(),
        names.len(),
        "no duplicates"
    );
    // Every relative reference of the document is an emitted file, and vice versa for the
    // files the document links directly.
    let reference = Regex::new(r#"(?:src|href)="(assets/[^"]+)""#).unwrap();
    let referenced: BTreeSet<String> = reference
        .captures_iter(html)
        .map(|caps| caps[1].trim_start_matches("assets/").to_string())
        .collect();
    for name in &referenced {
        assert!(names.contains(name), "{name} referenced but not emitted");
    }
    let theme_link = Regex::new(
        r#"<link id="presenter-theme" rel="stylesheet" href="assets/theme-[0-9a-f]{12}\.css">"#,
    )
    .unwrap();
    assert!(theme_link.is_match(html));
    assert!(
        !html.contains("<style"),
        "stylesheets are files in site mode"
    );
    // The KaTeX stylesheet file refers to its fonts by bare name (it lives in assets/).
    let katex_css = doc
        .files
        .iter()
        .find(|f| {
            f.path.to_string_lossy().contains("katex.min-") && f.path.extension().unwrap() == "css"
        })
        .unwrap();
    let katex_css = String::from_utf8(katex_css.bytes.clone()).unwrap();
    assert!(
        Regex::new(r"url\(KaTeX_Main-Regular-[0-9a-f]{12}\.woff2\)")
            .unwrap()
            .is_match(&katex_css)
    );
    // The theme stylesheet file is the `theme_css` of the document.
    let theme_file = doc
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("theme-"))
        .unwrap();
    assert_eq!(theme_file.bytes, doc.theme_css.as_bytes());
}

#[test]
fn katex_is_included_only_when_math_is_enabled() {
    let theme = default_theme();
    let mut no_math = sample_deck();
    no_math.features.math = false;
    // auto + no math: nothing KaTeX-related in the output.
    let doc = render_with(&no_math, &theme, &Config::default(), OutputMode::Standalone);
    assert!(!doc.html.contains("katex"));
    assert!(!doc.html.contains("__presenterKatexOptions"));
    let outside_theme_css = doc.html.replace(&doc.theme_css, "");
    assert!(!outside_theme_css.to_lowercase().contains("katex"));
    // auto + math: CSS inlined, fonts as data:font/woff2 URIs, scripts inlined.
    let doc = render_sample(OutputMode::Standalone);
    assert_eq!(doc.html.matches("url(data:font/woff2;base64,").count(), 20);
    assert!(
        !Regex::new(r"url\(fonts/KaTeX_[A-Za-z0-9_-]+\.woff2\)")
            .unwrap()
            .is_match(&doc.html),
        "every woff2 font is inlined"
    );
    assert!(doc.html.contains("katex.render"));
    // always: included without math in the deck.
    let mut config = Config::default();
    config.math.enabled = MathMode::Always;
    let doc = render_with(&no_math, &theme, &config, OutputMode::Standalone);
    assert_eq!(doc.html.matches("url(data:font/woff2;base64,").count(), 20);
    assert!(doc.html.contains("window.__presenterKatexOptions = {};"));
    // never: excluded although the deck has math.
    config.math.enabled = MathMode::Never;
    let doc = render_with(&sample_deck(), &theme, &config, OutputMode::Standalone);
    assert!(!doc.html.contains("katex"));
    // katex_options reach the page (sorted keys).
    let mut config = Config::default();
    config.math.katex_options = json!({"macros": {"\\R": "\\mathbb{R}"}, "fleqn": true})
        .as_object()
        .unwrap()
        .clone();
    let doc = render_with(&sample_deck(), &theme, &config, OutputMode::Serve);
    assert!(doc.html.contains(
        r#"window.__presenterKatexOptions = {"fleqn":true,"macros":{"\\R":"\\mathbb{R}"}};"#
    ));
}

#[test]
fn front_matter_math_table_applies() {
    // `math:` in front matter (kept in meta.vars) overrides presenter.toml.
    let mut deck = sample_deck();
    deck.meta
        .vars
        .insert("math".into(), json!({"enabled": "never"}));
    let doc = render_with(
        &deck,
        &default_theme(),
        &Config::default(),
        OutputMode::Standalone,
    );
    assert!(!doc.html.contains("katex"));
}

#[test]
fn reveal_config_json_precedence() {
    let theme = theme::resolve(
        root()
            .join("tests/fixtures/themes/child-theme")
            .to_str()
            .unwrap(),
        &[],
    )
    .unwrap();
    assert_eq!(theme.reveal_defaults()["transition"], "fade");
    let mut config = Config::default();
    let mut meta = DeckMeta::default();
    let transition = |json: String| -> Value {
        serde_json::from_str::<Value>(&json).unwrap()["transition"].clone()
    };
    // built-in "slide" < theme "fade"
    assert_eq!(
        transition(reveal_config_json(&config, &meta, &theme)),
        "fade"
    );
    // theme < presenter.toml
    config.reveal.insert("transition".into(), json!("slide"));
    assert_eq!(
        transition(reveal_config_json(&config, &meta, &theme)),
        "slide"
    );
    // presenter.toml < front matter
    meta.reveal.insert("transition".into(), json!("zoom"));
    let json = reveal_config_json(&config, &meta, &theme);
    assert_eq!(transition(json.clone()), "zoom");
    // Sorted keys, plugins stripped, integral numbers without ".0", width/height defaults.
    assert_eq!(
        json,
        r#"{"center":true,"controls":true,"hash":true,"height":720,"progress":true,"slideNumber":"c/t","transition":"zoom","width":1280}"#
    );
    config.reveal.insert("width".into(), json!(1024.0));
    config.reveal.insert("plugins".into(), json!(["zoom"]));
    meta.reveal.insert("margin".into(), json!(0.04));
    let json = reveal_config_json(&config, &meta, &theme);
    assert!(json.ends_with(r#""width":1024}"#), "{json}");
    assert!(json.contains(r#""margin":0.04,"#), "{json}");
    assert!(!json.contains("plugins"), "{json}");
    // The default theme sets no [reveal]: the built-ins show through.
    let json = reveal_config_json(&Config::default(), &DeckMeta::default(), &default_theme());
    assert_eq!(json, Config::default().reveal_options_json());
}

#[test]
fn unknown_layout_falls_back_to_default_and_warns() {
    let mut deck = sample_deck();
    deck.slides[1].layout = "nope".into();
    deck.slides[1].children[0].layout = "../deck".into();
    let doc = render_with(
        &deck,
        &default_theme(),
        &Config::default(),
        OutputMode::Standalone,
    );
    let warnings: Vec<_> = doc.diagnostics.iter().collect();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    for warning in &warnings {
        assert_eq!(warning.code, "unknown-layout");
        assert_eq!(warning.level, Level::Warning);
        assert_eq!(
            warning.path.as_deref(),
            Some(Path::new("/abs/talk/slides.md"))
        );
    }
    assert!(
        warnings[0]
            .message
            .starts_with("slide 2: unknown layout 'nope'"),
        "{}",
        warnings[0].message
    );
    assert!(
        warnings[0]
            .message
            .contains("blank, default, section, title")
    );
    assert_eq!(warnings[0].span.as_ref().unwrap().start_line, 19);
    assert!(
        warnings[1]
            .message
            .starts_with("slide 2.1: unknown layout '../deck'")
    );
    // Rendered with the default layout (which still names the layout in its class).
    let region = slides_region(&doc.html);
    assert!(
        region.contains("<section class=\"layout-nope\">\n<h2>Agenda</h2>"),
        "{region}"
    );
    assert!(
        region.contains("<section class=\"layout-..&#x2f;deck\">\n<h2>A vertical sub-slide</h2>")
    );
    // The context carries the decision.
    let registry = registry();
    let theme = default_theme();
    let input = RenderInput {
        deck: &deck,
        assets: &registry,
        theme: &theme,
        config: &Config::default(),
        output: OutputMode::Standalone,
        build: BuildInfo::build(true),
    };
    let mut diagnostics = Default::default();
    let ctx =
        serde_json::to_value(render::build_context(&input, &mut diagnostics).unwrap()).unwrap();
    assert_eq!(ctx["slides"][0]["layout_template"], "layouts/title.html");
    assert_eq!(ctx["slides"][1]["layout_template"], "layouts/default.html");
    assert_eq!(
        ctx["slides"][1]["children"][0]["layout_template"],
        "layouts/default.html"
    );
    assert_eq!(
        ctx["slides"][1]["children"][1]["layout_template"],
        "layouts/default.html"
    );
    assert_eq!(ctx["slides"][7]["layout_template"], "layouts/section.html");
}

/// A theme directory on disk extending the built-in default.
fn disk_theme(dir: &Path, manifest: &str, files: &[(&str, &[u8])]) -> Theme {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("theme.toml"), manifest).unwrap();
    for (rel, bytes) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    theme::resolve(dir.to_str().unwrap(), &[]).unwrap()
}

#[test]
fn helpers_with_an_unknown_path_name_the_path_and_the_template() {
    let tmp = tempfile::tempdir().unwrap();
    let cases = [
        (
            "deck.html",
            "line 1\n{{ style('nope.css') }}",
            "deck.html",
            "style(\"nope.css\")",
        ),
        (
            "deck.html",
            "{% include 'partials/x.html' %}",
            "partials/x.html",
            "script(\"vendor/nope.js\")",
        ),
        (
            "deck.html",
            "{{ asset('@default/nope.png') }}",
            "deck.html",
            "asset(\"@default/nope.png\")",
        ),
        (
            "deck.html",
            "{{ asset('@other/x.png') }}",
            "deck.html",
            "asset(\"@other/x.png\")",
        ),
        (
            "deck.html",
            "{{ script('../escape.js') }}",
            "deck.html",
            "script(\"../escape.js\")",
        ),
    ];
    for (i, (name, source, template_name, call)) in cases.into_iter().enumerate() {
        let dir = tmp.path().join(format!("t{i}"));
        let theme = disk_theme(
            &dir,
            &format!("name = \"t{i}\"\nextends = \"default\"\n"),
            &[
                (&format!("templates/{name}"), source.as_bytes()),
                (
                    "templates/partials/x.html",
                    b"\n\n{{ script('vendor/nope.js') }}",
                ),
            ],
        );
        for output in [OutputMode::Standalone, site(), OutputMode::Serve] {
            let err = try_render_with(&sample_deck(), &theme, &Config::default(), output.clone())
                .expect_err("unknown path");
            let Error::Template {
                template,
                line,
                message,
            } = err
            else {
                panic!("expected Error::Template, got {err:?}");
            };
            assert_eq!(template, template_name, "{call} in {output:?}");
            assert!(message.contains(call), "{message}");
            assert!(message.contains("unknown path"), "{message}");
            if template_name == "partials/x.html" {
                assert_eq!(line, Some(3));
            }
        }
    }
}

#[test]
fn theme_files_use_the_theme_path_form() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mine");
    let theme = disk_theme(
        &dir,
        r#"name = "mine"
extends = "default"
[styles]
entries = ["styles/mine.css"]
highlight_style = "styles/hl.css"
[scripts]
entries = ["scripts/extra.js"]
"#,
        &[
            ("styles/mine.css", b".mine { background: url(../assets/bg.png); }\n.logo { background: url('logo.png?v=2#x'); }\n"),
            ("styles/hl.css", b".hljs { color: red; }"),
            ("scripts/extra.js", b"window.extra = '</script>';"),
            ("assets/bg.png", b"bg-bytes"),
            ("assets/logo.png", b"logo-bytes"),
        ],
    );
    let deck = sample_deck();
    let registry = registry();
    let input = RenderInput {
        deck: &deck,
        assets: &registry,
        theme: &theme,
        config: &Config::default(),
        output: OutputMode::Serve,
        build: BuildInfo::serve(3),
    };
    let ctx = serde_json::to_value(render::build_context(&input, &mut Default::default()).unwrap())
        .unwrap();
    assert_eq!(ctx["theme"]["scripts"], json!(["@mine/scripts/extra.js"]));
    assert_eq!(ctx["theme"]["highlight_style"], "@mine/styles/hl.css");
    assert_eq!(ctx["theme"]["chain"], json!(["mine", "default"]));

    // serve: helper URLs in the @ form; theme CSS assets served by id from `assets`.
    let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Serve);
    assert!(
        doc.html
            .contains(r#"<script src="/__presenter/asset/@mine/scripts/extra.js"></script>"#)
    );
    assert!(
        doc.html
            .contains(r#"<link rel="stylesheet" href="/__presenter/asset/@mine/styles/hl.css">"#)
    );
    let deck_asset = Regex::new(r"/__presenter/deck-asset/([0-9a-f]{12})").unwrap();
    let ids: Vec<String> = deck_asset
        .captures_iter(&doc.theme_css)
        .map(|caps| caps[1].to_string())
        .collect();
    assert_eq!(ids.len(), 2, "{}", doc.theme_css);
    assert_eq!(doc.assets.read(&ids[0]).unwrap().as_ref(), b"bg-bytes");
    assert_eq!(doc.assets.read(&ids[1]).unwrap().as_ref(), b"logo-bytes");
    assert!(
        doc.theme_css
            .contains(&format!("url('/__presenter/deck-asset/{}?v=2#x')", ids[1]))
    );
    assert!(
        doc.theme_css
            .find("/* @default/styles/theme.css */")
            .unwrap()
            < doc.theme_css.find("/* @mine/styles/mine.css */").unwrap()
    );

    // standalone: everything inlined; the script cannot close its element early.
    let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Standalone);
    assert!(doc.html.contains("window.extra = '<\\/script>';"));
    assert!(doc.html.contains(".hljs { color: red; }"));
    assert!(doc.html.contains("url(data:image/png;base64,YmctYnl0ZXM=)"));
    assert!(
        doc.html
            .contains("url('data:image/png;base64,bG9nby1ieXRlcw==#x')")
    );

    // site: the theme stylesheet file names its assets by bare name.
    let doc = render_with(&deck, &theme, &Config::default(), site());
    assert!(
        Regex::new(r"url\(bg-[0-9a-f]{12}\.png\)")
            .unwrap()
            .is_match(&doc.theme_css)
    );
    let names: Vec<String> = doc
        .files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    for prefix in ["bg-", "logo-", "extra-", "hl-", "theme-"] {
        assert!(
            names.iter().any(|n| n.starts_with(prefix)),
            "{prefix} in {names:?}"
        );
    }
}

#[test]
fn theme_stylesheet_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let scss = disk_theme(
        &tmp.path().join("scss"),
        "name = \"scss\"\nextends = \"default\"\n[styles]\nentries = [\"styles/x.scss\"]\n",
        &[("styles/x.scss", b"$a: 1;")],
    );
    let err = try_render_with(
        &sample_deck(),
        &scss,
        &Config::default(),
        OutputMode::Standalone,
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Style { file, message } if file == "@scss/styles/x.scss" && message.contains("SCSS support arrives in T42")),
        "{err:?}"
    );
    let missing = disk_theme(
        &tmp.path().join("missing"),
        "name = \"missing\"\nextends = \"default\"\n[styles]\nentries = [\"styles/x.css\"]\n",
        &[("styles/x.css", b"a { background: url(nope.png) }")],
    );
    let err = try_render_with(
        &sample_deck(),
        &missing,
        &Config::default(),
        OutputMode::Serve,
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Style { file, message } if file == "@missing/styles/x.css" && message.contains("url(nope.png)")),
        "{err:?}"
    );
}

#[test]
fn resolve_helper_path_forms() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_theme(
        &tmp.path().join("t"),
        "name = \"t\"\nextends = \"default\"\n",
        &[
            ("assets/img/logo.svg", b"<svg/>"),
            ("notes.txt", b"root file"),
        ],
    );
    let resolve = |path: &str| render::resolve_helper_path(&theme, path).unwrap();
    let (bytes, mime) = resolve("vendor/reveal/dist/reveal.js").unwrap();
    assert!(bytes.len() > 100_000);
    assert!(mime.contains("javascript"), "{mime}");
    assert!(
        resolve("presenter/math.js")
            .unwrap()
            .0
            .starts_with(b"// KaTeX")
    );
    assert_eq!(
        resolve("vendor/katex/fonts/KaTeX_Main-Regular.woff2")
            .unwrap()
            .1,
        "font/woff2"
    );
    assert_eq!(
        resolve("img/logo.svg").unwrap(),
        (b"<svg/>".to_vec(), "image/svg+xml".into())
    );
    assert_eq!(resolve("./img//logo.svg").unwrap().0, b"<svg/>");
    assert_eq!(resolve("@t/notes.txt").unwrap().0, b"root file");
    assert_eq!(resolve("@t/assets/img/logo.svg").unwrap().0, b"<svg/>");
    assert!(
        resolve("@default/theme.toml")
            .unwrap()
            .0
            .starts_with(b"# The built-in theme")
    );
    assert!(resolve("@default/styles/theme.css").is_some());
    for missing in [
        "vendor/nope.js",
        "@nope/theme.toml",
        "@t/../t/notes.txt",
        "@t/../../etc/passwd",
        "../t/notes.txt",
        "notes.txt",
        "vendor/../themes/default/theme.toml",
        "",
        "/",
    ] {
        assert_eq!(resolve(missing), None, "{missing}");
    }
}

#[test]
fn context_has_the_spec_shape() {
    let deck = sample_deck();
    let registry = registry();
    let theme = default_theme();
    let input = RenderInput {
        deck: &deck,
        assets: &registry,
        theme: &theme,
        config: &Config::default(),
        output: OutputMode::Standalone,
        build: BuildInfo::build(true),
    };
    let mut diagnostics = Default::default();
    let ctx =
        serde_json::to_value(render::build_context(&input, &mut diagnostics).unwrap()).unwrap();
    let keys: BTreeSet<&str> = ctx
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "build", "config", "deck", "features", "meta", "reveal", "slides", "theme"
        ])
    );
    assert_eq!(ctx["deck"], serde_json::to_value(&deck).unwrap());
    assert_eq!(ctx["meta"], serde_json::to_value(&deck.meta).unwrap());
    assert_eq!(
        ctx["features"],
        json!({"math": true, "code": true, "notes": true, "notebook": true})
    );
    assert_eq!(
        ctx["build"],
        json!({"mode": "build", "standalone": true, "live_reload": false, "id": 0, "version": env!("CARGO_PKG_VERSION")})
    );
    assert_eq!(
        ctx["theme"],
        json!({"name": "default", "chain": ["default"], "reveal_theme": "white", "highlight_style": "monokai", "vars": {}, "scripts": []})
    );
    assert_eq!(
        ctx["reveal"]["plugins"],
        json!([
            "RevealHighlight",
            "RevealNotes",
            "RevealZoom",
            "RevealSearch"
        ])
    );
    assert_eq!(
        ctx["reveal"]["plugin_scripts"][0],
        "vendor/reveal/dist/plugin/highlight.js"
    );
    assert_eq!(
        ctx["reveal"]["config_json"],
        r#"{"center":true,"controls":true,"hash":true,"height":720,"progress":true,"slideNumber":"c/t","transition":"fade","width":1280}"#
    );
    // config: the effective config; reveal includes built-in defaults and front matter.
    assert_eq!(ctx["config"]["reveal"]["transition"], "fade");
    assert_eq!(ctx["config"]["reveal"]["width"], 1280);
    assert_eq!(
        ctx["config"]["reveal"]["plugins"],
        json!(["highlight", "notes", "zoom", "search"])
    );
    assert_eq!(ctx["config"]["math"]["enabled"], "auto");
    assert_eq!(ctx["config"]["notebook"]["show_prompts"], true);
    // Slides are the IR slides plus layout_template (children too).
    let mut slide = ctx["slides"][1].clone();
    assert_eq!(
        slide["children"][1]["layout_template"],
        "layouts/default.html"
    );
    let obj = slide.as_object_mut().unwrap();
    obj.remove("layout_template");
    for child in obj["children"].as_array_mut().unwrap() {
        child.as_object_mut().unwrap().remove("layout_template");
    }
    assert_eq!(slide, serde_json::to_value(&deck.slides[1]).unwrap());
    assert_eq!(BuildInfo::serve(4).mode, BuildMode::Serve);
}

#[test]
fn plugins_follow_deck_features() {
    let mut deck = sample_deck();
    deck.features.code = false;
    deck.features.notes = false;
    let doc = render_with(
        &deck,
        &default_theme(),
        &Config::default(),
        OutputMode::Serve,
    );
    assert!(!doc.html.contains("plugin/highlight"));
    assert!(!doc.html.contains("plugin/notes"));
    assert!(
        !doc.html.contains("monokai"),
        "highlight CSS follows features.code"
    );
    assert!(doc.html.contains("{ plugins: [RevealZoom, RevealSearch] }"));
}

#[test]
fn standalone_background_videos_are_reported() {
    let mut deck = sample_deck();
    deck.slides[2]
        .attrs
        .insert("data-background-video".into(), format!("asset:{ASSET_ID}"));
    deck.slides[1].children[1].attrs.insert(
        "data-background-video".into(),
        "https://example.org/v.mp4".into(),
    );
    let doc = render_with(
        &deck,
        &default_theme(),
        &Config::default(),
        OutputMode::Standalone,
    );
    let codes: Vec<&str> = doc.diagnostics.iter().map(|d| d.code).collect();
    assert_eq!(codes, ["standalone-background-video"]);
    let warning = doc.diagnostics.iter().next().unwrap();
    assert!(
        warning.message.starts_with("slide 3: "),
        "{}",
        warning.message
    );
    assert!(warning.message.contains("--site"));
    assert_eq!(warning.span.as_ref().unwrap().start_line, 47);
    for output in [site(), OutputMode::Serve] {
        let doc = render_with(&deck, &default_theme(), &Config::default(), output);
        assert!(doc.diagnostics.is_empty());
    }
}

#[test]
fn markdown_and_attrs_filters_in_templates() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_theme(
        &tmp.path().join("f"),
        "name = \"f\"\nextends = \"default\"\n",
        &[(
            "templates/deck.html",
            b"<p>{{ meta.subtitle|markdown }}</p>|{{ '*a* $x$'|markdown }}|<x{{ {'data-q': 'a\"b', 'data-n': none}|attrs }}>|{{ meta.nothing|markdown }}",
        )],
    );
    let mut deck = sample_deck();
    deck.meta.subtitle = Some("Fast *tools* & <b>raw</b>".into());
    let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Standalone);
    assert_eq!(
        doc.html,
        "<p>Fast <em>tools</em> &amp; <b>raw</b></p>|<em>a</em> <span data-math-style=\"inline\">x</span>|<x data-q=\"a&quot;b\">|"
    );
}

#[test]
fn writes_previews_for_manual_browser_checks() {
    let preview = root().join("target/preview");
    let standalone = render_sample(OutputMode::Standalone);
    std::fs::create_dir_all(&preview).unwrap();
    std::fs::write(preview.join("render-standalone.html"), &standalone.html).unwrap();
    let site_dir = preview.join("render-site");
    let _ = std::fs::remove_dir_all(&site_dir);
    let doc = render_sample(OutputMode::Site {
        assets_dir: PathBuf::from("assets"),
        url_prefix: "assets/".into(),
    });
    std::fs::create_dir_all(site_dir.join("assets")).unwrap();
    std::fs::write(site_dir.join("index.html"), &doc.html).unwrap();
    for file in &doc.files {
        std::fs::write(site_dir.join(&file.path), &file.bytes).unwrap();
    }
    assert!(site_dir.join("index.html").is_file());
    // Serve mode: the document and the stylesheet a dev server would hold.
    let serve = render_sample(OutputMode::Serve);
    std::fs::write(preview.join("render-serve.html"), &serve.html).unwrap();
    std::fs::write(preview.join("render-serve-theme.css"), &serve.theme_css).unwrap();
}
