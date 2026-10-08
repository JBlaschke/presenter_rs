//! Asset registry and emitter (T14; `docs/spec/ir.md` §3, `docs/spec/live-reload.md` §1,
//! `docs/spec/theme-format.md` §4–§5, `docs/spec/vendored-assets.md` §2): placeholders become
//! `data:` URIs (standalone), hashed files (site) or dev-server routes (serve), and CSS
//! `url()` references become placeholders.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use presenter::Error;
use presenter::assets::emit::{
    EmittedFile, Emitter, OutputMode, data_uri, hashed_file_name, register_css_urls,
};
use presenter::assets::registry::{AssetData, AssetRegistry, normalize_lexically};
use presenter::ir::{Block, CellOutput, Deck};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

fn site_mode() -> OutputMode {
    OutputMode::Site {
        assets_dir: PathBuf::from("dist/assets"),
        url_prefix: "assets/".to_string(),
    }
}

fn all_modes() -> [OutputMode; 3] {
    [OutputMode::Standalone, site_mode(), OutputMode::Serve]
}

const SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"/>";
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00";

/// A registry with one file asset (the fixture PNG) and two in-memory assets.
fn registry() -> (AssetRegistry, [String; 3]) {
    let mut registry = AssetRegistry::new();
    let png = registry.register_file(&fixture("md/images/ok.png"));
    let gif = registry.register_bytes(GIF.to_vec(), "image/gif", "background.gif");
    let svg = registry.register_bytes(SVG.to_vec(), "image/svg+xml", "output.svg");
    (registry, [png, gif, svg])
}

fn document([png, gif, svg]: &[String; 3]) -> String {
    format!(
        "<img src=\"asset:{png}\" alt=\"\"> <section data-background-image=\"asset:{gif}\"></section> \
         <div style=\"background:url(asset:{svg})\"></div>"
    )
}

#[test]
fn same_file_twice_is_one_asset_and_different_bytes_are_different_assets() {
    let mut registry = AssetRegistry::new();
    assert!(registry.is_empty());
    let path = fixture("md/images/ok.png");
    let id = registry.register_file(&path);
    assert_eq!(registry.register_file(&path), id);
    assert_eq!(registry.len(), 1);

    let a = registry.register_bytes(b"\x89PNG a".to_vec(), "image/png", "output.png");
    let b = registry.register_bytes(b"\x89PNG b".to_vec(), "image/png", "output.png");
    assert_ne!(a, b);
    assert_eq!(registry.len(), 3);
    assert!(!registry.is_empty());
}

#[test]
fn resolve_html_inlines_all_three_references_in_standalone_mode() {
    let (registry, ids) = registry();
    let png_bytes = std::fs::read(fixture("md/images/ok.png")).unwrap();
    let mode = OutputMode::Standalone;
    let mut emitter = Emitter::new(&registry, &mode);
    let out = emitter.resolve_html(&document(&ids)).unwrap();
    assert_eq!(
        out,
        format!(
            "<img src=\"{}\" alt=\"\"> <section data-background-image=\"{}\"></section> \
             <div style=\"background:url({})\"></div>",
            data_uri("image/png", &png_bytes),
            data_uri("image/gif", GIF),
            data_uri("image/svg+xml", SVG),
        )
    );
    assert!(out.contains("src=\"data:image/png;base64,iVBORw0KGgo"));
    assert!(!out.contains("asset:"));
    assert!(emitter.files.is_empty());
}

#[test]
fn resolve_html_emits_hashed_files_in_site_mode() {
    let (registry, [png, gif, svg]) = registry();
    let mode = site_mode();
    let mut emitter = Emitter::new(&registry, &mode);
    let out = emitter
        .resolve_html(&document(&[png.clone(), gif.clone(), svg.clone()]))
        .unwrap();
    assert_eq!(
        out,
        format!(
            "<img src=\"assets/ok-{png}.png\" alt=\"\"> \
             <section data-background-image=\"assets/background-{gif}.gif\"></section> \
             <div style=\"background:url(assets/output-{svg}.svg)\"></div>"
        )
    );
    let png_bytes = std::fs::read(fixture("md/images/ok.png")).unwrap();
    assert_eq!(
        emitter.files,
        [
            EmittedFile {
                path: PathBuf::from(format!("dist/assets/ok-{png}.png")),
                bytes: png_bytes,
            },
            EmittedFile {
                path: PathBuf::from(format!("dist/assets/background-{gif}.gif")),
                bytes: GIF.to_vec(),
            },
            EmittedFile {
                path: PathBuf::from(format!("dist/assets/output-{svg}.svg")),
                bytes: SVG.to_vec(),
            },
        ]
    );
}

