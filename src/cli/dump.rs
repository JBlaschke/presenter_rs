//! `presenter dump` (cli.md `dump`): prints the deck IR (`docs/spec/ir.md`) or, with
//! `--context`, the template context (`docs/spec/theme-format.md` §4) as JSON on stdout.
//!
//! The JSON is pretty-printed unless `--compact`. Object keys are sorted recursively, except
//! that the context's top-level keys keep their §4 order (`deck, meta, slides, features,
//! config, reveal, theme, build`). Missing assets are warnings here (on stderr, like every
//! diagnostic), never errors. The context is the one `presenter build` would render with:
//! the theme resolved, the effective config, and `build` for the output mode of
//! `[build] standalone`. Errors carry the input's location like `build`'s ([`locate`]), and
//! a failed dump (e.g. rejected front matter) still prints the frontend's diagnostics first.
//!
//! `--default-config` takes no deck (clap rejects INPUT, `--context`, `--theme`, `--pretty`
//! and `--compact` with it) and prints [`default_config_toml`] instead: TOML, not JSON. It
//! ignores `--config`: the output is the built-in defaults, whatever `presenter.toml` says.
//! Every setting in it is commented out, so the text is a reference that changes nothing
//! when saved as a `presenter.toml` (setting some keys to their default is not the same as
//! leaving them out; the header says which).

use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;

use super::{
    DumpArgs, GlobalArgs, UsageError, input_kind, locate, print_stdout, report_diagnostics,
};
use crate::config::Config;
use crate::pipeline::{self, BuildFailure, BuildOptions};
use crate::render::{BuildInfo, context::context_json};

/// Runs `presenter dump`.
pub fn run(args: &DumpArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let input = match (&args.input, args.default_config) {
        (_, true) => {
            print_stdout(default_config_toml()?.trim_end())?;
            return Ok(ExitCode::SUCCESS);
        }
        (Some(input), false) => input,
        // clap requires INPUT unless --default-config is given.
        (None, false) => return Err(UsageError("missing INPUT".to_string()).into()),
    };
    let mut opts = BuildOptions::new(input);
    opts.format = Some(input_kind(input)?);
    opts.config_path = global.config.clone();
    opts.cli.theme = args.theme.clone();
    opts.missing_asset_is_error = false;
    let json = if args.context {
        context(&opts, global)?
    } else {
        deck(&opts, global)?
    };
    let text = if args.compact {
        serde_json::to_string(&json)?
    } else {
        serde_json::to_string_pretty(&json)?
    };
    print_stdout(&text)?;
    Ok(ExitCode::SUCCESS)
}

/// The header of [`default_config_toml`]: what the text is, and the keys whose explicit
/// presence changes a build even at their default value (`pipeline` resolves a set
/// `out_dir` against its `presenter.toml`, a `[reveal]` key outranks the theme's
/// `theme.toml` `[reveal]`, and a listed `notes` plugin loads in decks without notes).
const DEFAULT_CONFIG_HEADER: &str = "\
# Every presenter.toml setting with its built-in default, commented out: saved as it is,
# this file changes nothing. To change a setting, copy its line into presenter.toml under
# the same [table] and delete the `#`. (`presenter init` writes a starter presenter.toml.)
#
# Setting these keys, even to the default shown, is not the same as leaving them out:
# - [build] out_dir is relative to the presenter.toml that sets it, so every deck below
#   that file writes into one directory; unset, each deck writes to dist/ next to itself.
# - A [reveal] key overrides the theme's theme.toml [reveal] value for that key.
# - [reveal] plugins: listing `notes` loads the notes plugin in every deck; unset, it is
#   loaded only for decks with speaker notes.
";

/// What `presenter dump --default-config` prints: every setting of `presenter.toml` with
/// its built-in default, i.e. [`Config::default`] with `[reveal]` filled from
/// [`Config::builtin_reveal_defaults`] (the defaults the renderer applies underneath an
/// empty `[reveal]`), as TOML with every value line commented out (`#key = value`, the
/// value inline) after a header: what the text is, and which keys change a build even when
/// set to their default. A key without a default (`[deck] title`, `author`, `date`) is a
/// `# key: unset` line. Tables and keys keep the struct order.
///
/// Read back with [`Config::from_toml_str`], the text is [`Config::default`]; with the `#`
/// of the value lines removed, it is the config above. The user guide's configuration page
/// is checked against this text (`tests/guide.rs`).
pub fn default_config_toml() -> anyhow::Result<String> {
    let config = Config {
        reveal: Config::builtin_reveal_defaults(),
        ..Config::default()
    };
    let Value::Object(tables) = serde_json::to_value(&config)? else {
        anyhow::bail!("the config does not serialize to a table");
    };
    let mut out = String::from(DEFAULT_CONFIG_HEADER);
    for (table, keys) in &tables {
        let Value::Object(keys) = keys else {
            anyhow::bail!("config entry `{table}` is not a table");
        };
        out.push_str(&format!("\n[{table}]\n"));
        for (key, value) in keys {
            if value.is_null() {
                out.push_str(&format!("# {key}: unset\n"));
            } else {
                out.push_str(&format!("#{key} = {}\n", toml::Value::try_from(value)?));
            }
        }
    }
    Ok(out)
}

