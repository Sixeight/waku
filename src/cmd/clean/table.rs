use console::{measure_text_width, style, truncate_str};

use super::WorktreeAnnotations;

pub(super) struct TableLayout {
    branch: usize,
    reason: usize,
    files: usize,
    commits: usize,
    updated: usize,
    pub(super) row_width: usize,
    show_details: bool,
}

pub(super) fn display_name(path: &str, branch: Option<&str>) -> String {
    branch.map(|b| b.to_string()).unwrap_or_else(|| {
        std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    })
}

pub(super) fn table_layout(
    items: &[(String, Option<String>)],
    ann: &WorktreeAnnotations,
    row_width: usize,
) -> TableLayout {
    let mut layout = TableLayout {
        branch: measure_text_width("BRANCH"),
        reason: measure_text_width("REASON"),
        files: measure_text_width("FILES"),
        commits: measure_text_width("COMMITS"),
        updated: measure_text_width("UPDATED"),
        row_width: row_width.max(1),
        show_details: row_width >= 72,
    };
    for (path, branch) in items {
        let branch = branch.as_deref();
        layout.branch = layout
            .branch
            .max(measure_text_width(&display_name(path, branch)));
        layout.reason = layout
            .reason
            .max(measure_text_width(worktree_reason(path, branch, ann)));
        layout.files = layout.files.max(measure_text_width(file_status(path, ann)));
        layout.commits = layout
            .commits
            .max(measure_text_width(&commit_status(path, branch, ann)));
        if let Some(commit) = ann.commits.get(path) {
            layout.updated = layout.updated.max(measure_text_width(&commit.updated));
        }
    }
    let (fixed_width, subject_reserve) = if layout.show_details {
        let subject = ann
            .commits
            .values()
            .map(|commit| measure_text_width(&commit.subject))
            .max()
            .unwrap_or(1)
            .min(12)
            .max(measure_text_width("SUBJECT"));
        (
            15 + layout.reason + layout.files + layout.commits + layout.updated,
            subject,
        )
    } else {
        (11 + layout.reason + layout.files + layout.commits, 0)
    };
    let branch_budget = layout
        .row_width
        .saturating_sub(fixed_width + subject_reserve)
        .max(1);
    layout.branch = layout.branch.min(branch_budget);
    layout
}

pub(super) fn worktree_reason(
    path: &str,
    branch: Option<&str>,
    ann: &WorktreeAnnotations,
) -> &'static str {
    if ann.gone.contains(path) {
        "closed"
    } else if ann.unchanged.contains(path) {
        "unchanged"
    } else if branch.is_none() {
        "detached"
    } else {
        "merged"
    }
}

fn commit_status(path: &str, branch: Option<&str>, ann: &WorktreeAnnotations) -> String {
    if worktree_reason(path, branch, ann) == "merged" {
        "merged".to_string()
    } else {
        ann.unique_commits
            .get(path)
            .map(|count| format!("{count} unique"))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

fn padded(value: &str, width: usize) -> String {
    let value = truncate_str(value, width.max(1), "…");
    let padding = " ".repeat(width.saturating_sub(measure_text_width(&value)));
    format!("{value}{padding}")
}

pub(super) fn candidate_title(width: usize) -> String {
    truncate_str(
        "Clean candidates ([✔] selected by default):",
        width.max(1),
        "…",
    )
    .into_owned()
}

pub(super) fn table_header(layout: &TableLayout) -> String {
    let header = if layout.show_details {
        format!(
            "DEL  {}  {}  {}  {}  {}  SUBJECT",
            padded("BRANCH", layout.branch),
            padded("REASON", layout.reason),
            padded("FILES", layout.files),
            padded("COMMITS", layout.commits),
            padded("UPDATED", layout.updated),
        )
    } else {
        format!(
            "DEL  {}  {}  {}  {}",
            padded("BRANCH", layout.branch),
            padded("REASON", layout.reason),
            padded("FILES", layout.files),
            padded("COMMITS", layout.commits),
        )
    };
    truncate_str(&header, layout.row_width, "…").into_owned()
}

fn file_status<'a>(path: &str, ann: &WorktreeAnnotations<'a>) -> &'a str {
    ann.protected.get(path).copied().unwrap_or_else(|| {
        if ann.dirty.contains(path) {
            "dirty"
        } else {
            "clean"
        }
    })
}

