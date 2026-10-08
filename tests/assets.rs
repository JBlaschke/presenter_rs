//! Asset registry and emitter (T14; `docs/spec/ir.md` §3, `docs/spec/live-reload.md` §1,
//! `docs/spec/theme-format.md` §4–§5, `docs/spec/vendored-assets.md` §2): placeholders become
//! `data:` URIs (standalone), hashed files (site) or dev-server routes (serve), and CSS
//! `url()` references become placeholders.

use std::path::{Path, PathBuf};

use presenter::Error;
use presenter::assets::emit::{
    EmittedFile, Emitter, OutputMode, data_uri, hashed_file_name, register_css_urls,
};
use presenter::assets::registry::{AssetData, AssetRegistry};
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
