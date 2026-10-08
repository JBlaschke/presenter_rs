//! Configuration contract tests (docs/spec/config.md): defaults, the all-keys fixture,
//! unknown-key hints, discovery, precedence for every source pair, front matter overlays,
//! the reveal options JSON and plugin validation.

use std::path::{Path, PathBuf};

use presenter::config::{
    BuildConfig, CliOverrides, Config, DeckConfig, MarkdownConfig, MathConfig, MathMode,
    NotebookConfig, PluginId, ServeConfig, SlideSplit, SlidesConfig,
};
use presenter::error::Error;
use presenter::ir::DeckMeta;
use serde_json::{Map, Value, json};

const DEFAULT_REVEAL_JSON: &str = r#"{"center":true,"controls":true,"hash":true,"height":720,"progress":true,"slideNumber":false,"transition":"slide","width":1280}"#;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config/presenter.toml")
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn meta_with(reveal: Value, vars: Value) -> DeckMeta {
    DeckMeta {
        reveal: object(reveal),
        vars: object(vars),
        ..DeckMeta::default()
    }
}

fn config_message(err: Error) -> String {
    match err {
        Error::Config(message) => message,
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// Defaults and the fixture

#[test]
fn default_equals_empty_toml() {
    assert_eq!(Config::default(), toml::from_str::<Config>("").unwrap());
    assert_eq!(Config::default(), Config::from_toml_str("").unwrap());
    // Empty tables are the same as missing ones.
    let empty_tables =
        "[deck]\n[build]\n[slides]\n[markdown]\n[reveal]\n[math]\n[notebook]\n[serve]\n";
    assert_eq!(
        Config::default(),
        Config::from_toml_str(empty_tables).unwrap()
    );
}

#[test]
fn defaults_match_spec_section_2() {
    let c = Config::default();
    assert_eq!(c.deck.theme, "default");
    assert_eq!(c.deck.lang, "en");
    assert_eq!(
        (c.deck.title, c.deck.author, c.deck.date),
        (None, None, None)
    );
    assert_eq!(c.build.out_dir, PathBuf::from("dist"));
    assert!(c.build.standalone && !c.build.minify);
    assert_eq!(c.slides.split, SlideSplit::Rules);
    assert_eq!(c.slides.slide_level, 2);
    assert!(!c.slides.incremental_lists);
    assert!(!c.markdown.smart_punctuation && !c.markdown.sourcepos);
    assert!(
        c.reveal.is_empty(),
        "Config.reveal holds only explicit keys"
    );
    assert_eq!(c.math.enabled, MathMode::Auto);
    assert!(c.math.katex_options.is_empty());
    assert_eq!(
        c.notebook.mime_priority,
        [
            "text/html",
            "image/svg+xml",
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "text/markdown",
            "text/latex",
            "application/json",
            "text/plain"
        ]
    );
    assert_eq!(c.notebook.hide_input_tags, ["hide-input", "remove-input"]);
    assert_eq!(
        c.notebook.hide_output_tags,
        ["hide-output", "remove-output"]
    );
    assert_eq!(
        c.notebook.remove_cell_tags,
        ["remove-cell", "hide-cell", "skip"]
    );
    assert!(!c.notebook.split_markdown_cells && c.notebook.show_prompts);
    assert_eq!(c.serve.host, "127.0.0.1");
    assert_eq!(c.serve.port, 3000);
    assert!(!c.serve.open);
}

#[test]
fn builtin_reveal_defaults_match_spec_section_2() {
    assert_eq!(
        Value::Object(Config::builtin_reveal_defaults()),
        json!({
            "hash": true, "slideNumber": false, "transition": "slide", "controls": true,
            "progress": true, "center": true, "width": 1280, "height": 720,
            "plugins": ["highlight", "notes", "zoom", "search"]
        })
    );
    assert_eq!(
        Config::default().effective_reveal(),
        Config::builtin_reveal_defaults()
    );
}

#[test]
fn fixture_sets_every_key_to_a_non_default_value() {
    let c = Config::load(&fixture()).unwrap();
    let d = Config::default();

    let DeckConfig {
        theme,
        lang,
        title,
        author,
        date,
    } = &c.deck;
    assert_eq!(theme, "./my-theme");
    assert_ne!(theme, &d.deck.theme);
    assert_ne!(lang, &d.deck.lang);
    assert!(title.is_some() && author.is_some() && date.is_some());

    let BuildConfig {
        out_dir,
        standalone,
        minify,
    } = &c.build;
    assert_eq!(out_dir, &PathBuf::from("public"));
    assert_ne!(standalone, &d.build.standalone);
    assert_ne!(minify, &d.build.minify);

    let SlidesConfig {
        split,
        slide_level,
        incremental_lists,
    } = &c.slides;
    assert_eq!(*split, SlideSplit::Headings);
    assert_eq!(*slide_level, 3);
    assert_ne!(incremental_lists, &d.slides.incremental_lists);

    let MarkdownConfig {
        smart_punctuation,
        sourcepos,
    } = &c.markdown;
    assert!(*smart_punctuation && *sourcepos);

    // Every built-in reveal key is overridden with a different value.
    let builtin = Config::builtin_reveal_defaults();
    for (key, value) in &builtin {
        let configured = c
            .reveal
            .get(key)
            .unwrap_or_else(|| panic!("fixture lacks reveal.{key}"));
        assert_ne!(configured, value, "reveal.{key}");
    }
    assert_eq!(c.reveal["slideNumber"], json!("c/t"));
    assert_eq!(c.reveal["width"], json!(1920));

    let MathConfig {
        enabled,
        katex_options,
    } = &c.math;
    assert_eq!(*enabled, MathMode::Always);
    assert_eq!(
        Value::Object(katex_options.clone()),
        json!({"macros": {"\\R": "\\mathbb{R}"}})
    );

    let NotebookConfig {
        mime_priority,
        hide_input_tags,
        hide_output_tags,
        remove_cell_tags,
        split_markdown_cells,
        show_prompts,
    } = &c.notebook;
    assert_eq!(mime_priority, &["image/png", "text/plain"]);
    assert_ne!(hide_input_tags, &d.notebook.hide_input_tags);
    assert_ne!(hide_output_tags, &d.notebook.hide_output_tags);
    assert_ne!(remove_cell_tags, &d.notebook.remove_cell_tags);
    assert!(*split_markdown_cells && !*show_prompts);

    let ServeConfig { host, port, open } = &c.serve;
    assert_eq!((host.as_str(), *port, *open), ("0.0.0.0", 8080, true));

    assert_eq!(c.enabled_plugins(), [PluginId::Notes, PluginId::Zoom]);
    assert_eq!(
        c.reveal_options_json(),
        r#"{"center":false,"controls":false,"hash":false,"height":1080,"progress":false,"slideNumber":"c/t","transition":"fade","width":1920}"#
    );
}

// ---------------------------------------------------------------------------------------
// Errors and "did you mean"

#[test]
fn unknown_table_suggests_build() {
    let message = config_message(Config::from_toml_str("[buidl]\nout_dir = \"x\"\n").unwrap_err());
    assert!(message.contains("did you mean 'build'"), "{message}");
    assert!(message.contains("line 1"), "{message}");

    // The plain `From<toml::de::Error>` conversion carries the hint as well.
    let err: Error = toml::from_str::<Config>("[buidl]\n").unwrap_err().into();
    assert!(err.to_string().contains("did you mean 'build'"), "{err}");
}

#[test]
fn load_error_names_the_file_and_hints() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("presenter.toml");
    std::fs::write(&path, "[deck]\nlang = \"en\"\n\n[buidl]\n").unwrap();
    let message = config_message(Config::load(&path).unwrap_err());
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("line 4"), "{message}");
    assert!(message.contains("did you mean 'build'"), "{message}");
}