#[test]
fn resolve_html_uses_dev_server_routes_in_serve_mode() {
    let (registry, [png, gif, svg]) = registry();
    let mode = OutputMode::Serve;
    let mut emitter = Emitter::new(&registry, &mode);
    let out = emitter
        .resolve_html(&document(&[png.clone(), gif.clone(), svg.clone()]))
        .unwrap();
    assert_eq!(
        out,
        format!(
            "<img src=\"/__presenter/deck-asset/{png}\" alt=\"\"> \
             <section data-background-image=\"/__presenter/deck-asset/{gif}\"></section> \
             <div style=\"background:url(/__presenter/deck-asset/{svg})\"></div>"
        )
    );
    assert!(emitter.files.is_empty());
    assert_eq!(
        emitter.url_for(&png).unwrap(),
        format!("/__presenter/deck-asset/{png}")
    );
}

#[test]
fn unknown_id_is_an_other_error_naming_the_id_in_every_mode() {
    let (registry, ids) = registry();
    let html = format!("{} <img src=\"asset:0123456789ab\">", document(&ids));
    for mode in all_modes() {
        let mut emitter = Emitter::new(&registry, &mode);
        let err = emitter.resolve_html(&html).unwrap_err();
        assert!(matches!(err, Error::Other(_)), "{mode:?}: {err:?}");
        assert!(err.to_string().contains("0123456789ab"), "{mode:?}: {err}");
        let err = emitter
            .resolve_css("x{background:url(asset:0123456789ab)}")
            .unwrap_err();
        assert!(err.to_string().contains("0123456789ab"), "{mode:?}: {err}");
        let err = emitter.url_for("0123456789ab").unwrap_err();
        assert!(matches!(err, Error::Other(_)), "{mode:?}: {err:?}");
    }
}

#[test]
fn font_face_fallbacks_lenient_and_strict() {
    let css = "@font-face{src:url(fonts/a.woff2) format('woff2'), url(fonts/a.woff)}";
    let woff2 = b"wOF2 font bytes".to_vec();
    let mut resolver =
        |reference: &str| (reference == "fonts/a.woff2").then(|| AssetData::Bytes(woff2.clone()));

    let mut registry = AssetRegistry::new();
    let out = register_css_urls(css, &mut registry, &mut resolver, true).unwrap();
    assert_eq!(registry.len(), 1);
    let asset = registry.iter().next().unwrap();
    assert_eq!(asset.mime, "font/woff2");
    assert_eq!(asset.file_name_hint, "a.woff2");
    assert_eq!(
        out,
        format!(
            "@font-face{{src:url(asset:{}) format('woff2'), url(fonts/a.woff)}}",
            asset.id
        )
    );

    // The placeholder then inlines like any other asset.
    let mode = OutputMode::Standalone;
    let inlined = Emitter::new(&registry, &mode).resolve_css(&out).unwrap();
    assert_eq!(
        inlined,
        format!(
            "@font-face{{src:url({}) format('woff2'), url(fonts/a.woff)}}",
            data_uri("font/woff2", b"wOF2 font bytes")
        )
    );

    let mut strict = AssetRegistry::new();
    let err = register_css_urls(css, &mut strict, &mut resolver, false).unwrap_err();
    assert!(
        matches!(&err, Error::MissingAsset { path, span: None } if path == Path::new("fonts/a.woff")),
        "{err:?}"
    );
}

#[test]
fn data_uri_of_png_magic() {
    assert!(data_uri("image/png", b"\x89PNG").starts_with("data:image/png;base64,iVBORw"));
}

