//! IR contract tests (docs/spec/ir.md): the canonical fixture `tests/fixtures/ir/sample-deck.json`
//! round-trips losslessly, covers every variant, satisfies the §2 invariants, and its asset
//! id is reproducible with the registry; plus the JSON shape of each type.

use std::collections::BTreeMap;
use std::path::Path;

use presenter::assets::registry::AssetRegistry;
use presenter::ir::{
    AssetInfo, Block, CellOutput, Deck, DeckMeta, Features, NotebookCell, Slide, SourceInfo,
    SourceKind, SourceSpan,
};
use serde_json::{Value, json};

fn fixture_path(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

fn sample_text() -> String {
    std::fs::read_to_string(fixture_path("ir/sample-deck.json")).expect("fixture exists")
}

fn sample() -> Deck {
    serde_json::from_str(&sample_text()).expect("sample-deck.json deserializes into Deck")
}

/// Every slide, top-level and children, in document order.
fn all_slides(deck: &Deck) -> Vec<&Slide> {
    deck.slides
        .iter()
        .flat_map(|s| std::iter::once(s).chain(s.children.iter()))
        .collect()
}

fn all_outputs(deck: &Deck) -> Vec<&CellOutput> {
    all_slides(deck)
        .into_iter()
        .flat_map(|s| s.blocks.iter())
        .filter_map(|b| match b {
            Block::Cell(cell) => Some(cell.outputs.iter()),
            Block::Html { .. } => None,
        })
        .flatten()
        .collect()
}

#[test]
fn sample_round_trips_through_pretty_json() {
    let deck = sample();
    let pretty = serde_json::to_string_pretty(&deck).unwrap();
    let again: Deck = serde_json::from_str(&pretty).unwrap();
    assert_eq!(again, deck);
}

#[test]
fn reserialized_sample_equals_fixture_as_value() {
    let fixture: Value = serde_json::from_str(&sample_text()).unwrap();
    let reserialized = serde_json::to_value(sample()).unwrap();
    assert_eq!(reserialized, fixture);
}

#[test]
fn sample_covers_every_variant() {
    let deck = sample();
    let slides = all_slides(&deck);

    let html_blocks = slides
        .iter()
        .flat_map(|s| &s.blocks)
        .filter(|b| matches!(b, Block::Html { .. }))
        .count();
    let cells: Vec<&NotebookCell> = slides
        .iter()
        .flat_map(|s| &s.blocks)
        .filter_map(|b| match b {
            Block::Cell(c) => Some(c),
            Block::Html { .. } => None,
        })
        .collect();
    assert!(html_blocks > 0 && !cells.is_empty());

    // Exhaustive match: adding a CellOutput variant breaks this test until the fixture covers it.
    let (mut stream, mut error, mut display_some, mut display_none, mut unsupported) =
        (0, 0, 0, 0, 0);
    let mut stream_names = Vec::new();
    for output in all_outputs(&deck) {
        match output {
            CellOutput::Stream { name, .. } => {
                stream += 1;
                stream_names.push(name.as_str());
            }
            CellOutput::Error { .. } => error += 1,
            CellOutput::Display {
                execution_count: Some(_),
                ..
            } => display_some += 1,
            CellOutput::Display {
                execution_count: None,
                ..
            } => display_none += 1,
            CellOutput::Unsupported { .. } => unsupported += 1,
        }
    }
    assert!(
        stream >= 2 && error >= 1 && display_some >= 1 && display_none >= 1 && unsupported >= 1
    );
    assert_eq!(stream_names, ["stdout", "stderr"]);

    // Cell flags in both states.
    assert!(cells.iter().any(|c| c.hide_input) && cells.iter().any(|c| !c.hide_input));
    assert!(cells.iter().any(|c| c.fragment) && cells.iter().any(|c| !c.fragment));
    assert!(cells.iter().any(|c| c.id.is_some()) && cells.iter().any(|c| c.id.is_none()));

    // Meta for the title layout.
    let meta = &deck.meta;
    assert_eq!(meta.title.as_deref(), Some("Rust for data people"));
    assert!(meta.subtitle.is_some() && meta.author.is_some() && meta.date.is_some());
    assert_eq!(meta.reveal.get("transition"), Some(&json!("fade")));
    assert!(!meta.vars.is_empty());

    // Slide-level features the theme must render.
    assert_eq!(deck.slides[0].layout, "title");
    assert!(deck.slides.iter().any(|s| s.layout == "section"));
    assert!(deck.slides.iter().any(|s| s.children.len() >= 2));
    assert!(slides.iter().any(|s| s.notes.is_some()) && slides.iter().any(|s| s.notes.is_none()));
    assert!(slides.iter().any(|s| s.title.is_none()));
    assert!(slides.iter().any(|s| s.id.is_some()));
    let dark = deck.slides.iter().find(|s| s.classes == ["dark"]).unwrap();
    assert_eq!(dark.attrs["data-background-color"], "#111");
    assert!(
        slides
            .iter()
            .any(|s| s.attrs.get("data-auto-animate") == Some(&String::new()))
    );

    let all_html: String = slides
        .iter()
        .flat_map(|s| &s.blocks)
        .filter_map(|b| match b {
            Block::Html { html } => Some(html.as_str()),
            Block::Cell(_) => None,
        })
        .collect();
    assert!(all_html.contains(r#"data-math-style="inline""#));
    assert!(all_html.contains(r#"data-math-style="display""#));
    assert!(all_html.contains(r#"src="asset:32debefb8003""#));
    assert!(all_html.contains(r#"data-line-numbers="1-2|4""#));

    assert_eq!(
        deck.features,
        Features {
            math: true,
            code: true,
            notes: true,
            notebook: true
        }
    );
    assert_eq!(deck.source.kind, SourceKind::Markdown);
}

#[test]
fn sample_satisfies_invariants() {
    let deck = sample();
    assert_eq!(deck.invariant_violations(), Vec::<String>::new());
}

/// The fixture's only asset is the 1×1 PNG `tests/fixtures/md/images/ok.png` registered by
/// bytes, so a renderer test can `register_bytes(read(ok.png), "image/png", "ok.png")` and
/// resolve every `asset:` placeholder in the fixture.
#[test]
fn sample_asset_id_is_reproducible_from_ok_png() {
    let deck = sample();
    let bytes = std::fs::read(fixture_path("md/images/ok.png")).unwrap();
    let mut registry = AssetRegistry::new();
    let id = registry.register_bytes(bytes, "image/png", "ok.png");
    assert_eq!(id, "32debefb8003");
    assert_eq!(registry.infos(&deck.source.base_dir), deck.assets);
}

#[test]
fn invariant_violations_are_detected() {
    let base = sample();
    let check = |mutate: &dyn Fn(&mut Deck), needle: &str| {
        let mut deck = base.clone();
        mutate(&mut deck);
        let problems = deck.invariant_violations();
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "expected a problem containing {needle:?}, got {problems:?}"
        );
    };
    check(
        &|d| d.slides[1].children[0].children.push(Slide::new()),
        "vertical child has children",
    );
    check(&|d| d.slides[0].layout.clear(), "empty layout");
    for key in ["id", "class", "layout", "Data-X"] {
        check(
            &|d| {
                d.slides[0].attrs.insert(key.to_string(), "v".into());
            },
            "not allowed",
        );
    }
    check(
        &|d| {
            d.slides[0].span = Some(SourceSpan {
                start_line: 0,
                end_line: 1,
                cell: None,
            })
        },
        "bad span",
    );
    check(&|d| d.assets.clear(), "not in deck.assets");
    check(&|d| d.features.notes = false, "features.notes");
    check(&|d| d.features.math = false, "features.math");
    check(&|d| d.features.notebook = false, "features.notebook/code");
    check(&|d| d.features.code = false, "features.notebook/code");
}

/// The JSON example of ir.md §6 (sparse: most slide fields omitted) deserializes.
#[test]
fn spec_example_deserializes() {
    let example = r##"{
      "meta": { "title": "Rust for data people", "author": "J. Blaschke", "lang": "en",
                "theme": "./theme", "reveal": { "transition": "fade" }, "vars": { "venue": "PyData" } },
      "slides": [
        { "id": null, "layout": "title", "classes": [], "attrs": {}, "title": "Rust for data people",
          "blocks": [ { "type": "html", "html": "<h1>Rust for data people</h1>" } ],
          "notes": null, "children": [], "span": { "start_line": 8, "end_line": 8, "cell": null } },
        { "layout": "default", "classes": ["dark"], "attrs": { "data-background-color": "#111" },
          "title": "Code",
          "blocks": [ { "type": "html", "html": "<h2>Code</h2>" } ],
          "notes": "<p>Mention compile times.</p>",
          "children": [ { "layout": "default", "title": "A vertical sub-slide",
                          "blocks": [ { "type": "html", "html": "<h2>A vertical sub-slide</h2>" } ] } ] },
        { "layout": "default", "title": null,
          "blocks": [ { "type": "cell", "index": 3, "language": "python", "source": "df.describe()",
                        "execution_count": 2, "hide_input": false, "hide_output": false, "fragment": false, "tags": [],
                        "metadata": {},
                        "outputs": [ { "kind": "display", "mime": "text/html", "html": "<table>…</table>", "execution_count": 2 } ] } ] }
      ],
      "features": { "math": true, "code": true, "notes": true, "notebook": true },
      "source": { "path": "/abs/talk/slides.md", "kind": "markdown", "base_dir": "/abs/talk" },
      "assets": [ { "id": "3f9a1c2b7e0d", "mime": "image/png", "origin": "file:figures/plot.png" } ]
    }"##;
    let deck: Deck = serde_json::from_str(example).unwrap();
    assert_eq!(deck.slides.len(), 3);
    let child = &deck.slides[1].children[0];
    assert_eq!(child.id, None);
    assert!(child.classes.is_empty() && child.attrs.is_empty() && child.span.is_none());
    let Block::Cell(cell) = &deck.slides[2].blocks[0] else {
        panic!("expected a cell");
    };
    assert_eq!(cell.id, None);
    assert_eq!(deck.meta.subtitle, None);
}

#[test]
fn slide_defaults() {
    let slide = Slide::new();
    assert_eq!(slide, Slide::default());
    assert_eq!(slide.layout, "default");
    assert!(slide.blocks.is_empty() && slide.children.is_empty() && slide.attrs.is_empty());
    // Missing fields in JSON take the same defaults (layout "default", not "").
    let parsed: Slide = serde_json::from_str("{}").unwrap();
    assert_eq!(parsed, slide);
}

#[test]
fn json_shapes_match_the_spec() {
    assert_eq!(
        serde_json::to_value(Block::html("<p>x</p>".into())).unwrap(),
        json!({"type": "html", "html": "<p>x</p>"})
    );
    let cell = NotebookCell {
        index: 1,
        language: "python".into(),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(Block::Cell(cell)).unwrap(),
        json!({"type": "cell", "index": 1, "id": null, "language": "python", "source": "",
               "execution_count": null, "outputs": [], "hide_input": false, "hide_output": false,
               "fragment": false, "tags": [], "metadata": null})
    );
    let outputs = [
        CellOutput::Stream {
            name: "stdout".into(),
            html: "hi".into(),
        },
        CellOutput::Error {
            ename: "E".into(),
            evalue: "v".into(),
            html: "tb".into(),
        },
        CellOutput::Display {
            mime: "text/plain".into(),
            html: "<pre>1</pre>".into(),
            execution_count: Some(1),
        },
        CellOutput::Unsupported {
            mimes: vec!["application/x".into()],
        },
    ];
    assert_eq!(
        serde_json::to_value(outputs).unwrap(),
        json!([
            {"kind": "stream", "name": "stdout", "html": "hi"},
            {"kind": "error", "ename": "E", "evalue": "v", "html": "tb"},
            {"kind": "display", "mime": "text/plain", "html": "<pre>1</pre>", "execution_count": 1},
            {"kind": "unsupported", "mimes": ["application/x"]}
        ])
    );
    assert_eq!(
        serde_json::to_value([SourceKind::Markdown, SourceKind::Notebook]).unwrap(),
        json!(["markdown", "notebook"])
    );
    let source = SourceInfo {
        path: "/a/b.md".into(),
        kind: SourceKind::Markdown,
        base_dir: "/a".into(),
    };
    assert_eq!(
        serde_json::to_value(source).unwrap(),
        json!({"path": "/a/b.md", "kind": "markdown", "base_dir": "/a"})
    );
    let info = AssetInfo {
        id: "0123456789ab".into(),
        mime: "image/png".into(),
        origin: "inline:3".into(),
    };
    assert_eq!(
        serde_json::to_value(info).unwrap(),
        json!({"id": "0123456789ab", "mime": "image/png", "origin": "inline:3"})
    );
}

#[test]
fn slide_attrs_serialize_sorted() {
    let mut slide = Slide::new();
    for key in ["data-transition", "data-auto-animate", "aria-label"] {
        slide.attrs.insert(key.into(), String::new());
    }
    let json = serde_json::to_string(&slide.attrs).unwrap();
    assert_eq!(
        json,
        r#"{"aria-label":"","data-auto-animate":"","data-transition":""}"#
    );
}

#[test]
fn deck_meta_defaults_and_preserves_var_order() {
    let meta: DeckMeta = serde_json::from_str("{}").unwrap();
    assert_eq!(meta, DeckMeta::default());
    let meta: DeckMeta =
        serde_json::from_str(r#"{"vars": {"zeta": 1, "alpha": 2, "mid": 3}}"#).unwrap();
    let keys: Vec<&str> = meta.vars.keys().map(String::as_str).collect();
    assert_eq!(keys, ["zeta", "alpha", "mid"], "author order is kept");
}

#[test]
fn features_merge_is_fieldwise_or() {
    let mut features = Features {
        math: true,
        ..Features::default()
    };
    features.merge(Features {
        code: true,
        ..Features::default()
    });
    features.merge(Features::default());
    assert_eq!(
        features,
        Features {
            math: true,
            code: true,
            notes: false,
            notebook: false
        }
    );
    let parsed: Features = serde_json::from_str(r#"{"notes": true}"#).unwrap();
    assert!(parsed.notes && !parsed.math);
}

#[test]
fn attrs_map_type_is_btreemap() {
    // Compile-time guard for invariant 7 (stable JSON): attrs must stay a BTreeMap.
    let slide = Slide::new();
    let _: &BTreeMap<String, String> = &slide.attrs;
}