#[test]
fn unknown_nested_keys_and_values_get_hints() {
    let cases = [
        ("[slides]\nsplt = \"rules\"\n", "did you mean 'split'"),
        ("[serve]\nprot = 1\n", "did you mean 'port'"),
        (
            "[notebook]\nshow_prompt = true\n",
            "did you mean 'show_prompts'",
        ),
        ("[slides]\nsplit = \"heading\"\n", "did you mean 'headings'"),
        ("[math]\nenabled = \"alway\"\n", "did you mean 'always'"),
    ];
    for (toml, hint) in cases {
        let message = config_message(Config::from_toml_str(toml).unwrap_err());
        assert!(message.contains(hint), "{toml:?}: {message}");
    }
}

#[test]
fn far_unknown_keys_fail_without_a_hint() {
    let message = config_message(Config::from_toml_str("[zzzzzzzz]\n").unwrap_err());
    assert!(message.contains("unknown field `zzzzzzzz`"), "{message}");
    assert!(!message.contains("did you mean"), "{message}");
}

#[test]
fn type_errors_are_config_errors() {
    let message = config_message(Config::from_toml_str("[serve]\nport = \"x\"\n").unwrap_err());
    assert!(message.contains("line 2"), "{message}");
    assert!(message.contains("expected u16"), "{message}");
    let message = config_message(Config::from_toml_str("[serve]\nport = 70000\n").unwrap_err());
    assert!(message.contains("line 2, column 8"), "{message}");
    assert!(
        message.contains("invalid value: integer `70000`, expected u16"),
        "{message}"
    );
}