#[test]
fn site_mode_writes_exactly_one_file_per_distinct_id_with_deterministic_names() {
    let (registry, [png, gif, svg]) = registry();
    let html = format!(
        "<img src=\"asset:{png}\"><img src=\"asset:{png}\" srcset=\"asset:{png} 1x, asset:{gif} 2x\">"
    );
    let css = format!("a{{b:url(asset:{gif})}} c{{d:url(asset:{png})}}");
    let mode = site_mode();

    let emit = || {
        let mut emitter = Emitter::new(&registry, &mode);
        let html_out = emitter.resolve_html(&html).unwrap();
        let css_out = emitter.resolve_css(&css).unwrap();
        // Asking again for an id already emitted does not emit it twice.
        assert_eq!(
            emitter.url_for(&png).unwrap(),
            format!("assets/ok-{png}.png")
        );
        (html_out, css_out, emitter.files)
    };
    let (html_out, css_out, files) = emit();
    let names: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(
        names,
        [
            Path::new("dist/assets").join(hashed_file_name("ok.png", &png)),
            Path::new("dist/assets").join(hashed_file_name("background.gif", &gif)),
        ]
    );
    assert!(!html_out.contains(&svg) && !css_out.contains("asset:"));
    assert_eq!(
        html_out.matches(&format!("assets/ok-{png}.png")).count(),
        3,
        "{html_out}"
    );

    // A second, independent emission produces byte-identical output and files.
    assert_eq!(emit(), (html_out, css_out, files));
}

#[test]
fn deleted_file_is_missing_asset_at_emit_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("figure.png");
    std::fs::write(&path, b"\x89PNG figure").unwrap();
    let mut registry = AssetRegistry::new();
    let id = registry.register_file(&path);
    let AssetData::File(stored) = registry.get(&id).unwrap().data.clone() else {
        panic!("expected a file asset");
    };
    assert_eq!(&*registry.read(&id).unwrap(), b"\x89PNG figure");

    // Deleted between parse and emit.
    std::fs::remove_file(&path).unwrap();
    let is_missing =
        |err: &Error| matches!(err, Error::MissingAsset { path, span: None } if *path == stored);
    let err = registry.read(&id).unwrap_err();
    assert!(is_missing(&err), "{err:?}");

    let html = format!("<img src=\"asset:{id}\">");
    for mode in [OutputMode::Standalone, site_mode()] {
        let mut emitter = Emitter::new(&registry, &mode);
        let err = emitter.resolve_html(&html).unwrap_err();
        assert!(is_missing(&err), "{mode:?}: {err:?}");
        assert!(emitter.files.is_empty());
    }
    // Serve mode reads nothing at render time; the dev server reports it on request.
    let serve = OutputMode::Serve;
    let out = Emitter::new(&registry, &serve).resolve_html(&html).unwrap();
    assert_eq!(out, format!("<img src=\"/__presenter/deck-asset/{id}\">"));
}

/// The real vendored KaTeX stylesheet references `.woff2`, `.woff` and `.ttf` for every
/// font; only the woff2 files are vendored (vendored-assets.md §2).
#[test]
fn katex_css_inlines_woff2_fonts_and_leaves_fallbacks_untouched() {
    let css = presenter::vendor::get_str("vendor/katex/katex.min.css").unwrap();
    fn katex_file(reference: &str) -> Option<AssetData> {
        let bytes = presenter::vendor::get(&format!("vendor/katex/{reference}"))?;
        Some(AssetData::Bytes(bytes.into_owned()))
    }
    let mut resolved = Vec::new();
    let mut registry = AssetRegistry::new();
    let mut resolver = |reference: &str| {
        let data = katex_file(reference)?;
        resolved.push(reference.to_string());
        Some(data)
    };
    let out = register_css_urls(&css, &mut registry, &mut resolver, true).unwrap();
    let woff2 = presenter::vendor::list("vendor/katex/fonts/");
    assert_eq!(woff2.len(), 20);
    assert_eq!(resolved.len(), 20, "{resolved:?}");
    assert!(resolved.iter().all(|r| r.ends_with(".woff2")));
    assert_eq!(registry.len(), 20);
    assert!(registry.iter().all(|a| a.mime == "font/woff2"));
    assert!(out.contains("url(fonts/KaTeX_Main-Regular.woff)"));
    assert!(out.contains("url(fonts/KaTeX_Main-Regular.ttf)"));
    assert!(!out.contains("url(fonts/KaTeX_Main-Regular.woff2)"));

    let mode = OutputMode::Standalone;
    let inlined = Emitter::new(&registry, &mode).resolve_css(&out).unwrap();
    assert_eq!(inlined.matches("url(data:font/woff2;base64,").count(), 20);
    assert!(!inlined.contains("asset:"));

    // Without leniency the first missing fallback is an error.
    let err =
        register_css_urls(&css, &mut AssetRegistry::new(), &mut katex_file, false).unwrap_err();
    assert!(
        matches!(&err, Error::MissingAsset { path, .. } if path.extension().is_some_and(|e| e == "woff")),
        "{err:?}"
    );
}

