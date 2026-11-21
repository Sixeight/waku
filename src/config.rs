use std::path::{Path, PathBuf};

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
        if let Some(config) = Self::try_load_fast(dir) {
            return Ok(config);
        }
        Ok(Self::from_entries(git::config_get_regexp_in(
            dir,
            Self::PATTERN,
        )?))
    }

    /// Spawn-free fast path reading system, global, and local Git config files
    /// directly. Returns `None` whenever override env vars, includes, worktree
    /// config, non-standard layouts, or complex syntax require `git config`.
    pub(crate) fn try_load_fast(dir: &Path) -> Option<Self> {
        const OVERRIDE_ENVS: &[&str] = &[
            "GIT_CONFIG",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
        ];
        if OVERRIDE_ENVS
            .iter()
            .any(|var| std::env::var_os(var).is_some())
        {
            return None;
        }

        let dot_git = dir.join(".git");
        if !dot_git.is_dir() {
            return None;
        }
        let local_config = dot_git.join("config");
        if !local_config.is_file() {
            return None;
        }

        let mut files = Vec::with_capacity(4);

        // 1. System config
        let no_system = std::env::var("GIT_CONFIG_NOSYSTEM")
            .ok()
            .is_some_and(|v| is_git_truthy(&v));
        if !no_system {
            match std::env::var_os("GIT_CONFIG_SYSTEM") {
                Some(path) => {
                    if !path.is_empty() && path != "/dev/null" {
                        files.push(PathBuf::from(path));
                    }
                }
                None => files.push(PathBuf::from("/etc/gitconfig")),
            }
        }

        // 2. Global config
        match std::env::var_os("GIT_CONFIG_GLOBAL") {
            Some(path) => {
                if !path.is_empty() && path != "/dev/null" {
                    files.push(PathBuf::from(path));
                }
            }
            None => {
                let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
                let home_path = PathBuf::from(&home);
                let xdg = std::env::var_os("XDG_CONFIG_HOME")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home_path.join(".config"));
                files.push(xdg.join("git/config"));
                files.push(home_path.join(".gitconfig"));
            }
        }

        // 3. Local repo config
        files.push(local_config);

        let mut entries = Vec::new();
        for path in files {
            let contents = match std::fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            parse_git_config_into(&contents, &mut entries)?;
        }

        Some(Self::from_entries(entries))
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

fn is_git_truthy(val: &str) -> bool {
    matches!(
        val.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_git_config_into(contents: &str, entries: &mut Vec<(String, String)>) -> Option<()> {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let mut section: Option<(String, Option<String>)> = None;

    for raw_line in contents.split('\n') {
        let raw_line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        // Any trailing backslash might be a multiline continuation; fall back to git.
        if raw_line.ends_with('\\') {
            return None;
        }
        let line = raw_line.trim_start_matches([' ', '\t']);
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let (sec, sub) = parse_section_header(rest)?;
            if sec == "include" || sec == "includeif" {
                return None;
            }
            section = Some((sec, sub));
            continue;
        }
        let (sec, sub) = section.as_ref()?;
        let (raw_key, raw_value) = match line.split_once('=') {
            Some((k, v)) => (k.trim_end_matches([' ', '\t']), Some(v)),
            None => {
                let k = strip_unquoted_comment(line).trim_end_matches([' ', '\t']);
                (k, None)
            }
        };
        if !is_valid_key(raw_key) {
            return None;
        }
        let key = raw_key.to_ascii_lowercase();
        if sec == "extensions" && key == "worktreeconfig" {
            return None;
        }
        let matches_pattern = sec == "waku"
            || (sec == "checkout" && sub.is_none() && key == "workers")
            || (sec == "branch" && sub.is_some() && key == "remote");
        if !matches_pattern {
            continue;
        }
        let full_key = match sub {
            Some(sub) => format!("{sec}.{sub}.{key}"),
            None => format!("{sec}.{key}"),
        };
        let value = match raw_value {
            Some(v) => parse_config_value(v)?,
            None => String::new(),
        };
        entries.push((full_key, value));
    }

    Some(())
}

fn parse_section_header(rest: &str) -> Option<(String, Option<String>)> {
    let bytes = rest.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && (bytes[idx].is_ascii_alphanumeric() || bytes[idx] == b'-') {
        idx += 1;
    }
    if idx == 0 {
        return None;
    }
    let section = rest[..idx].to_ascii_lowercase();
    let tail = &rest[idx..];
    if let Some(after_close) = tail.strip_prefix(']') {
        let after = after_close.trim_start_matches([' ', '\t']);
        if after.is_empty() || after.starts_with('#') || after.starts_with(';') {
            return Some((section, None));
        }
        return None;
    }
    let after_ws = tail.trim_start_matches([' ', '\t']);
    if after_ws.len() == tail.len() {
        return None;
    }
    let after_quote = after_ws.strip_prefix('"')?;
    let close_quote = after_quote.find('"')?;
    let subsection = &after_quote[..close_quote];
    if subsection.contains('\\') {
        return None;
    }
    let after_close = after_quote[close_quote + 1..].strip_prefix(']')?;
    let after = after_close.trim_start_matches([' ', '\t']);
    if after.is_empty() || after.starts_with('#') || after.starts_with(';') {
        return Some((section, Some(subsection.to_string())));
    }
    None
}

fn strip_unquoted_comment(line: &str) -> &str {
    match line.find(['#', ';']) {
        Some(pos) => &line[..pos],
        None => line,
    }
}

fn is_valid_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_alphabetic() => {
            bytes.all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }
        _ => false,
    }
}

