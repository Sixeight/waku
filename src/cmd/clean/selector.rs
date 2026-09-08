use console::{style, truncate_str, Term};

use super::{candidate_title, table_header, table_layout, worktree_row, WorktreeAnnotations};

pub(super) struct SelectorDisplay {
    terminal_size: (u16, u16),
    start: usize,
    capacity: usize,
    title: String,
    header: String,
    rows: Vec<[String; 2]>,
}

pub(super) struct SelectorUpdate<'a> {
    pub(super) checked: &'a [bool],
    pub(super) previous_cursor: usize,
    pub(super) cursor: usize,
    pub(super) selected_count: usize,
}

impl SelectorDisplay {
    pub(super) fn new(
        term: &Term,
        items: &[(String, Option<String>)],
        ann: &WorktreeAnnotations,
        cursor: usize,
    ) -> Self {
        Self::build(term.size(), items, ann, cursor, 0)
    }

    fn build(
        terminal_size: (u16, u16),
        items: &[(String, Option<String>)],
        ann: &WorktreeAnnotations,
        cursor: usize,
        previous_start: usize,
    ) -> Self {
        let row_width = usize::from(terminal_size.1).saturating_sub(4);
        let layout = table_layout(items, ann, row_width);
        let capacity = viewport_capacity(usize::from(terminal_size.0));
        let start = viewport_start(items.len(), cursor, capacity, previous_start);
        let rows = items
            .iter()
            .map(|(path, branch)| {
                [
                    worktree_row(path, branch.as_deref(), ann, &layout, false),
                    worktree_row(path, branch.as_deref(), ann, &layout, true),
                ]
            })
            .collect();
        Self {
            terminal_size,
            start,
            capacity,
            title: candidate_title(layout.row_width),
            header: table_header(&layout),
            rows,
        }
    }

    fn visible_end(&self) -> usize {
        (self.start + self.capacity).min(self.rows.len())
    }

    pub(super) fn line_count(&self) -> usize {
        if self.capacity == 0 {
            1
        } else {
            self.visible_end() - self.start + 3
        }
    }

    fn candidate_line(&self, index: usize, checked: bool, cursor: usize) -> String {
        let row = &self.rows[index][usize::from(checked)];
        if cursor == index {
            format!("  {} {row}", style("▸").bold())
        } else {
            format!("    {row}")
        }
    }

    fn footer(&self, selected_count: usize, cursor: usize) -> String {
        let position = if cursor == self.rows.len() {
            "run".to_string()
        } else {
            format!("{}/{}", cursor + 1, self.rows.len())
        };
        let summary = format!("{position} · {selected_count} selected");
        let footer = if cursor == self.rows.len() {
            format!("  {} {}", style("▸").bold(), style(summary).bold())
        } else {
            format!("    {}", style(summary).dim())
        };
        truncate_str(&footer, usize::from(self.terminal_size.1).max(1), "…").into_owned()
    }

    pub(super) fn draw(
        &self,
        term: &Term,
        checked: &[bool],
        cursor: usize,
        selected_count: usize,
    ) -> std::io::Result<()> {
        if self.capacity == 0 {
            return term.write_line(&self.footer(selected_count, cursor));
        }
        term.write_line(&self.title)?;
        term.write_line(&format!("    {}", self.header))?;
        for (index, &is_checked) in checked
            .iter()
            .enumerate()
            .take(self.visible_end())
            .skip(self.start)
        {
            term.write_line(&self.candidate_line(index, is_checked, cursor))?;
        }
        term.write_line(&self.footer(selected_count, cursor))
    }