/// The canonical IR fixture's only asset is the fixture PNG registered by bytes; resolving
/// every HTML string of the deck leaves no placeholder behind.
#[test]
fn sample_deck_placeholders_resolve_in_every_mode() {
    let text = std::fs::read_to_string(fixture("ir/sample-deck.json")).unwrap();
    let deck: Deck = serde_json::from_str(&text).unwrap();
    let png = std::fs::read(fixture("md/images/ok.png")).unwrap();
    let mut registry = AssetRegistry::new();
    let id = registry.register_bytes(png.clone(), "image/png", "ok.png");
    assert_eq!(id, deck.assets[0].id);

    let html: String = deck
        .slides
        .iter()
        .flat_map(|s| std::iter::once(s).chain(&s.children))
        .flat_map(|s| &s.blocks)
        .flat_map(|block| match block {
            Block::Html { html } => vec![html.clone()],
            Block::Cell(cell) => cell
                .outputs
                .iter()
                .filter_map(|o| match o {
                    CellOutput::Display { html, .. } => Some(html.clone()),
                    _ => None,
                })
                .collect(),
        })
        .collect();
    assert_eq!(html.matches(&format!("asset:{id}")).count(), 2);

    for mode in all_modes() {
        let mut emitter = Emitter::new(&registry, &mode);
        let out = emitter.resolve_html(&html).unwrap();
        assert!(!out.contains("asset:"), "{mode:?}");
        let expected_url = match &mode {
            OutputMode::Standalone => data_uri("image/png", &png),
            OutputMode::Site { .. } => format!("assets/ok-{id}.png"),
            OutputMode::Serve => format!("/__presenter/deck-asset/{id}"),
        };
        assert_eq!(out.matches(&expected_url).count(), 2, "{mode:?}");
        let expected_files = usize::from(matches!(mode, OutputMode::Site { .. }));
        assert_eq!(emitter.files.len(), expected_files, "{mode:?}");
    }
}

/// Theme assets registered separately (T15) merge into the deck registry and resolve in
/// the same pass.
#[test]
fn merged_registries_resolve_together() {
    let (mut deck_assets, [png, _, svg]) = registry();
    let mut theme_assets = AssetRegistry::new();
    let css = register_css_urls(
        ".reveal{background:url(\"bg.svg\")}",
        &mut theme_assets,
        &mut |_| Some(AssetData::Bytes(SVG.to_vec())),
        false,
    )
    .unwrap();
    assert_eq!(theme_assets.len(), 1);
    deck_assets.merge(theme_assets);
    // bg.svg has the same bytes as the deck's output.svg: one asset, first hint kept.
    assert_eq!(deck_assets.len(), 3);
    assert_eq!(deck_assets.get(&svg).unwrap().file_name_hint, "output.svg");
    assert_eq!(css, format!(".reveal{{background:url(\"asset:{svg}\")}}"));

    let html = format!("<style>{css}</style><img src=\"asset:{png}\">");
    let mode = OutputMode::Serve;
    let out = Emitter::new(&deck_assets, &mode)
        .resolve_html(&html)
        .unwrap();
    assert_eq!(
        out,
        format!(
            "<style>.reveal{{background:url(\"/__presenter/deck-asset/{svg}\")}}</style>\
             <img src=\"/__presenter/deck-asset/{png}\">"
        )
    );
}

/// The file a browser fetches for `url` referenced from the file `referrer` (both relative
/// to the site root): relative URLs resolve against the referrer's own directory.
fn fetched_file(referrer: &str, url: &str) -> PathBuf {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let dir = Path::new(referrer).parent().unwrap_or(Path::new(""));
    normalize_lexically(&dir.join(path))
}

