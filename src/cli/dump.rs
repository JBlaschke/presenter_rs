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

/// What `presenter dump --default-config` prints: a `presenter.toml` with every setting at
/// its built-in default, i.e. [`Config::default`] with `[reveal]` filled from
/// [`Config::builtin_reveal_defaults`] (the defaults the renderer applies underneath an
/// empty `[reveal]`), serialized to TOML after a one-line comment. Keys without a default
/// (`[deck] title`, `author`, `date`) are absent. Reading the text back with
/// [`Config::from_toml_str`] gives that config again. The user guide's configuration page
/// is checked against this text (`tests/guide.rs`).
pub fn default_config_toml() -> anyhow::Result<String> {
    let config = Config {
        reveal: Config::builtin_reveal_defaults(),
        ..Config::default()
    };
    let body = toml::to_string(&config)?;
    Ok(format!(
        "# presenter.toml with every setting at its built-in default\n\n{body}"
    ))
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

    #[test]
    fn default_config_reads_back_as_the_defaults_with_the_builtin_reveal_options() {
        let text = default_config_toml().unwrap();
        assert!(
            text.starts_with("# presenter.toml with every setting"),
            "{text}"
        );
        let config = Config::from_toml_str(&text).unwrap();
        assert_eq!(config.reveal, Config::builtin_reveal_defaults());
        assert_eq!(
            Config {
                reveal: Default::default(),
                ..config
            },
            Config::default()
        );
        for table in [
            "deck", "build", "slides", "markdown", "reveal", "math", "notebook", "serve",
        ] {
            assert!(
                text.contains(&format!("\n[{table}]\n")),
                "no [{table}]: {text}"
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
