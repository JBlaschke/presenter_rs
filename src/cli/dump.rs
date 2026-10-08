//! `presenter dump` (cli.md `dump`): prints the deck IR (`docs/spec/ir.md`) or, with
//! `--context`, the template context (`docs/spec/theme-format.md` §4) as JSON on stdout.
//!
//! The JSON is pretty-printed unless `--compact`. Object keys are sorted recursively, except
//! that the context's top-level keys keep their §4 order (`deck, meta, slides, features,
//! config, reveal, theme, build`). Missing assets are warnings here (on stderr, like every
//! diagnostic), never errors. The context is the one `presenter build` would render with:
//! the theme resolved, the effective config, and `build` for the output mode of
//! `[build] standalone`.

use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;

use super::{DumpArgs, GlobalArgs, input_format, print_stdout, report_diagnostics};
use crate::pipeline::{self, BuildOptions};
use crate::render::{BuildInfo, context::context_json};

/// Runs `presenter dump`.
pub fn run(args: &DumpArgs, global: &GlobalArgs) -> anyhow::Result<ExitCode> {
    let mut opts = BuildOptions::new(&args.input);
    opts.format = Some(input_format(&args.input, None)?);
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

/// The deck IR, keys sorted.
fn deck(opts: &BuildOptions, global: &GlobalArgs) -> anyhow::Result<Value> {
    let (parsed, _, _) = pipeline::parse_only(opts)?;
    report_diagnostics(&parsed.diagnostics, global);
    Ok(sort_keys(serde_json::to_value(&parsed.deck)?))
}

/// The template context: top-level keys in §4 order, nested keys sorted.
fn context(opts: &BuildOptions, global: &GlobalArgs) -> anyhow::Result<Value> {
    let prepared = pipeline::prepare(opts)?;
    let standalone = prepared.config.build.standalone;
    let output = pipeline::output_mode(standalone, Path::new(""));
    let mut diagnostics = prepared.diagnostics.clone();
    let input = prepared.render_input(output, BuildInfo::build(standalone));
    let context = context_json(&input, &mut diagnostics)?;
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
    fn keys_are_sorted_recursively() {
        let value = json!({"b": 1, "a": {"d": [ {"z": 0, "y": 1} ], "c": null}});
        let sorted = serde_json::to_string(&sort_keys(value)).unwrap();
        assert_eq!(sorted, r#"{"a":{"c":null,"d":[{"y":1,"z":0}]},"b":1}"#);
    }
}
