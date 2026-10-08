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
//! Convert render errors with [`crate::error::template_error`] passing `env.debug()` so
//! that serve mode (debug) carries the source excerpt and referenced variables.

use std::sync::Arc;

use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior};

use crate::error::Result;

use super::Theme;

/// A minijinja environment for `theme`: the loader returns the first `templates/<name>`
/// walking the chain child → root (`@<theme>/<name>` reads that theme only), `.html`
/// templates are auto-escaped (HTML), undefined values are lenient (see the module docs)
/// and `debug` is passed to `Environment::set_debug` (`true` in serve mode).
///
/// The loader captures a clone of `theme`. A load failure (unreadable file, bad UTF-8, an
/// `@` name that names no theme of the chain) becomes a minijinja error naming the
/// template; a template no theme has is minijinja's "template not found".
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
            format!("cannot load template '{name}': {err}"),
        )),
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
        // `From` never adds debug info.
        let env = environment(&theme, true).unwrap();
        let err = env
            .get_template("outer.html")
            .unwrap()
            .render(())
            .unwrap_err();
        let Error::Template { message, .. } = Error::from(err) else {
            panic!("expected Error::Template");
        };
        assert!(!message.contains("x.y.z"), "{message}");
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

        let (_, _, message) = render_error(&theme, false, "b.html", ().into());
        assert!(message.contains("no theme named 'other'"), "{message}");

        let env = environment(&theme, false).unwrap();
        let Error::Template { template, line, .. } =
            Error::from(env.get_template("nope.html").unwrap_err())
        else {
            panic!("expected Error::Template");
        };
        assert_eq!((template.as_str(), line), ("<unknown>", None));
    }
}
