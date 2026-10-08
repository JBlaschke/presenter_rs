# T13 — Theme discovery, manifest, inheritance chain, template environment

| | |
|---|---|
| Milestone | M1 |
| Size | M |
| Depends on | T01 |
| Parallel-safe with | T02, T10, T11, T12, T14, T30 |
| Files you own | `src/theme/mod.rs`, `src/theme/manifest.rs`, `src/theme/resolve.rs`, `src/theme/templates.rs`, `tests/theme_resolve.rs`, `tests/fixtures/themes/child-theme/**`, `tests/fixtures/themes/broken-cycle/**` |

## Goal
Load a theme by name or path, follow `extends` to build the chain, read files from disk or
the embedded tree uniformly, and produce a minijinja `Environment` whose loader searches
the chain.

## Read first
- `docs/spec/theme-format.md` §1–§3, §5 (entries concatenation)
- `docs/spec/vendored-assets.md` §4 (embedded access)

## API to implement
```rust
// manifest.rs
#[derive(Deserialize, Default)] #[serde(default, deny_unknown_fields)]
pub struct ThemeManifest { pub name: String, pub extends: Option<String>, pub description: Option<String>,
    pub styles: StylesSection, pub scripts: ScriptsSection, pub reveal: Map<String, Value>,
    pub layouts: BTreeMap<String, String>, pub vars: Map<String, Value> }
// StylesSection { entries: Vec<String>, reveal_theme: Option<String>, highlight_style: Option<String> }
// ScriptsSection { entries: Vec<String> }

// mod.rs
pub enum ThemeSource { Embedded { prefix: String /* "themes/default/" */ }, Disk { dir: PathBuf } }
pub struct ThemeDir { pub name: String, pub source: ThemeSource, pub manifest: ThemeManifest }
pub struct Theme { pub chain: Vec<ThemeDir> }    // child first
pub struct ThemeFile { pub theme: String, pub rel_path: String, pub bytes: Vec<u8> }
impl Theme {
    pub fn name(&self) -> &str;
    pub fn reveal_theme(&self) -> String;        // child override, else "white"
    pub fn highlight_style(&self) -> String;     // else "monokai"
    pub fn reveal_defaults(&self) -> Map<String, Value>;   // deep-merged root→child
    pub fn vars(&self) -> Map<String, Value>;
    pub fn style_entries(&self) -> Result<Vec<ThemeFile>>;  // parent-first concatenation order
    pub fn script_entries(&self) -> Result<Vec<ThemeFile>>;
    pub fn read_template(&self, name: &str) -> Result<Option<(String /* theme */, String)>>;  // supports "@name/…"
    pub fn has_template(&self, name: &str) -> bool;
    pub fn layouts(&self) -> Vec<String>;        // union of templates/layouts/*.html stems across chain
    pub fn read_asset(&self, rel: &str) -> Result<Option<Vec<u8>>>;   // assets/<rel> across chain
    pub fn watch_dirs(&self) -> Vec<PathBuf>;    // disk theme dirs (recursive watch)
    pub fn environment(&self, debug: bool) -> Result<minijinja::Environment<'static>>;
}
// resolve.rs
pub fn resolve(spec: &str, search_roots: &[&Path]) -> Result<Theme>;   // theme-format.md §3
pub fn builtin_names() -> Vec<String>;                                 // from vendor::list("themes/")
```

## Steps
1. `resolve`: try `spec` as a directory containing `theme.toml` relative to each
   `search_roots` entry in order (config file dir, source dir, CWD) and as an absolute path;
   else builtin by name. Follow `extends` (a path is relative to the *extending* theme's
   directory; a name is builtin). Detect cycles by name+path; cap depth at 8.
2. Uniform file access: `ThemeDir::read(&self, rel: &str) -> Result<Option<Vec<u8>>>` and
   `ThemeDir::list(&self, rel_dir: &str) -> Vec<String>` over disk (`std::fs`) or
   `vendor::get/list` with the `Embedded.prefix`.
3. `environment`: `Environment::new()`, `set_auto_escape_callback(|n| n.ends_with(".html"))`,
   `set_debug(debug)`, `set_undefined_behavior(UndefinedBehavior::Strict)` **only** in debug
   (templates may reference optional meta fields; in strict mode use `meta.subtitle is defined`
   — decide and document; recommended: `Lenient` always, `Strict` behind `--strict-templates` later),
   `set_loader` with a closure capturing `Arc<Theme>` that calls `read_template`.
   Helpers/filters are **not** registered here (T15); provide `pub fn with_helpers(env, f)`
   hook or simply return the env for T15 to extend.
4. Error mapping: `From<minijinja::Error>` in `error.rs` (additive) → `Error::Template { template: e.name(), line: e.line(), message: e.to_string() }`. In debug mode also include `e.display_debug_info()` in the message.

## Acceptance criteria
- Fixture `child-theme` (`extends = "default"`, overrides `layouts/title.html` and adds
  `styles/child.css`): `chain` names `["child-theme", "default"]`; `read_template("layouts/title.html")`
  comes from `child-theme`; `read_template("layouts/default.html")` from `default`;
  `read_template("@default/layouts/title.html")` from `default`; `style_entries()` order is
  `[default/styles/theme.css, child-theme/styles/child.css]`; `reveal_defaults` merges child over root.
- `resolve("default", &[])` works with no disk access; `resolve("nope", …)` →
  `Error::Theme` whose message lists the search roots.
- `broken-cycle` (A extends B extends A) → `Error::Theme` mentioning "cycle".
- A missing required root template (`layouts/default.html`) → `Error::Theme` listing the missing files.
- `environment(true)` renders `{% include "layouts/title.html" %}` from the child fixture given a stub context.

## Out of scope
Helpers (`style()`, …) and CSS compilation (T15/T42); `theme` CLI (T41).

## Hints (verified)
- minijinja 2.24: `env.set_loader(move |name: &str| -> Result<Option<String>, minijinja::Error> {...})`
  requires the `loader` feature (enabled in T00). `minijinja::Error::line()` and `name()` exist;
  `Environment<'static>` works with owned template sources returned from the loader.
- `rust-embed` in debug mode reads from disk relative to `CARGO_MANIFEST_DIR`; tests run with
  CWD at the crate root in Cargo, so both modes agree.