fn parse_config_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim_start_matches([' ', '\t']);
    let mut out = String::with_capacity(trimmed.len());
    let mut in_quotes = false;
    let mut pending_ws = String::new();
    let mut chars = trimmed.chars();

    while let Some(ch) = chars.next() {
        if !in_quotes {
            match ch {
                '#' | ';' => break,
                '"' => {
                    out.push_str(&pending_ws);
                    pending_ws.clear();
                    in_quotes = true;
                }
                ' ' | '\t' => pending_ws.push(ch),
                '\\' => {
                    out.push_str(&pending_ws);
                    pending_ws.clear();
                    out.push(decode_escape(chars.next()?)?);
                }
                _ => {
                    out.push_str(&pending_ws);
                    pending_ws.clear();
                    out.push(ch);
                }
            }
        } else {
            match ch {
                '"' => in_quotes = false,
                '\\' => out.push(decode_escape(chars.next()?)?),
                _ => out.push(ch),
            }
        }
    }

    if in_quotes {
        return None;
    }
    Some(out)
}

fn decode_escape(ch: char) -> Option<char> {
    match ch {
        '\\' => Some('\\'),
        '"' => Some('"'),
        'n' => Some('\n'),
        't' => Some('\t'),
        'b' => Some('\x08'),
        _ => None,
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

    #[test]
    fn fast_config_parser_matches_git_semantics_and_bails_on_complex_syntax() {
        let sample = "\
[core]
\trepositoryformatversion = 0
[checkout]
\tworkers
[waku \"link\"]
\tinclude = a b   # trailing comment
\tinclude = \"c # d\"
\tinclude = \"quoted \\\"value\\\"\"
[Waku \"Create\"]
\tfetch = true
[branch \"feat/x\"]
\tremote = origin
";
        let mut parsed = Vec::new();
        parse_git_config_into(sample, &mut parsed).unwrap();
        assert_eq!(
            parsed,
            vec![
                ("checkout.workers".to_string(), "".to_string()),
                ("waku.link.include".to_string(), "a b".to_string()),
                ("waku.link.include".to_string(), "c # d".to_string()),
                (
                    "waku.link.include".to_string(),
                    "quoted \"value\"".to_string()
                ),
                ("waku.Create.fetch".to_string(), "true".to_string()),
                ("branch.feat/x.remote".to_string(), "origin".to_string()),
            ]
        );

        // Must bail out on include, includeIf, worktreeConfig, or line continuations.
        for complex in [
            "[include]\n\tpath = ~/.gitconfig.local\n",
            "[includeIf \"gitdir:~/work/\"]\n\tpath = ~/.gitconfig.work\n",
            "[extensions]\n\tworktreeConfig = true\n",
            "[waku]\n\tfoo = bar \\\nbaz\n",
        ] {
            let mut out = Vec::new();
            assert!(
                parse_git_config_into(complex, &mut out).is_none(),
                "expected fallback for: {complex:?}"
            );
        }
    }
}