#[test]
fn load_missing_file_is_a_config_error_naming_the_path() {
    let message =
        config_message(Config::load(Path::new("/no/such/dir/presenter.toml")).unwrap_err());
    assert!(message.contains("/no/such/dir/presenter.toml"), "{message}");
}

// ---------------------------------------------------------------------------------------
// Discovery

#[test]
fn discover_walks_up_and_stops_at_the_first_file() {
    let root = tempfile::tempdir().unwrap();
    let top = root.path().join("project");
    let nested = top.join("talks/2026/deck");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(top.join("presenter.toml"), "[serve]\nport = 4000\n").unwrap();

    let (path, config) = Config::discover(&nested).unwrap().expect("found");
    assert_eq!(path, top.join("presenter.toml"));
    assert_eq!(config.serve.port, 4000);

    // A nearer file wins; the walk stops there even though the outer one exists.
    let mid = top.join("talks");
    std::fs::write(mid.join("presenter.toml"), "[serve]\nport = 5000\n").unwrap();
    let (path, config) = Config::discover(&nested).unwrap().expect("found");
    assert_eq!(path, mid.join("presenter.toml"));
    assert_eq!(config.serve.port, 5000);

    // `..` segments are resolved lexically before walking up.
    let dotted = nested.join("../../2026/deck/.");
    let (path, _) = Config::discover(&dotted).unwrap().expect("found");
    assert_eq!(path, mid.join("presenter.toml"));
}

#[test]
fn discover_reports_errors_in_the_found_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("presenter.toml"), "[buidl]\n").unwrap();
    let message = config_message(Config::discover(root.path()).unwrap_err());
    assert!(message.contains("did you mean 'build'"), "{message}");
}

#[test]
fn discover_without_a_file_is_none_or_outside() {
    let root = tempfile::tempdir().unwrap();
    let found = Config::discover(root.path()).unwrap();
    // No presenter.toml in the temp dir; anything found must come from above it.
    if let Some((path, _)) = found {
        assert!(!path.starts_with(root.path()), "{}", path.display());
    }
}