/// theme-format.md §4 (site column): the theme stylesheet is written into `assets/` and
/// linked from the document, and a browser resolves the `url()`s inside it against the
/// stylesheet's own URL. Every URL, followed from where it is written, must reach a file
/// the emitter emitted.
#[test]
fn site_mode_stylesheet_in_assets_dir_references_files_next_to_it() {
    let (mut registry, [png, gif, _]) = registry();
    let theme_css = ".reveal{background:url(img/bg.gif)} \
                     h1{background:url(\"images/ok.png?v=1#x\")} \
                     @font-face{src:url(fonts/missing.woff)}";
    let mut resolver = |reference: &str| match reference {
        "img/bg.gif" => Some(AssetData::Bytes(GIF.to_vec())),
        other => {
            let path = fixture("md").join(other);
            path.is_file().then_some(AssetData::File(path))
        }
    };
    let css = register_css_urls(theme_css, &mut registry, &mut resolver, true).unwrap();
    assert_eq!(
        registry.len(),
        3,
        "bg.gif and ok.png are already registered"
    );

    let mode = site_mode();
    let mut emitter = Emitter::new(&registry, &mode);
    let html = emitter
        .resolve_html(&format!(
            "<link rel=\"stylesheet\" href=\"assets/theme.css\">\
             <img src=\"asset:{png}\"><div style=\"background:url(asset:{gif})\"></div>"
        ))
        .unwrap();
    let sheet = emitter.resolve_css_in_assets_dir(&css).unwrap();
    assert_eq!(
        sheet,
        format!(
            ".reveal{{background:url(background-{gif}.gif)}} \
             h1{{background:url(\"ok-{png}.png?v=1#x\")}} \
             @font-face{{src:url(fonts/missing.woff)}}"
        )
    );
    let emitted: Vec<PathBuf> = emitter.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(
        emitted,
        [
            PathBuf::from(format!("dist/assets/ok-{png}.png")),
            PathBuf::from(format!("dist/assets/background-{gif}.gif")),
        ]
    );

    // Follow every URL like a browser: the document is dist/index.html, the stylesheet
    // dist/assets/theme.css.
    let urls = regex::Regex::new(r#"(?:src|href)="([^"]+)"|url\("?([^")]+)"?\)"#).unwrap();
    let mut followed = 0;
    for (referrer, text) in [
        ("dist/index.html", &html),
        ("dist/assets/theme.css", &sheet),
    ] {
        for caps in urls.captures_iter(text) {
            let url = caps.get(1).or_else(|| caps.get(2)).unwrap().as_str();
            let target = fetched_file(referrer, url);
            if url.ends_with("theme.css") {
                assert_eq!(target, Path::new("dist/assets/theme.css"));
            } else if !url.starts_with("fonts/missing") {
                assert!(emitted.contains(&target), "{referrer}: {url} -> {target:?}");
                followed += 1;
            }
        }
    }
    assert_eq!(followed, 4);

    // The same CSS inlined into the document (`<style>`) is relative to the document; those
    // URLs would miss from inside assets/ (dist/assets/assets/…).
    let inline = emitter.resolve_css(&css).unwrap();
    assert!(inline.starts_with(&format!(
        ".reveal{{background:url(assets/background-{gif}.gif)}}"
    )));
    let from_sheet = fetched_file(
        "dist/assets/theme.css",
        &format!("assets/background-{gif}.gif"),
    );
    assert!(!emitted.contains(&from_sheet));
    assert_eq!(emitter.files.len(), 2);
}

/// T15's template helpers are minijinja functions, which must be `Send + Sync + 'static`, so
/// they cannot hold an `Emitter<'_>` (it borrows the registry and the mode). They keep owned
/// state behind a mutex and create a short-lived emitter per call, carrying `files` across
/// calls; each id is still emitted exactly once.
#[test]
fn per_call_emitters_in_static_template_helpers_emit_each_id_once() {
    struct HelperState {
        registry: AssetRegistry,
        mode: OutputMode,
        files: Vec<EmittedFile>,
    }
    let (registry, [png, gif, _]) = registry();
    let state = Arc::new(Mutex::new(HelperState {
        registry,
        mode: site_mode(),
        files: Vec::new(),
    }));
    let mut env = minijinja::Environment::new();
    let helper = Arc::clone(&state);
    env.add_function(
        "asset_url",
        move |id: String| -> Result<String, minijinja::Error> {
            let fail = |message: String| {
                minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, message)
            };
            let mut guard = helper.lock().map_err(|e| fail(e.to_string()))?;
            let HelperState {
                registry,
                mode,
                files,
            } = &mut *guard;
            let mut emitter = Emitter::with_files(registry, mode, std::mem::take(files));
            let url = emitter.url_for(&id).map_err(|e| fail(e.to_string()));
            *files = emitter.files;
            url
        },
    );
    let out = env
        .render_str(
            "{{ asset_url(a) }} {{ asset_url(b) }}|{% for _ in range(3) %}{{ asset_url(a) }} {% endfor %}",
            minijinja::context! { a => png, b => gif },
        )
        .unwrap();
    let (a_url, b_url) = (
        format!("assets/ok-{png}.png"),
        format!("assets/background-{gif}.gif"),
    );
    assert_eq!(out, format!("{a_url} {b_url}|{a_url} {a_url} {a_url} "));
    {
        let state = state.lock().unwrap();
        let paths: Vec<&Path> = state.files.iter().map(|f| f.path.as_path()).collect();
        let expected = [format!("dist/{a_url}"), format!("dist/{b_url}")];
        assert_eq!(paths, [Path::new(&expected[0]), Path::new(&expected[1])]);
    }
    // Errors still name the id.
    let err = env
        .render_str("{{ asset_url('0123456789ab') }}", ())
        .unwrap_err();
    assert!(format!("{err:#}").contains("0123456789ab"), "{err:#}");
    assert_eq!(state.lock().unwrap().files.len(), 2);
}

