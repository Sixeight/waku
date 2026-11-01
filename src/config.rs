use std::path::Path;

use anyhow::Result;

use crate::git;

/// Git config entries used by waku, loaded in one `git config --get-regexp`.
///
/// The pattern covers `waku.*`, `checkout.workers`, and `branch.*.remote`.
/// Single-value lookups use the last matching entry (`last` / `bool`),
/// matching `git config --get`. `first` remains for callers that need
/// the earliest value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    entries: Vec<(String, String)>,
}

impl Config {
    pub const PATTERN: &'static str = r"^(waku\.|checkout\.workers$|branch\..*\.remote$)";

    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self::from_entries(git::config_get_regexp_in(
            dir,
            Self::PATTERN,
        )?))
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: Vec<(String, String)>) -> Self {
        Self { entries }
    }

    pub fn values(&self, key: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn bool(&self, key: &str) -> bool {
        self.last(key)
            .is_some_and(|value| matches!(value, "true" | "yes" | "on" | "1"))
    }

    pub fn first(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn last(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    pub fn iter(&self) -> impl Iterator<Item = &(String, String)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Config {
        Config::from_entries(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
    }

    #[test]
    fn values_filters_by_key() {
        let config = entries(&[
            ("waku.link.include", "node_modules"),
            ("waku.copy.include", ".env"),
            ("waku.link.include", ".direnv"),
        ]);
        assert_eq!(
            config.values("waku.link.include"),
            vec!["node_modules", ".direnv"]
        );
        assert_eq!(config.values("waku.copy.include"), vec![".env"]);
    }

    #[test]
    fn values_returns_empty_for_missing_key() {
        let config = entries(&[("waku.link.include", "node_modules")]);
        assert!(config.values("waku.copy.include").is_empty());
    }

    #[test]
    fn values_empty_config() {
        assert!(Config::empty().values("waku.link.include").is_empty());
    }

    #[test]
    fn bool_reads_last_matching_value() {
        let config = entries(&[
            ("waku.create.fetch", "false"),
            ("waku.create.fetch", "true"),
        ]);
        assert!(config.bool("waku.create.fetch"));
    }

    #[test]
    fn bool_returns_false_for_missing_key() {
        assert!(!Config::empty().bool("waku.create.fetch"));
    }

    #[test]
    fn last_wins_for_duplicated_keys() {
        let config = entries(&[
            ("waku.worktrees.path", "/first"),
            ("waku.worktrees.path", "/last"),
            ("waku.worktreeinclude", "copy"),
            ("waku.worktreeinclude", "link"),
            ("waku.command.agent", "global-agent"),
            ("waku.command.agent", "local-agent"),
            ("waku.create.fetch", "false"),
            ("waku.create.fetch", "true"),
        ]);
        assert_eq!(config.last("waku.worktrees.path"), Some("/last"));
        assert_eq!(config.last("waku.worktreeinclude"), Some("link"));
        assert_eq!(config.last("waku.command.agent"), Some("local-agent"));
        assert!(config.bool("waku.create.fetch"));
        assert_eq!(config.last("missing"), None);
    }

    #[test]
    fn first_still_returns_the_earliest_value() {
        let config = entries(&[
            ("waku.worktrees.path", "/first"),
            ("waku.worktrees.path", "/last"),
            ("waku.worktreeinclude", "copy"),
            ("waku.worktreeinclude", "link"),
        ]);
        assert_eq!(config.first("waku.worktrees.path"), Some("/first"));
        assert_eq!(config.first("waku.worktreeinclude"), Some("copy"));
        assert_eq!(config.first("missing"), None);
    }

    #[test]
    fn contains_key_is_true_for_any_matching_entry() {
        let config = entries(&[("checkout.workers", "4")]);
        assert!(config.contains_key("checkout.workers"));
        assert!(!config.contains_key("waku.create.fetch"));
    }
}