/// Guard: fixtures for frontends and the pipeline must not see a project config, so the
/// all-non-default config fixture is kept out of their ancestor directories.
#[test]
fn fixture_inputs_do_not_discover_a_repo_config() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in ["tests/fixtures/md", "tests/fixtures/ir", "tests/fixtures"] {
        if let Some((path, _)) = Config::discover(&repo.join(dir)).unwrap() {
            assert!(
                !path.starts_with(repo),
                "{dir} discovers {}; pipeline tests on fixtures would use it",
                path.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Precedence: built-ins < theme [reveal] < presenter.toml < front matter < CLI

/// Builds a config with `reveal.transition` set by the sources whose bit is set in `mask`
/// (bit 0 theme, 1 file, 2 front matter, 3 CLI), applying them in pipeline order or with
/// front matter before the theme (what a frontend does on its clone).
fn config_with_sources(mask: u8, front_matter_first: bool) -> Config {
    let set = |bit: u8| mask & (1 << bit) != 0;
    let mut config = if set(1) {
        Config::from_toml_str("[reveal]\ntransition = \"file\"\n").unwrap()
    } else {
        Config::default()
    };
    let theme = object(json!({"transition": "theme"}));
    let front = meta_with(json!({"transition": "front matter"}), json!({}));
    if front_matter_first && set(2) {
        config.merge_front_matter(&front).unwrap();
    }
    if set(0) {
        config.merge_theme_reveal(&theme);
    }
    if !front_matter_first && set(2) {
        config.merge_front_matter(&front).unwrap();
    }
    if set(3) {
        config.apply_cli(&CliOverrides::default());
        config
            .apply_cli_reveal(&object(json!({"transition": "cli"})))
            .unwrap();
    }
    config
}

#[test]
fn reveal_transition_precedence_covers_every_pair() {
    let names = ["theme", "file", "front matter", "cli"];
    for front_matter_first in [false, true] {
        for mask in 0u8..16 {
            let config = config_with_sources(mask, front_matter_first);
            let expected = (0..4u8)
                .rev()
                .find(|bit| mask & (1 << bit) != 0)
                .map_or("slide", |bit| names[bit as usize]);
            assert_eq!(
                config.effective_reveal()["transition"],
                json!(expected),
                "mask {mask:04b}, front matter first: {front_matter_first}"
            );
            assert!(
                config
                    .reveal_options_json()
                    .contains(&format!(r#""transition":"{expected}""#))
            );
        }
    }
}

#[test]
fn named_precedence_pairs_for_transition() {
    let get = |mask| config_with_sources(mask, false).effective_reveal()["transition"].clone();
    assert_eq!(get(0b0000), json!("slide"), "built-in");
    assert_eq!(get(0b0001), json!("theme"), "built-in < theme");
    assert_eq!(get(0b0011), json!("file"), "theme < file");
    assert_eq!(get(0b0101), json!("front matter"), "theme < front matter");
    assert_eq!(get(0b1001), json!("cli"), "theme < CLI");
    assert_eq!(get(0b0110), json!("front matter"), "file < front matter");
    assert_eq!(get(0b1010), json!("cli"), "file < CLI");
    assert_eq!(get(0b1100), json!("cli"), "front matter < CLI");
}

#[test]
fn theme_reveal_only_fills_absent_keys() {
    let mut config =
        Config::from_toml_str("[reveal]\ntransition = \"file\"\nwidth = 1600\n").unwrap();
    config.merge_theme_reveal(&object(json!({
        "transition": "theme", "slideNumber": "c/t", "width": 800
    })));
    assert_eq!(
        Value::Object(config.reveal.clone()),
        json!({"transition": "file", "width": 1600, "slideNumber": "c/t"})
    );
}

#[test]
fn cli_overrides_file_values() {
    let mut config = Config::load(&fixture()).unwrap();
    // Exactly the six fields of config.md §4, so a struct literal written from the spec
    // compiles.
    config.apply_cli(&CliOverrides {
        theme: Some("cli-theme".into()),
        out: Some(PathBuf::from("ignored.html")),
        standalone: Some(true),
        port: Some(0),
        host: Some("localhost".into()),
        open: Some(false),
    });
    assert_eq!(config.deck.theme, "cli-theme");
    assert!(config.build.standalone);
    assert_eq!(
        config.build.out_dir,
        PathBuf::from("public"),
        "-o is not build.out_dir"
    );
    assert_eq!(
        (config.serve.host.as_str(), config.serve.port),
        ("localhost", 0)
    );
    assert!(!config.serve.open);

    // Unset overrides change nothing.
    let before = Config::load(&fixture()).unwrap();
    let mut after = before.clone();
    after.apply_cli(&CliOverrides::default());
    assert_eq!(after, before);
}

// ---------------------------------------------------------------------------------------
// Front matter

#[test]
fn front_matter_slides_overlay_only_present_keys() {
    let mut config = Config::default();
    config.slides.slide_level = 4;
    config.slides.incremental_lists = true;
    let meta = meta_with(json!({}), json!({"slides": {"split": "headings"}}));
    config.merge_front_matter(&meta).unwrap();
    assert_eq!(config.slides.split, SlideSplit::Headings);
    assert_eq!(config.slides.slide_level, 4);
    assert!(config.slides.incremental_lists);
    assert!(meta.vars.contains_key("slides"), "the map stays in vars");
}

#[test]
fn front_matter_overlays_every_table() {
    let mut config = Config::default();
    let meta = meta_with(
        json!({"transition": "zoom", "width": 1600}),
        json!({
            "venue": "PyData",
            "markdown": {"sourcepos": true},
            "math": {"enabled": "never", "katex_options": {"strict": false}},
            "notebook": {"show_prompts": false, "remove_cell_tags": ["gone"]}
        }),
    );
    config.merge_front_matter(&meta).unwrap();
    assert!(config.markdown.sourcepos && !config.markdown.smart_punctuation);
    assert_eq!(config.math.enabled, MathMode::Never);
    assert_eq!(
        Value::Object(config.math.katex_options.clone()),
        json!({"strict": false})
    );
    assert!(!config.notebook.show_prompts);
    assert_eq!(config.notebook.remove_cell_tags, ["gone"]);
    assert_eq!(
        config.notebook.hide_input_tags,
        NotebookConfig::default().hide_input_tags
    );
    assert_eq!(
        Value::Object(config.reveal.clone()),
        json!({"transition": "zoom", "width": 1600})
    );
}

#[test]
fn front_matter_merge_is_idempotent() {
    let meta = meta_with(
        json!({"transition": "zoom"}),
        json!({"slides": {"split": "headings", "slide_level": 3}, "math": {"enabled": "always"}}),
    );
    let mut once = Config::load(&fixture()).unwrap();
    once.merge_front_matter(&meta).unwrap();
    let mut twice = once.clone();
    twice.merge_front_matter(&meta).unwrap();
    assert_eq!(once, twice);
}

#[test]
fn front_matter_unknown_key_is_an_error_with_hint_and_no_change() {
    let mut config = Config::default();
    let meta = meta_with(
        json!({"transition": "zoom"}),
        json!({"slides": {"splt": "headings"}}),
    );
    let err = config.merge_front_matter(&meta).unwrap_err();
    let Error::FrontMatter(message) = &err else {
        panic!("expected Error::FrontMatter, got {err:?}");
    };
    assert!(message.contains("slides"), "{message}");
    assert!(message.contains("did you mean 'split'"), "{message}");
    assert_eq!(config, Config::default(), "a failed merge changes nothing");

    let bad_type = meta_with(
        json!({}),
        json!({"serve": {}, "slides": {"slide_level": "two"}}),
    );
    assert!(matches!(
        Config::default().merge_front_matter(&bad_type),
        Err(Error::FrontMatter(_))
    ));
}

#[test]
fn front_matter_non_object_tables_are_plain_vars() {
    let mut config = Config::default();
    let meta = meta_with(
        json!({}),
        json!({"slides": 3, "math": "yes", "serve": {"port": 1}}),
    );
    config.merge_front_matter(&meta).unwrap();
    assert_eq!(
        config,
        Config::default(),
        "only slides/markdown/math/notebook objects apply"
    );
}

// ---------------------------------------------------------------------------------------
// Reveal options JSON and plugins

#[test]
fn reveal_options_json_of_defaults_is_exact() {
    assert_eq!(Config::default().reveal_options_json(), DEFAULT_REVEAL_JSON);
}

#[test]
fn reveal_options_json_sorts_strips_plugins_and_integral_floats() {
    let mut config = Config::default();
    let meta = meta_with(
        json!({
            "width": 1600.0, "margin": 0.04, "plugins": ["notes"],
            "keyboard": {"39": "next", "13": null}, "autoSlide": 5000
        }),
        json!({}),
    );
    config.merge_front_matter(&meta).unwrap();
    assert_eq!(
        config.reveal_options_json(),
        r#"{"autoSlide":5000,"center":true,"controls":true,"hash":true,"height":720,"keyboard":{"13":null,"39":"next"},"margin":0.04,"progress":true,"slideNumber":false,"transition":"slide","width":1600}"#
    );
}

#[test]
fn enabled_plugins_default_order() {
    assert_eq!(
        Config::default().enabled_plugins(),
        [
            PluginId::Highlight,
            PluginId::Notes,
            PluginId::Zoom,
            PluginId::Search
        ]
    );
}

#[test]
fn enabled_plugins_ignore_math_and_duplicates() {
    let config = Config::from_toml_str(
        "[reveal]\nplugins = [\"notes\", \"math\", \"highlight\", \"notes\"]\n",
    )
    .unwrap();
    assert_eq!(
        config.enabled_plugins(),
        [PluginId::Notes, PluginId::Highlight]
    );
    config.validate_plugins().unwrap();
    let none = Config::from_toml_str("[reveal]\nplugins = []\n").unwrap();
    assert!(none.enabled_plugins().is_empty());
}

#[test]
fn presenter_toml_rejects_unknown_and_malformed_plugins() {
    let message = config_message(
        Config::from_toml_str("[reveal]\nplugins = [\"hightlight\"]\n").unwrap_err(),
    );
    assert!(
        message.contains("unknown reveal plugin 'hightlight' (did you mean 'highlight'?)"),
        "{message}"
    );
    for (toml, needle) in [
        (
            "[reveal]\nplugins = [\"markdown\"]\n",
            "unknown reveal plugin 'markdown'",
        ),
        ("[reveal]\nplugins = \"notes\"\n", "must be a list"),
        (
            "[reveal]\nplugins = [1]\n",
            "entries must be strings, got 1",
        ),
    ] {
        let message = config_message(Config::from_toml_str(toml).unwrap_err());
        assert!(message.contains(needle), "{toml:?}: {message}");
    }

    // Config::load names the file.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("presenter.toml");
    std::fs::write(&path, "[reveal]\nplugins = [\"zoom\", \"serach\"]\n").unwrap();
    let message = config_message(Config::load(&path).unwrap_err());
    assert!(
        message.starts_with(&format!("{}: ", path.display())),
        "{message}"
    );
    assert!(message.contains("did you mean 'search'"), "{message}");
}

#[test]
fn front_matter_and_cli_reject_unknown_plugins_and_change_nothing() {
    let mut config = Config::default();
    let meta = meta_with(
        json!({"plugins": ["notes", "zom"], "transition": "fade"}),
        json!({}),
    );
    let err = config.merge_front_matter(&meta).unwrap_err();
    let Error::FrontMatter(message) = &err else {
        panic!("expected Error::FrontMatter, got {err:?}");
    };
    assert!(message.contains("did you mean 'zoom'"), "{message}");
    assert_eq!(config, Config::default());

    let bad = object(json!({"plugins": "zoom", "transition": "fade"}));
    let message = config_message(config.apply_cli_reveal(&bad).unwrap_err());
    assert!(message.contains("must be a list"), "{message}");
    assert_eq!(config, Config::default());

    config
        .apply_cli_reveal(&object(json!({"plugins": ["zoom", "math"]})))
        .unwrap();
    assert_eq!(config.enabled_plugins(), [PluginId::Zoom]);
}

#[test]
fn enabled_plugins_skip_invalid_theme_entries_and_validate_reports_them() {
    // A theme's [reveal] is merged without validation; enabled_plugins() stays usable.
    let mut config = Config::default();
    config.merge_theme_reveal(&object(json!({"plugins": ["zoom", "bogus", 3, "notes"]})));
    assert_eq!(config.enabled_plugins(), [PluginId::Zoom, PluginId::Notes]);
    let message = config_message(config.validate_plugins().unwrap_err());
    assert!(
        message.contains("unknown reveal plugin 'bogus'"),
        "{message}"
    );
    assert!(
        message.contains("entries must be strings, got 3"),
        "{message}"
    );

    let mut not_list = Config::default();
    not_list.merge_theme_reveal(&object(json!({"plugins": "zoom"})));
    assert!(not_list.enabled_plugins().is_empty());
    assert!(matches!(not_list.validate_plugins(), Err(Error::Config(_))));

    // Without `plugins` anywhere but the built-ins, all four are enabled and valid.
    Config::default().validate_plugins().unwrap();
}

// ---------------------------------------------------------------------------------------
// TOML dates and other scalars

#[test]
fn deck_strings_accept_toml_dates_and_scalars() {
    let config =
        Config::from_toml_str("[deck]\ndate = 2026-10-08\ntitle = 42\nauthor = true\n").unwrap();
    assert_eq!(config.deck.date.as_deref(), Some("2026-10-08"));
    assert_eq!(config.deck.title.as_deref(), Some("42"));
    assert_eq!(config.deck.author.as_deref(), Some("true"));

    for (toml, text) in [
        ("date = 1979-05-27T07:32:00Z", "1979-05-27T07:32:00Z"),
        (
            "date = 1979-05-27T00:32:00-07:00",
            "1979-05-27T00:32:00-07:00",
        ),
        ("date = 07:32:00", "07:32:00"),
        ("date = \"Spring 2026\"", "Spring 2026"),
        ("date = 2.5", "2.5"),
    ] {
        let config = Config::from_toml_str(&format!("[deck]\n{toml}\n")).unwrap();
        assert_eq!(config.deck.date.as_deref(), Some(text), "{toml}");
    }

    for (toml, kind) in [
        ("title = [1]", "an array"),
        ("author = { a = 1 }", "a table"),
    ] {
        let message =
            config_message(Config::from_toml_str(&format!("[deck]\n{toml}\n")).unwrap_err());
        assert!(
            message.contains(&format!(
                "expected a string, number, boolean or date, got {kind}"
            )),
            "{toml}: {message}"
        );
        assert!(message.contains("line 2"), "{toml}: {message}");
    }
}

#[test]
fn toml_dates_in_free_form_maps_become_strings() {
    let config = Config::from_toml_str(
        "[reveal]\nreleased = 2026-10-08\nnested = { at = [07:32:00] }\n\
         [math.katex_options]\nwhen = 1979-05-27T07:32:00Z\n",
    )
    .unwrap();
    assert_eq!(config.reveal["released"], json!("2026-10-08"));
    assert_eq!(config.reveal["nested"], json!({"at": ["07:32:00"]}));
    assert_eq!(
        config.math.katex_options["when"],
        json!("1979-05-27T07:32:00Z")
    );
    let options = config.reveal_options_json();
    assert!(options.contains(r#""released":"2026-10-08""#), "{options}");
    assert!(!options.contains("toml_private"), "{options}");

    // One-entry string maps that are not dates stay maps, from TOML and from front matter.
    let macros = json!({"macros": {"\\R": "\\mathbb{R}"}, "d": {"x": "2026-10-08"}});
    let mut config = Config::from_toml_str(
        "[math.katex_options]\nmacros = { \"\\\\R\" = \"\\\\mathbb{R}\" }\nd = { x = \"2026-10-08\" }\n",
    )
    .unwrap();
    assert_eq!(Value::Object(config.math.katex_options.clone()), macros);
    config.math.katex_options.clear();
    let meta = meta_with(json!({}), json!({"math": {"katex_options": macros}}));
    config.merge_front_matter(&meta).unwrap();
    assert_eq!(Value::Object(config.math.katex_options.clone()), macros);
}

#[test]
fn config_serializes_for_the_template_context() {
    let value = serde_json::to_value(Config::default()).unwrap();
    assert_eq!(value["slides"]["split"], json!("rules"));
    assert_eq!(value["math"]["enabled"], json!("auto"));
    assert_eq!(value["build"]["out_dir"], json!("dist"));
    assert_eq!(value["reveal"], json!({}));
    // And back, through serde_json as the front matter overlay does.
    let back: Config = serde_json::from_value(value).unwrap();
    assert_eq!(back, Config::default());
}