/// The deck IR, keys sorted.
fn deck(opts: &BuildOptions, global: &GlobalArgs) -> anyhow::Result<Value> {
    let (parsed, _, _) = match pipeline::try_parse_only(opts) {
        Ok(parsed) => parsed,
        Err(BuildFailure { error, diagnostics }) => {
            report_diagnostics(&diagnostics, global);
            return Err(locate(error, &opts.input));
        }
    };
    report_diagnostics(&parsed.diagnostics, global);
    Ok(sort_keys(serde_json::to_value(&parsed.deck)?))
}

/// The template context: top-level keys in §4 order, nested keys sorted.
fn context(opts: &BuildOptions, global: &GlobalArgs) -> anyhow::Result<Value> {
    let prepared = match pipeline::try_prepare(opts) {
        Ok(prepared) => prepared,
        Err(BuildFailure { error, diagnostics }) => {
            report_diagnostics(&diagnostics, global);
            return Err(locate(error, &opts.input));
        }
    };
    let standalone = prepared.config.build.standalone;
    let output = pipeline::output_mode(standalone, Path::new(""));
    let mut diagnostics = prepared.diagnostics.clone();
    let input = prepared.render_input(output, BuildInfo::build(standalone));
    let context = context_json(&input, &mut diagnostics).map_err(|err| locate(err, &opts.input))?;
    report_diagnostics(&diagnostics, global);
    Ok(match context {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, sort_keys(value)))
                .collect(),
        ),
        other => sort_keys(other),
    })
}

/// `value` with every object's keys in sorted order (recursively).
fn sort_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(key, value)| (key, sort_keys(value)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_keys).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `text` with the `#` of its commented-out value lines (`#key = value`) removed.
    fn uncomment(text: &str) -> String {
        text.lines()
            .map(|line| {
                let setting = line.strip_prefix('#').filter(|rest| {
                    rest.split_once(" = ").is_some_and(|(key, _)| {
                        !key.is_empty()
                            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    })
                });
                setting.unwrap_or(line)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn default_config_saved_as_it_is_changes_nothing() {
        let text = default_config_toml().unwrap();
        assert!(
            text.starts_with("# Every presenter.toml setting with its built-in default"),
            "{text}"
        );
        for line in text.lines() {
            assert!(
                line.is_empty() || line.starts_with('#') || line.starts_with('['),
                "a line that sets something: {line:?}"
            );
        }
        assert_eq!(Config::from_toml_str(&text).unwrap(), Config::default());
        // `pipeline` resolves `out_dir` against the file only when the file sets it.
        let table: toml::Table = text.parse().unwrap();
        assert!(table["build"].get("out_dir").is_none());
        assert!(table["reveal"].as_table().unwrap().is_empty());
    }

    #[test]
    fn default_config_with_its_settings_switched_on_is_the_builtin_defaults() {
        let text = default_config_toml().unwrap();
        let config = Config::from_toml_str(&uncomment(&text)).unwrap();
        let expected = Config {
            reveal: Config::builtin_reveal_defaults(),
            ..Config::default()
        };
        assert_eq!(config, expected);
        for table in [
            "deck", "build", "slides", "markdown", "reveal", "math", "notebook", "serve",
        ] {
            assert!(
                text.contains(&format!("\n[{table}]\n")),
                "no [{table}]: {text}"
            );
        }
        // Inline, so a `katex_options = { macros = … }` line under `[math]` stays valid.
        assert!(text.contains("\n#katex_options = {}\n"), "{text}");
        assert!(!text.contains("[math.katex_options]"), "{text}");
        for key in ["title", "author", "date"] {
            assert!(
                text.contains(&format!("\n# {key}: unset\n")),
                "{key}: {text}"
            );
        }
    }

    #[test]
    fn keys_are_sorted_recursively() {
        let value = json!({"b": 1, "a": {"d": [ {"z": 0, "y": 1} ], "c": null}});
        let sorted = serde_json::to_string(&sort_keys(value)).unwrap();
        assert_eq!(sorted, r#"{"a":{"c":null,"d":[{"y":1,"z":0}]},"b":1}"#);
    }
}
