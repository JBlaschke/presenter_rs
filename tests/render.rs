//! T15: the HTML renderer (`presenter::render`) with the built-in default theme, in all
//! three output modes (docs/spec/theme-format.md §4–§5, config.md §4).
//!
//! The fixture deck is `tests/fixtures/ir/sample-deck.json`; its only asset
//! (`asset:32debefb8003`) is `tests/fixtures/md/images/ok.png` registered by bytes. The
//! renderer takes the effective config, so the tests merge the theme and front matter first
//! with `context::effective_config`, as the pipeline does.
//! `writes_previews_for_manual_browser_checks` (ignored; run it with
//! `cargo test --test render -- --ignored`) writes the standalone, site and serve outputs to
//! `target/preview/render-*` for the manual browser check.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use presenter::assets::emit::{OutputMode, data_uri};
use presenter::assets::registry::AssetRegistry;
use presenter::config::{Config, MathMode};
use presenter::error::{Error, Level};
use presenter::ir::{Deck, DeckMeta};
use presenter::render::{
    self, BuildInfo, BuildMode, RenderInput, RenderedDocument, context::effective_config,
    reveal::reveal_config_json,
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

/// Renders `deck` with `config` merged as the pipeline does (theme `[reveal]` under it,
/// front matter over it).
fn try_render_with(
    deck: &Deck,
    theme: &Theme,
    config: &Config,
    output: OutputMode,
) -> presenter::Result<RenderedDocument> {
    let config = effective_config(config, &deck.meta, theme)?;
    try_render_effective(deck, theme, &config, output)
}

/// Renders `deck` with `config` as given (already effective).
fn try_render_effective(
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

/// The effective config of `deck` with `theme` and the default `presenter.toml`.
fn effective(deck: &Deck, theme: &Theme) -> Config {
    effective_config(&Config::default(), &deck.meta, theme).expect("front matter merges")
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
        config: &effective(&deck, &theme),
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
        config: &effective(&deck, &theme),
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
    // (template source, template that fails, helper call, reason)
    let cases = [
        (
            "line 1\n{{ style('nope.css') }}",
            "deck.html",
            "style(\"nope.css\")",
            "(looked in the assets/ directories of the theme chain)",
        ),
        (
            "{% include 'partials/x.html' %}",
            "partials/x.html",
            "script(\"vendor/nope.js\")",
            "(looked in the embedded vendor tree)",
        ),
        (
            "{{ asset('@default/nope.png') }}",
            "deck.html",
            "asset(\"@default/nope.png\")",
            "(looked in the directory of theme 'default')",
        ),
        (
            "{{ asset('@other/x.png') }}",
            "deck.html",
            "asset(\"@other/x.png\")",
            "(there is no theme 'other' in the chain t3 → default)",
        ),
        (
            "{{ script('../escape.js') }}",
            "deck.html",
            "script(\"../escape.js\")",
            "(it climbs out of its root directory with '..')",
        ),
        (
            "{{ style('./') }}",
            "deck.html",
            "style(\"./\")",
            "(the path is empty)",
        ),
    ];
    for (i, (source, template_name, call, reason)) in cases.into_iter().enumerate() {
        let dir = tmp.path().join(format!("t{i}"));
        let theme = disk_theme(
            &dir,
            &format!("name = \"t{i}\"\nextends = \"default\"\n"),
            &[
                ("templates/deck.html", source.as_bytes()),
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
            assert!(
                message.contains(&format!("{call}: unknown path {reason}")),
                "{message}"
            );
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
        config: &effective(&deck, &theme),
        output: OutputMode::Serve,
        build: BuildInfo::serve(3),
    };
    let ctx = serde_json::to_value(render::build_context(&input, &mut Default::default()).unwrap())
        .unwrap();
    assert_eq!(ctx["theme"]["scripts"], json!(["@mine/scripts/extra.js"]));
    assert_eq!(ctx["theme"]["highlight_style"], "@mine/styles/hl.css");
    assert_eq!(ctx["theme"]["chain"], json!(["mine", "default"]));

    // serve: helper URLs in the @ form, the theme stylesheet's url()s included, so the dev
    // server's asset route (resolve_helper_path) serves them; no deck asset is involved.
    let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Serve);
    assert!(
        doc.html
            .contains(r#"<script src="/__presenter/asset/@mine/scripts/extra.js"></script>"#)
    );
    assert!(
        doc.html
            .contains(r#"<link rel="stylesheet" href="/__presenter/asset/@mine/styles/hl.css">"#)
    );
    assert!(!doc.theme_css.contains("deck-asset"), "{}", doc.theme_css);
    assert!(
        doc.theme_css
            .contains(".mine { background: url(/__presenter/asset/@mine/assets/bg.png); }"),
        "{}",
        doc.theme_css
    );
    // Found through the §5 assets/ fallback: named by its theme-root helper path.
    assert!(
        doc.theme_css.contains(
            ".logo { background: url('/__presenter/asset/@mine/assets/logo.png?v=2#x'); }"
        ),
        "{}",
        doc.theme_css
    );
    let served = |url: &str| {
        let path = url.strip_prefix(render::html::SERVE_HELPER_PREFIX).unwrap();
        render::resolve_helper_path(&theme, path)
            .unwrap()
            .unwrap()
            .0
    };
    assert_eq!(
        served("/__presenter/asset/@mine/assets/bg.png"),
        b"bg-bytes"
    );
    assert_eq!(
        served("/__presenter/asset/@mine/assets/logo.png"),
        b"logo-bytes"
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
fn style_entry_assets_fallback_takes_any_reference() {
    // §5: a [styles] entry's url(rel) falls back to assets/<rel> in the chain, child first,
    // also when rel starts with vendor/, presenter/ or @ (helper paths that would name the
    // embedded tree or a theme root). The hit is named @<theme>/assets/<rel>.
    let tmp = tempfile::tempdir().unwrap();
    disk_theme(
        &tmp.path().join("p"),
        "name = \"p\"\nextends = \"default\"\n",
        &[
            ("assets/vendor/a.png", b"parent-a"),
            ("assets/presenter/b.png", b"parent-b"),
        ],
    );
    let css = ".a { background: url(vendor/a.png); }\n\
               .b { background: url(presenter/b.png); }\n\
               .c { background: url('@x/c.png'); }\n";
    let theme = disk_theme(
        &tmp.path().join("v"),
        "name = \"v\"\nextends = \"../p\"\n[styles]\nentries = [\"styles/x.css\"]\n",
        &[
            ("styles/x.css", css.as_bytes()),
            ("assets/vendor/a.png", b"child-a"),
            ("assets/@x/c.png", b"child-c"),
        ],
    );
    assert_eq!(theme.chain.len(), 3);
    // (selector, expected bytes, helper path of the hit)
    let expected: [(&str, &[u8], &str); 3] = [
        (".a", b"child-a", "@v/assets/vendor/a.png"),
        (".b", b"parent-b", "@p/assets/presenter/b.png"),
        (".c", b"child-c", "@v/assets/@x/c.png"),
    ];
    let deck = sample_deck();
    let url_of = |css: &str, selector: &str| -> String {
        let pattern = format!(
            r#"{} \{{ background: url\('?([^')]*)'?\); \}}"#,
            regex::escape(selector)
        );
        Regex::new(&pattern)
            .unwrap()
            .captures(css)
            .unwrap_or_else(|| panic!("{selector} in {css}"))[1]
            .to_string()
    };

    let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Standalone);
    for (selector, bytes, _) in expected {
        assert_eq!(
            url_of(&doc.theme_css, selector),
            data_uri("image/png", bytes)
        );
    }
    assert!(doc.html.contains(&data_uri("image/png", b"child-c")));

    let doc = render_with(&deck, &theme, &Config::default(), site());
    for (selector, bytes, _) in expected {
        let name = url_of(&doc.theme_css, selector);
        let file = doc
            .files
            .iter()
            .find(|f| f.path.file_name().unwrap().to_string_lossy() == name)
            .unwrap_or_else(|| panic!("{name} is emitted"));
        assert_eq!(file.bytes, bytes, "{selector}");
    }

    // serve: the URL names the theme file, which the asset route serves; the same paths
    // in the vendor/presenter form would name the embedded tree.
    let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Serve);
    for (selector, bytes, path) in expected {
        let url = url_of(&doc.theme_css, selector);
        assert_eq!(url, format!("/__presenter/asset/{path}"));
        let served = render::resolve_helper_path(&theme, path).unwrap().unwrap();
        assert_eq!(served, (bytes.to_vec(), "image/png".to_string()));
    }

    // Still only for [styles] entries, and a reference that is nowhere is still an error.
    let missing = disk_theme(
        &tmp.path().join("w"),
        "name = \"w\"\nextends = \"../p\"\n[styles]\nentries = [\"styles/x.css\"]\n",
        &[("styles/x.css", b".x { background: url(vendor/nope.png); }")],
    );
    for output in [OutputMode::Standalone, site(), OutputMode::Serve] {
        let err = try_render_with(&deck, &missing, &Config::default(), output.clone())
            .expect_err("unresolvable url()");
        assert!(
            matches!(&err, Error::Style { file, message } if file == "@w/styles/x.css"
                && message == "url(vendor/nope.png) not found next to the stylesheet or in the theme chain's assets/ directories"),
            "{output:?}: {err:?}"
        );
    }
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
        config: &effective(&deck, &theme),
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
fn json_filter_is_minijinja_tojson() {
    // theme-format.md §4 lists `json` among the built-in filters; tojson sorts keys and
    // escapes `<`, so its output is safe inside <script>.
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_theme(
        &tmp.path().join("j"),
        "name = \"j\"\nextends = \"default\"\n",
        &[(
            "templates/deck.html",
            b"{{ features|json }}|{{ {'b': '</script>', 'a': 1}|json }}|{{ theme.chain|json(indent=2) }}",
        )],
    );
    let doc = render_with(
        &sample_deck(),
        &theme,
        &Config::default(),
        OutputMode::Standalone,
    );
    assert_eq!(
        doc.html,
        "{\"code\":true,\"math\":true,\"notebook\":true,\"notes\":true}|{\"a\":1,\"b\":\"\\u003c/script\\u003e\"}|[\n  \"j\",\n  \"default\"\n]"
    );
}

#[test]
fn markdown_filter_follows_smart_punctuation() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_theme(
        &tmp.path().join("m"),
        "name = \"m\"\nextends = \"default\"\n",
        &[("templates/deck.html", b"{{ meta.subtitle|markdown }}")],
    );
    let mut deck = sample_deck();
    deck.meta.subtitle = Some("\"Fast\" tools -- really".into());
    let render = |deck: &Deck, config: &Config| {
        render_with(deck, &theme, config, OutputMode::Standalone).html
    };
    assert_eq!(
        render(&deck, &Config::default()),
        "&quot;Fast&quot; tools -- really"
    );
    let mut config = Config::default();
    config.markdown.smart_punctuation = true;
    assert_eq!(render(&deck, &config), "“Fast” tools – really");
    // Front matter `markdown:` reaches the filter through the effective config.
    deck.meta
        .vars
        .insert("markdown".into(), json!({"smart_punctuation": true}));
    assert_eq!(render(&deck, &Config::default()), "“Fast” tools – really");
}

#[test]
fn command_line_reveal_options_win_over_front_matter() {
    let deck = sample_deck();
    let theme = default_theme();
    let mut config = effective(&deck, &theme);
    let cli = json!({"transition": "convex"});
    config.apply_cli_reveal(cli.as_object().unwrap()).unwrap();
    for output in [OutputMode::Standalone, OutputMode::Serve] {
        let doc = try_render_effective(&deck, &theme, &config, output).unwrap();
        assert!(doc.html.contains(r#""transition":"convex""#));
        assert!(!doc.html.contains(r#""transition":"fade""#));
    }
    let registry = registry();
    let input = RenderInput {
        deck: &deck,
        assets: &registry,
        theme: &theme,
        config: &config,
        output: OutputMode::Serve,
        build: BuildInfo::serve(0),
    };
    let ctx = render::context::context_json(&input, &mut Default::default()).unwrap();
    assert_eq!(ctx["config"]["reveal"]["transition"], "convex");
    assert!(
        ctx["reveal"]["config_json"]
            .as_str()
            .unwrap()
            .contains(r#""transition":"convex""#)
    );
    // The renderer merges nothing itself: a raw config does not get the front matter.
    let doc = try_render_effective(&deck, &theme, &Config::default(), OutputMode::Serve).unwrap();
    assert!(doc.html.contains(r#""slideNumber":false"#));
    assert!(doc.html.contains(r#""transition":"slide""#));
}

#[test]
fn inlined_scripts_never_swallow_the_scripts_after_them() {
    // `<!--` then `<script` with no `-->` would keep the element open past its
    // `</script>`, and Reveal.initialize would become part of this script's text.
    let js = "window.x = '<!-- <script>';";
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_theme(
        &tmp.path().join("x"),
        "name = \"x\"\nextends = \"default\"\n[scripts]\nentries = [\"scripts/x.js\"]\n",
        &[("scripts/x.js", js.as_bytes())],
    );
    let doc = render_with(
        &sample_deck(),
        &theme,
        &Config::default(),
        OutputMode::Standalone,
    );
    let uri = data_uri("text/javascript;charset=utf-8", js.as_bytes());
    let tag = format!("<script src=\"{uri}\"></script>\n<script>window.__presenterKatexOptions");
    assert!(doc.html.contains(&tag));
    assert!(!doc.html.contains(js), "not inlined as text");
    // Every other script is inlined as text (the vendored ones are safe).
    assert_eq!(doc.html.matches("<script src=").count(), 1);
}

#[test]
fn byte_order_marks_are_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_theme(
        &tmp.path().join("b"),
        r#"name = "b"
extends = "default"
[styles]
entries = ["styles/bom.css"]
highlight_style = "styles/hl.css"
[scripts]
entries = ["scripts/bom.js"]
"#,
        &[
            (
                "styles/bom.css",
                "\u{feff}.reveal { outline-color: rgb(1, 2, 3); }\n".as_bytes(),
            ),
            ("styles/hl.css", "\u{feff}.hljs { color: red; }".as_bytes()),
            ("scripts/bom.js", "\u{feff}window.bom = 1;".as_bytes()),
        ],
    );
    for output in [OutputMode::Standalone, site(), OutputMode::Serve] {
        let doc = render_with(&sample_deck(), &theme, &Config::default(), output.clone());
        assert!(
            doc.theme_css
                .contains("/* @b/styles/bom.css */\n.reveal { outline-color: rgb(1, 2, 3); }"),
            "{output:?}"
        );
        assert!(!doc.theme_css.contains('\u{feff}'), "{output:?}");
        if output == OutputMode::Standalone {
            assert!(doc.html.contains("<style>\n.hljs { color: red; }"));
            assert!(doc.html.contains("<script>\nwindow.bom = 1;"));
        }
        if matches!(output, OutputMode::Site { .. }) {
            let hl = doc
                .files
                .iter()
                .find(|f| f.path.to_string_lossy().contains("/hl-"))
                .unwrap();
            assert_eq!(hl.bytes, b".hljs { color: red; }");
        }
    }
}

#[test]
fn backslash_style_entries_resolve_urls_next_to_the_stylesheet() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_theme(
        &tmp.path().join("c"),
        "name = \"c\"\nextends = \"default\"\n[styles]\nentries = [\"styles\\\\x.css\"]\n",
        &[
            ("styles/x.css", b".x { background: url(img/b.png); }"),
            ("styles/img/b.png", b"b-bytes"),
        ],
    );
    let doc = render_with(
        &sample_deck(),
        &theme,
        &Config::default(),
        OutputMode::Standalone,
    );
    assert!(doc.theme_css.contains(
        "/* @c/styles/x.css */\n.x { background: url(data:image/png;base64,Yi1ieXRlcw==); }"
    ));
    let doc = render_with(
        &sample_deck(),
        &theme,
        &Config::default(),
        OutputMode::Serve,
    );
    assert!(
        doc.theme_css
            .contains(".x { background: url(/__presenter/asset/@c/styles/img/b.png); }")
    );
}

#[test]
fn style_urls_resolve_to_the_same_files_in_every_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = |name: &str| {
        format!(
            "name = \"{name}\"\nextends = \"default\"\n[styles]\nhighlight_style = \"styles/hl.css\"\n"
        )
    };
    let theme = disk_theme(
        &tmp.path().join("m"),
        &manifest("m"),
        &[
            ("styles/hl.css", b".hljs { background: url(img/x.png); }"),
            ("styles/img/x.png", b"x-bytes"),
        ],
    );
    let deck = sample_deck();
    let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Standalone);
    assert!(
        doc.html
            .contains(".hljs { background: url(data:image/png;base64,eC1ieXRlcw==); }")
    );
    let doc = render_with(&deck, &theme, &Config::default(), site());
    let hl = doc
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("/hl-"))
        .unwrap();
    let hl = String::from_utf8(hl.bytes.clone()).unwrap();
    let name = Regex::new(r"url\((x-[0-9a-f]{12}\.png)\)")
        .unwrap()
        .captures(&hl)
        .unwrap()[1]
        .to_string();
    let x = doc.files.iter().find(|f| f.path.ends_with(&name)).unwrap();
    assert_eq!(x.bytes, b"x-bytes");
    // serve: the browser resolves url(img/x.png) against the stylesheet's URL; the asset
    // route serves that path with resolve_helper_path.
    let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Serve);
    assert!(
        doc.html
            .contains(r#"<link rel="stylesheet" href="/__presenter/asset/@m/styles/hl.css">"#)
    );
    let served = render::resolve_helper_path(&theme, "@m/styles/img/x.png").unwrap();
    assert_eq!(served.unwrap().0, b"x-bytes");

    // No assets/ fallback for style() (§5 covers [styles] entries only): a browser in serve
    // mode would request @n/styles/logo.png, so every mode rejects url(logo.png).
    let theme = disk_theme(
        &tmp.path().join("n"),
        &manifest("n"),
        &[
            ("styles/hl.css", b".hljs { background: url(logo.png); }"),
            ("assets/logo.png", b"logo-bytes"),
        ],
    );
    assert_eq!(
        render::resolve_helper_path(&theme, "@n/styles/logo.png").unwrap(),
        None
    );
    for output in [OutputMode::Standalone, site(), OutputMode::Serve] {
        let err = try_render_with(&deck, &theme, &Config::default(), output.clone())
            .expect_err("unresolvable url()");
        let Error::Template { message, .. } = &err else {
            panic!("expected Error::Template, got {err:?}");
        };
        assert!(
            message.contains(
                "style '@n/styles/hl.css': url(logo.png) not found next to the stylesheet"
            ),
            "{output:?}: {message}"
        );
        assert!(!message.contains("assets/ directories"), "{message}");
    }
}

#[test]
fn stock_reveal_themes_in_standalone_builds() {
    let names: Vec<String> = presenter::vendor::list("vendor/reveal/dist/theme/")
        .into_iter()
        .filter_map(|path| {
            let file = path.strip_prefix("vendor/reveal/dist/theme/")?;
            Some(file.strip_suffix(".css")?.to_string())
        })
        .collect();
    assert_eq!(names.len(), 14, "{names:?}");
    let mut deck = sample_deck();
    deck.features = Default::default();
    let deck_links = network_references(slides_region(&render_sample(OutputMode::Standalone).html));
    let tmp = tempfile::tempdir().unwrap();
    let mut without_imports = Vec::new();
    for name in &names {
        let theme = disk_theme(
            &tmp.path().join(name),
            &format!(
                "name = \"s-{name}\"\nextends = \"default\"\n[styles]\nreveal_theme = \"{name}\"\n"
            ),
            &[],
        );
        let doc = render_with(&deck, &theme, &Config::default(), OutputMode::Standalone);
        let vendor_css =
            presenter::vendor::get_str(&format!("vendor/reveal/dist/theme/{name}.css")).unwrap();
        let imports = network_references(&vendor_css);
        // The only network references are the deck's own and the theme's imports, and each
        // import is reported.
        let expected: BTreeSet<String> = deck_links.union(&imports).cloned().collect();
        assert_eq!(network_references(&doc.html), expected, "{name}");
        let warnings: Vec<_> = doc.diagnostics.iter().collect();
        assert_eq!(warnings.len(), imports.len(), "{name}: {warnings:?}");
        let call = format!("style(\"vendor/reveal/dist/theme/{name}.css\") imports ");
        for warning in &warnings {
            assert_eq!(warning.code, "remote-import");
            assert_eq!(warning.level, Level::Warning);
            assert!(warning.message.starts_with(&call), "{}", warning.message);
        }
        for url in &imports {
            assert!(
                warnings
                    .iter()
                    .any(|w| w.message.starts_with(&format!("{call}{url}:"))),
                "{name}: {url} reported"
            );
        }
        if imports.is_empty() {
            without_imports.push(name.as_str());
        }
        if name == "simple" {
            for output in [site(), OutputMode::Serve] {
                let doc = render_with(&deck, &theme, &Config::default(), output);
                assert!(doc.diagnostics.is_empty(), "standalone only");
            }
        }
    }
    assert_eq!(
        without_imports,
        [
            "black-contrast",
            "black",
            "dracula",
            "serif",
            "white-contrast",
            "white"
        ]
    );
}

#[test]
fn theme_stylesheet_remote_imports_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let theme = disk_theme(
        &tmp.path().join("r"),
        "name = \"r\"\nextends = \"default\"\n[styles]\nentries = [\"styles/r.css\"]\n",
        &[(
            "styles/r.css",
            b"@import url(https://fonts.example/x.css);\n.r { color: red; }",
        )],
    );
    let doc = render_with(
        &sample_deck(),
        &theme,
        &Config::default(),
        OutputMode::Standalone,
    );
    let messages: Vec<&str> = doc.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].starts_with(
        "theme stylesheet entry '@r/styles/r.css' imports https://fonts.example/x.css"
    ));
}

#[test]
#[ignore = "writes target/preview/render-* for the manual browser check; run with --ignored"]
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