    pub(super) fn update(
        &mut self,
        term: &Term,
        items: &[(String, Option<String>)],
        ann: &WorktreeAnnotations,
        update: SelectorUpdate<'_>,
    ) -> std::io::Result<()> {
        let terminal_size = term.size();
        let capacity = viewport_capacity(usize::from(terminal_size.0));
        let start = viewport_start(items.len(), update.cursor, capacity, self.start);
        if terminal_size != self.terminal_size || start != self.start {
            term.clear_last_lines(self.line_count())?;
            if terminal_size != self.terminal_size {
                *self = Self::build(terminal_size, items, ann, update.cursor, start);
            } else {
                self.start = start;
            }
            return self.draw(term, update.checked, update.cursor, update.selected_count);
        }

        let changed = if update.previous_cursor == update.cursor {
            &[update.cursor][..]
        } else {
            &[update.previous_cursor, update.cursor][..]
        };
        for &index in changed {
            if (self.start..self.visible_end()).contains(&index) {
                let line = self.candidate_line(index, update.checked[index], update.cursor);
                self.replace_line(term, index - self.start + 2, &line)?;
            }
        }
        self.replace_line(
            term,
            self.line_count() - 1,
            &self.footer(update.selected_count, update.cursor),
        )
    }

    fn replace_line(&self, term: &Term, line: usize, content: &str) -> std::io::Result<()> {
        let remaining = self.line_count() - line;
        term.move_cursor_up(remaining)?;
        term.clear_line()?;
        term.write_line(content)?;
        term.move_cursor_down(remaining - 1)
    }
}

fn viewport_capacity(terminal_rows: usize) -> usize {
    terminal_rows.saturating_sub(4)
}

fn viewport_start(
    item_count: usize,
    cursor: usize,
    capacity: usize,
    previous_start: usize,
) -> usize {
    if capacity == 0 {
        return item_count;
    }
    let last_start = item_count.saturating_sub(capacity);
    let previous_start = previous_start.min(last_start);
    if cursor >= item_count {
        last_start
    } else if cursor < previous_start {
        cursor
    } else if cursor >= previous_start + capacity {
        (cursor + 1).saturating_sub(capacity).min(last_start)
    } else {
        previous_start
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use console::strip_ansi_codes;

    use super::*;

    #[test]
    fn viewport_stays_bounded_and_follows_cursor() {
        assert_eq!(viewport_start(100, 100, 10, 0), 90);
        assert_eq!(viewport_start(100, 99, 10, 90), 90);
        assert_eq!(viewport_start(100, 89, 10, 90), 89);
        assert_eq!(viewport_start(100, 90, 10, 89), 89);
        assert_eq!(viewport_start(100, 99, 10, 89), 90);
    }

    #[test]
    fn viewport_uses_available_terminal_rows() {
        assert_eq!(viewport_capacity(24), 20);
        assert_eq!(viewport_capacity(5), 1);
        assert_eq!(viewport_capacity(4), 0);
        assert_eq!(viewport_capacity(2), 0);
    }

    #[test]
    fn display_bounds_rows_and_summarizes_selection() {
        let items: Vec<_> = (0..100)
            .map(|index| {
                (
                    format!("/tmp/worktrees/branch-{index}"),
                    Some(format!("branch-{index}")),
                )
            })
            .collect();
        let annotations = WorktreeAnnotations {
            dirty: &HashSet::new(),
            unchanged: &HashSet::new(),
            gone: &HashSet::new(),
            commits: &HashMap::new(),
            unique_commits: &HashMap::new(),
            force: false,
        };
        let display = SelectorDisplay::build((12, 80), &items, &annotations, 100, 0);

        assert_eq!(display.start, 92);
        assert_eq!(display.visible_end(), 100);
        assert_eq!(display.line_count(), 11);
        assert_eq!(
            strip_ansi_codes(&display.footer(2, 99)),
            "    100/100 · 2 selected"
        );
        assert_eq!(
            strip_ansi_codes(&display.footer(2, 100)),
            "  ▸ run · 2 selected"
        );

        let compact = SelectorDisplay::build((3, 80), &items, &annotations, 100, 0);
        assert_eq!(compact.start, 100);
        assert_eq!(compact.line_count(), 1);
    }
}