/// A resolver like the theme's (T15): the reference joined to the theme directory, if that
/// is a file. Records what it was asked for.
fn theme_resolver<'a>(
    theme_dir: &'a Path,
    calls: &'a mut Vec<String>,
) -> impl FnMut(&str) -> Option<AssetData> + 'a {
    move |reference: &str| {
        calls.push(reference.to_string());
        let path = theme_dir.join(reference);
        path.is_file().then_some(AssetData::File(path))
    }
}

/// Percent-decoding happens before the resolver sees a path, but never changes the path's
/// structure: a reference that would decode to a root-absolute path, a scheme or another
/// directory (`url(%2F…)`, `url(..%2F…)`, `url(C%3A/…)`) reaches the resolver undecoded, as
/// the literal names a browser requests, so `Path::join` (which an absolute path replaces)
/// cannot reach a file outside the theme. Written root-absolute and scheme references stay
/// untouched. Nothing is left out silently: unresolvable with `lenient = false`, an encoded
/// reference is `MissingAsset` like any other relative one.
#[test]
fn percent_encoded_references_cannot_escape_the_relative_path_rules() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "TOP SECRET").unwrap();
    let theme_dir = dir.path().join("theme");
    std::fs::create_dir_all(theme_dir.join("img")).unwrap();
    std::fs::write(theme_dir.join("img/bg.png"), GIF).unwrap();

    let absolute = secret.to_str().unwrap().to_string();
    let encoded = absolute.replace('/', "%2F").replace('\\', "%5C");
    // What the resolver gets for these: the reference as written.
    let literal = [
        encoded.as_str(),
        "..%2Fsecret.txt",
        "img%2F..%2F..%2Fsecret.txt",
        "%2F%2Fexample.com%2Fbg.png",
        "%5C%5Cexample.com%5Cbg.png",
        "C%3A/secret.txt",
        "h%74tps://example.com/bg.png",
        // one segment for a browser, so not `img/bg.png` either
        "img%2Fbg.png",
    ];
    let css = format!(
        "a{{b:{} url(\"{encoded}\") url({absolute}) url(https://example.com/bg.png)}} \
         c{{d:url(img/b%67.png)}}",
        literal.map(|r| format!("url({r})")).join(" "),
    );

    // Lenient (vendor CSS): the literal names do not resolve and stay as written.
    let mut calls = Vec::new();
    let mut registry = AssetRegistry::new();
    let out = register_css_urls(
        &css,
        &mut registry,
        &mut theme_resolver(&theme_dir, &mut calls),
        true,
    )
    .unwrap();
    assert_eq!(calls, [&literal[..], &["img/bg.png"]].concat());
    // Every path the resolver saw is plain relative segments, so it stays below the theme.
    for call in &calls {
        let below = Path::new(call)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        assert!(below, "{call}");
    }
    assert_eq!(registry.len(), 1);
    let id = registry.iter().next().unwrap().id.clone();
    assert_eq!(
        out,
        css.replace("url(img/b%67.png)", &format!("url(asset:{id})"))
    );
    let mode = OutputMode::Standalone;
    let inlined = Emitter::new(&registry, &mode).resolve_css(&out).unwrap();
    assert!(!inlined.contains(&data_uri("text/plain", b"TOP SECRET")));
    assert!(inlined.contains(&data_uri("image/png", GIF)));

    // Strict (theme CSS): the error names the reference instead of skipping it.
    for reference in [
        encoded.as_str(),
        "fonts%2Fa.woff2",
        "..%2Fsecret.txt",
        "C%3A/x.png",
    ] {
        let mut calls = Vec::new();
        let err = register_css_urls(
            &format!("a{{b:url({reference})}}"),
            &mut AssetRegistry::new(),
            &mut theme_resolver(&theme_dir, &mut calls),
            false,
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::MissingAsset { path, span: None } if path == Path::new(reference)),
            "{reference}: {err:?}"
        );
        assert_eq!(calls, [reference]);
    }
    // A file with that literal name is found.
    std::fs::write(theme_dir.join("a%2Fb.png"), GIF).unwrap();
    let mut calls = Vec::new();
    let mut registry = AssetRegistry::new();
    let out = register_css_urls(
        "a{b:url(a%2Fb.png)}",
        &mut registry,
        &mut theme_resolver(&theme_dir, &mut calls),
        false,
    );
    assert!(out.unwrap().contains("url(asset:"));
    assert_eq!(calls, ["a%2Fb.png"]);
}