pub(super) fn worktree_row(
    path: &str,
    branch: Option<&str>,
    ann: &WorktreeAnnotations,
    layout: &TableLayout,
    checked: bool,
) -> String {
    let mark = if checked { "✔" } else { " " };
    let name = display_name(path, branch);
    let visible_name = truncate_str(&name, layout.branch, "…");
    let styled_name = if branch.is_some() {
        style(&visible_name).bold().to_string()
    } else {
        visible_name.to_string()
    };
    let name_padding = " ".repeat(
        layout
            .branch
            .saturating_sub(measure_text_width(&visible_name)),
    );
    let reason = worktree_reason(path, branch, ann);
    let files = file_status(path, ann);
    let commits = commit_status(path, branch, ann);
    let (updated, subject) = ann
        .commits
        .get(path)
        .map(|commit| (commit.updated.as_str(), commit.subject.as_str()))
        .unwrap_or(("—", "—"));
    let row = if layout.show_details {
        format!(
            "[{mark}]  {styled_name}{name_padding}  {}  {}  {}  {}  {subject}",
            padded(reason, layout.reason),
            padded(files, layout.files),
            padded(&commits, layout.commits),
            padded(updated, layout.updated),
        )
    } else {
        format!(
            "[{mark}]  {styled_name}{name_padding}  {}  {}  {}",
            padded(reason, layout.reason),
            padded(files, layout.files),
            padded(&commits, layout.commits),
        )
    };
    truncate_str(&row, layout.row_width, "…").into_owned()
}

#[cfg(test)]
mod tests {
    use super::super::CommitInfo;
    use super::*;
    use console::strip_ansi_codes;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn worktree_rows_align_decision_columns_by_display_width() {
        let merged_path = "/tmp/worktrees/short";
        let closed_path = "/tmp/worktrees/日本語";
        let items = vec![
            (merged_path.to_string(), Some("short".to_string())),
            (closed_path.to_string(), Some("日本語".to_string())),
        ];
        let dirty = HashSet::from([merged_path.to_string()]);
        let unchanged = HashSet::new();
        let gone = HashSet::from([closed_path.to_string()]);
        let commits = HashMap::from([
            (
                merged_path.to_string(),
                CommitInfo {
                    updated: "4 seconds ago".to_string(),
                    subject: "fix short".to_string(),
                },
            ),
            (
                closed_path.to_string(),
                CommitInfo {
                    updated: "2 days ago".to_string(),
                    subject: "fix wide".to_string(),
                },
            ),
        ]);
        let unique_commits = HashMap::from([(closed_path.to_string(), 2)]);
        let annotations = WorktreeAnnotations {
            protected: &HashMap::new(),
            dirty: &dirty,
            unchanged: &unchanged,
            gone: &gone,
            commits: &commits,
            unique_commits: &unique_commits,
            force: false,
        };
        let layout = table_layout(&items, &annotations, 200);

        let header = table_header(&layout);
        let merged_row = worktree_row(merged_path, Some("short"), &annotations, &layout, false);
        let closed_row = worktree_row(closed_path, Some("日本語"), &annotations, &layout, false);
        let merged = strip_ansi_codes(&merged_row);
        let closed = strip_ansi_codes(&closed_row);

        assert_eq!(
            header,
            "DEL  BRANCH  REASON  FILES  COMMITS   UPDATED        SUBJECT"
        );
        assert_eq!(
            merged,
            "[ ]  short   merged  dirty  merged    4 seconds ago  fix short"
        );
        assert_eq!(
            closed,
            "[ ]  日本語  closed  clean  2 unique  2 days ago     fix wide"
        );

        let narrow_layout = table_layout(&items, &annotations, 50);
        let narrow_header = table_header(&narrow_layout);
        let narrow_row = worktree_row(
            closed_path,
            Some("日本語"),
            &annotations,
            &narrow_layout,
            false,
        );
        assert!(measure_text_width(&narrow_header) <= 50);
        assert!(measure_text_width(&narrow_row) <= 50);
        assert!(strip_ansi_codes(&narrow_row).contains("2 unique"));
    }

    #[test]
    fn unlimited_width_keeps_a_long_branch_name() {
        let path = "/tmp/worktrees/long";
        let branch = format!("feature-{}", "n".repeat(80));
        let items = vec![(path.to_string(), Some(branch.clone()))];
        let commits = HashMap::from([(
            path.to_string(),
            CommitInfo {
                updated: "1 second ago".to_string(),
                subject: "add feature".to_string(),
            },
        )]);
        let annotations = WorktreeAnnotations {
            protected: &HashMap::new(),
            dirty: &HashSet::new(),
            unchanged: &HashSet::new(),
            gone: &HashSet::new(),
            commits: &commits,
            unique_commits: &HashMap::new(),
            force: false,
        };

        let wide = table_layout(&items, &annotations, usize::MAX);
        let wide_row = worktree_row(path, Some(&branch), &annotations, &wide, true);
        let wide_row = strip_ansi_codes(&wide_row);
        assert!(wide_row.contains(&branch), "{wide_row}");
        assert!(!wide_row.contains('…'), "{wide_row}");

        let narrow = table_layout(&items, &annotations, 40);
        let narrow_row = worktree_row(path, Some(&branch), &annotations, &narrow, true);
        let narrow_row = strip_ansi_codes(&narrow_row);
        assert!(!narrow_row.contains(&branch), "{narrow_row}");
        assert!(narrow_row.contains('…'), "{narrow_row}");
    }
}
