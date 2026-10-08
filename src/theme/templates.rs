//! minijinja environment whose loader searches the theme chain
//! (`docs/spec/theme-format.md` §3–§4).
//!
//! [`environment`] returns a bare environment: auto-escaping for `.html` templates, the
//! chain loader and the debug flag. Helpers and filters (`style()`, `attrs`, …) are added
//! by the renderer (T15) on the returned value.
//!
//! **Undefined values are lenient** (minijinja's `UndefinedBehavior::Lenient`), in build
//! and serve mode alike: an undefined variable or attribute prints as the empty string, is
//! falsy and iterates as an empty sequence, so templates may test optional fields directly
//! (`{% if meta.subtitle %}`); only operations on an undefined value beyond that (such as
//! looking up an attribute *of* it) fail. A strict mode for theme authors can be added
//! later behind a CLI flag without changing templates that use `is defined`.
//!
//! The environment caches each template after the loader first returns it, so create a new
//! environment per render (the loader itself never caches: disk themes are re-read).
//! Render errors convert with `?` (`From<minijinja::Error> for Error`): the message carries
//! minijinja's debug info (source excerpt, referenced variables) whenever minijinja
//! captured it, which for runtime errors happens only in a debug environment (serve mode).
//! [`crate::error::template_error`] gives explicit control over that.

use std::sync::Arc;

use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior};

use crate::error::{Error, Result};

use super::Theme;

/// A minijinja environment for `theme`: the loader returns the first `templates/<name>`
/// walking the chain child → root (`@<theme>/<name>` reads that theme only), `.html`
/// templates are auto-escaped (HTML), undefined values are lenient (see the module docs)
/// and `debug` is passed to `Environment::set_debug` (`true` in serve mode).
///
/// The loader captures a clone of `theme`. A load failure (unreadable file, bad UTF-8, an
/// `@` name that names no theme of the chain) becomes a minijinja `InvalidOperation` error
/// naming the template; a template no theme has (including a name
/// [`Theme::read_template`] refuses, such as `../x.html`) is minijinja's
/// "template not found".
pub fn environment(theme: &Theme, debug: bool) -> Result<Environment<'static>> {
    let mut env = Environment::new();
    env.set_debug(debug);
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".html") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    env.set_undefined_behavior(UndefinedBehavior::Lenient);
    let theme = Arc::new(theme.clone());
    env.set_loader(move |name| load(&theme, name));
    Ok(env)
}

/// The loader: template source from the chain, or a minijinja error.
fn load(theme: &Theme, name: &str) -> std::result::Result<Option<String>, minijinja::Error> {
    match theme.read_template(name) {
        Ok(found) => {
            if let Some((from, _)) = &found {
                tracing::trace!(template = name, theme = %from, "loaded template");
            }
            Ok(found.map(|(_, source)| source))
        }
        Err(err) => Err(minijinja::Error::new(
            ErrorKind::InvalidOperation,
            load_detail(err),
        )),
    }
}