/// A query written with CSS escapes after a reference is stored without them, so standalone
/// output drops it whole instead of gluing its tail onto the `data:` URI (which would make the
/// base64 payload invalid); site output keeps an equivalent query.
#[test]
fn standalone_drops_css_escaped_queries_whole() {
    let mut resolver =
        |reference: &str| (reference == "bg.png").then(|| AssetData::Bytes(b"x".to_vec()));
    let uri = data_uri("image/png", b"x");
    for (css, standalone, site_query) in [
        (
            r"a{b:url(bg.png?v=\31)}",
            format!("a{{b:url({uri})}}"),
            "?v=1",
        ),
        (
            r"a{b:url(bg.png?x=\(1\))}",
            format!("a{{b:url({uri})}}"),
            "?x=%281%29",
        ),
        (
            r#"a{b:url("bg.png?v=\31")}"#,
            format!("a{{b:url(\"{uri}\")}}"),
            "?v=1",
        ),
        (
            r"a{b:url(bg.png?a\ b)}",
            format!("a{{b:url({uri})}}"),
            "?a%20b",
        ),
        (
            r"a{b:url('bg.png?x=(1) y')}",
            format!("a{{b:url('{uri}')}}"),
            "?x=%281%29%20y",
        ),
        (
            r"a{b:url(bg.png?v=\31#f\(1\))}",
            format!(r"a{{b:url({uri}#f\(1\))}}"),
            "?v=1",
        ),
        // An HTML numeric reference running on into the fragment: the query's `&` is encoded.
        (
            r"a{b:url(bg.png?v=\31&#1\(x\))}",
            format!(r"a{{b:url({uri}#1\(x\))}}"),
            "?v=1%26",
        ),
        (
            r"a{b:url(bg.png?&#1\31)}",
            format!(r"a{{b:url({uri}#1\31)}}"),
            "?%26",
        ),
        (
            r"a{b:url('bg.png?a&#2(')}",
            format!("a{{b:url('{uri}#2(')}}"),
            "?a%26",
        ),
        (
            r"a{b:url(bg.png?v=1&#1&#35;x)}",
            format!("a{{b:url({uri}#1&#35;x)}}"),
            "?v=1%26",
        ),
    ] {
        let mut registry = AssetRegistry::new();
        let out = register_css_urls(css, &mut registry, &mut resolver, false).unwrap();
        assert!(!out.contains(r"?v=\"), "{css}: {out}");
        let mode = OutputMode::Standalone;
        let inlined = Emitter::new(&registry, &mode).resolve_css(&out).unwrap();
        assert_eq!(inlined, standalone, "{css}");

        let mode = site_mode();
        let site = Emitter::new(&registry, &mode)
            .resolve_css_in_assets_dir(&out)
            .unwrap();
        let id = registry.iter().next().unwrap().id.clone();
        assert!(
            site.contains(&format!("bg-{id}.png{site_query}")),
            "{css}: {site}"
        );
    }
}
