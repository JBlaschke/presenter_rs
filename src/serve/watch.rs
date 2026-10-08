//! File watcher, debouncing, rebuild loop and change classification.
//!
//! Implemented by T20. T00 defines only [`ChangeKind`], the contract type shared by the
//! watcher (T20) and the dev server (T21).

/// What kind of rebuild a change batch requires (live-reload.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// Only stylesheets/theme assets changed.
    Css,
    /// Anything else.
    Content,
}

#[cfg(test)]
mod tests {
    use super::ChangeKind;

    #[test]
    fn change_kind_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&ChangeKind::Css).unwrap(), "\"css\"");
        assert_eq!(
            serde_json::to_string(&ChangeKind::Content).unwrap(),
            "\"content\""
        );
        let parsed: ChangeKind = serde_json::from_str("\"css\"").unwrap();
        assert_eq!(parsed, ChangeKind::Css);
    }
}