/// The detail of a [`Theme::read_template`] error for the loader's minijinja error. The
/// messages already name the template (or the file path), so only the message is used:
/// the outer `theme '…':` / `template '…':` prefixes would repeat what the enclosing
/// [`Error::Template`] says.
fn load_detail(err: Error) -> String {
    match err {
        Error::Theme { message, .. } | Error::Template { message, .. } => message,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Error, template_error};
    use crate::theme::{ThemeDir, ThemeManifest, ThemeSource};
    use std::path::Path;

    /// A one-theme chain on disk with the given templates (no manifest checks).
    fn disk_theme(root: &Path, templates: &[(&str, &str)]) -> Theme {
        for (name, source) in templates {
            let path = root.join("templates").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        Theme {
            chain: vec![ThemeDir {
                name: "t".into(),
                source: ThemeSource::Disk {
                    dir: root.to_path_buf(),
                },
                manifest: ThemeManifest::default(),
            }],
        }
    }

    #[test]
    fn html_templates_are_auto_escaped_others_not() {
        let tmp = tempfile::tempdir().unwrap();
        let theme = disk_theme(
            tmp.path(),
            &[("a.html", "{{ x }}|{{ x|safe }}"), ("a.txt", "{{ x }}")],
        );
        let env = environment(&theme, false).unwrap();
        let ctx = minijinja::context! { x => "<b>&</b>" };
        let html = env.get_template("a.html").unwrap().render(&ctx).unwrap();
        assert_eq!(html, "&lt;b&gt;&amp;&lt;&#x2f;b&gt;|<b>&</b>");
        let text = env.get_template("a.txt").unwrap().render(&ctx).unwrap();
        assert_eq!(text, "<b>&</b>");
    }

    #[test]
    fn undefined_values_are_lenient() {
        let tmp = tempfile::tempdir().unwrap();
        let source = "[{{ meta.subtitle }}]{% if meta.subtitle %}x{% endif %}{% for i in meta.list %}{{ i }}{% endfor %}";
        let theme = disk_theme(tmp.path(), &[("a.html", source)]);
        for debug in [false, true] {
            let env = environment(&theme, debug).unwrap();
            let ctx = minijinja::context! { meta => minijinja::context! { title => "T" } };
            assert_eq!(
                env.get_template("a.html").unwrap().render(&ctx).unwrap(),
                "[]"
            );
        }
    }

    /// Renders template `name` and returns the converted error's fields.
    fn render_error(
        theme: &Theme,
        debug: bool,
        name: &str,
        ctx: minijinja::Value,
    ) -> (String, Option<usize>, String) {
        let env = environment(theme, debug).unwrap();
        assert_eq!(env.debug(), debug);
        let err = env.get_template(name).unwrap().render(ctx).unwrap_err();
        match template_error(err, env.debug()) {
            Error::Template {
                template,
                line,
                message,
            } => (template, line, message),
            other => panic!("expected Error::Template, got {other:?}"),
        }
    }

    #[test]
    fn errors_map_to_template_errors_at_the_innermost_location() {
        let tmp = tempfile::tempdir().unwrap();
        let theme = disk_theme(
            tmp.path(),
            &[
                ("syntax.html", "line 1\n{% include \"bad-syntax.html\" %}"),
                ("bad-syntax.html", "one\ntwo\n{{ 1 + }}\n"),
                ("outer.html", "a\n{% include \"mid.html\" %}"),
                ("mid.html", "{% include \"inner.html\" %}"),
                ("inner.html", "a\n{{ x.y.z }}\n"),
            ],
        );
        let (template, line, message) = render_error(&theme, false, "syntax.html", ().into());
        assert_eq!((template.as_str(), line), ("bad-syntax.html", Some(3)));
        assert!(message.starts_with("syntax error"), "{message}");
        assert!(
            !message.contains("(in "),
            "location not repeated: {message}"
        );

        let (template, line, message) = render_error(&theme, false, "outer.html", ().into());
        assert_eq!((template.as_str(), line), ("inner.html", Some(2)));
        assert_eq!(
            message,
            "undefined value (via mid.html line 1, outer.html line 2)"
        );
    }

    /// Renders `name` in an environment with the given debug flag and converts the error
    /// with `From` (what `?` does).
    fn from_error(theme: &Theme, debug: bool, name: &str, ctx: minijinja::Value) -> String {
        let env = environment(theme, debug).unwrap();
        let err = env.get_template(name).unwrap().render(ctx).unwrap_err();
        match Error::from(err) {
            Error::Template { message, .. } => message,
            other => panic!("expected Error::Template, got {other:?}"),
        }
    }

    #[test]
    fn from_carries_debug_info_exactly_in_debug_environments() {
        let tmp = tempfile::tempdir().unwrap();
        let theme = disk_theme(
            tmp.path(),
            &[
                ("outer.html", "{% include \"inner.html\" %}"),
                ("inner.html", "a\n{{ x.y.z }}\n"),
                ("syntax.html", "one\n{{ 1 + }}\n"),
            ],
        );
        let ctx = minijinja::context! { x => minijinja::context! { w => 1 } };
        // Serve mode (debug): `?` keeps the source excerpt and the referenced variables.
        let message = from_error(&theme, true, "outer.html", ctx.clone());
        assert!(
            message.starts_with("undefined value (via outer.html line 1)\n"),
            "{message}"
        );
        assert!(message.contains("2 > {{ x.y.z }}"), "{message}");
        assert!(message.contains("Referenced variables"), "{message}");
        // Build mode: minijinja captures no debug info for runtime errors.
        let message = from_error(&theme, false, "outer.html", ctx);
        assert_eq!(message, "undefined value (via outer.html line 1)");
        // Syntax errors always carry their source; `template_error(_, false)` drops it.
        let env = environment(&theme, false).unwrap();
        let syntax_error = || env.get_template("syntax.html").unwrap_err();
        let Error::Template { message, .. } = Error::from(syntax_error()) else {
            panic!("expected Error::Template");
        };
        assert!(message.contains("2 > {{ 1 + }}"), "{message}");
        let Error::Template { message, .. } = template_error(syntax_error(), false) else {
            panic!("expected Error::Template");
        };
        assert!(!message.contains('\n'), "{message}");
    }

    #[test]
    fn recursive_include_sites_are_collapsed() {
        let tmp = tempfile::tempdir().unwrap();
        let theme = disk_theme(
            tmp.path(),
            &[
                ("deck.html", "{% include \"slide.html\" %}"),
                ("slide.html", "x\n{% include \"slide.html\" %}"),
            ],
        );
        let (template, line, message) = render_error(&theme, false, "deck.html", ().into());
        assert_eq!((template.as_str(), line), ("slide.html", Some(2)));
        assert!(message.contains("recursion limit exceeded"), "{message}");
        // One entry per distinct site: `slide.html line 2 ×N` (N set by minijinja's
        // recursion limit), then the outermost include.
        let via = message.split_once(" (via ").map_or("", |(_, via)| via);
        let (repeated, outer) = via.split_once(", ").unwrap_or_default();
        assert!(
            repeated.starts_with("slide.html line 2 ×"),
            "collapsed: {message}"
        );
        assert_eq!(outer, "deck.html line 1)", "{message}");
        assert_eq!(message.matches("slide.html line 2").count(), 1, "{message}");
    }

    #[test]
    fn debug_errors_carry_the_innermost_debug_info() {
        let tmp = tempfile::tempdir().unwrap();
        let theme = disk_theme(
            tmp.path(),
            &[
                ("outer.html", "{% include \"inner.html\" %}"),
                ("inner.html", "a\n{{ x.y.z }}\n"),
            ],
        );
        let ctx = minijinja::context! { x => minijinja::context! { w => 1 } };
        let (template, line, message) = render_error(&theme, true, "outer.html", ctx.clone());
        assert_eq!((template.as_str(), line), ("inner.html", Some(2)));
        assert!(
            message.starts_with("undefined value (via outer.html line 1)\n"),
            "{message}"
        );
        assert!(
            message.contains("2 > {{ x.y.z }}"),
            "source excerpt: {message}"
        );
        assert!(message.contains("Referenced variables"), "{message}");

        let (_, _, message) = render_error(&theme, false, "outer.html", ctx);
        assert_eq!(message, "undefined value (via outer.html line 1)");
    }

    #[test]
    fn missing_templates_and_bad_names_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let theme = disk_theme(
            tmp.path(),
            &[
                ("a.html", "{% include \"nope.html\" %}"),
                ("b.html", "{% include \"@other/x.html\" %}"),
            ],
        );
        let (template, _, message) = render_error(&theme, false, "a.html", ().into());
        assert_eq!(template, "a.html");
        assert!(message.contains("nope.html"), "{message}");

        let (template, line, message) = render_error(&theme, false, "b.html", ().into());
        assert_eq!((template.as_str(), line), ("b.html", Some(1)));
        assert_eq!(
            message,
            "invalid operation: template '@other/x.html': no theme named 'other' in the chain (t)"
        );

        let env = environment(&theme, false).unwrap();
        let Error::Template { template, line, .. } =
            Error::from(env.get_template("nope.html").unwrap_err())
        else {
            panic!("expected Error::Template");
        };
        assert_eq!((template.as_str(), line), ("nope.html", None));
    }

    #[test]
    fn names_the_lookup_refuses_are_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let theme = disk_theme(
            tmp.path(),
            &[
                ("slide.html", "slide"),
                ("layouts/a.html", "{% include \"../slide.html\" %}"),
            ],
        );
        let env = environment(&theme, false).unwrap();
        for name in ["layouts/../slide.html", "./slide.html", "../theme.toml"] {
            let err = env.get_template(name).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::TemplateNotFound, "{name}");
        }
        let (template, _, message) = render_error(&theme, false, "layouts/a.html", ().into());
        assert_eq!(template, "layouts/a.html");
        assert!(message.starts_with("template not found"), "{message}");
    }
}
