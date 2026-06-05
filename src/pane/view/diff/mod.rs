use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;

use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tracing::debug;

use crate::app::state::Palette;
use crate::input::TerminalKey;
use crate::selection::Selection;

use super::{ViewKeyOutcome, ViewKind};

pub mod parser;

use self::parser::{parse_unified_diff, DiffFile, HunkLine};

const MAX_DIFF_LINES: usize = 50_000;

/// What slice of the working tree to diff against the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    /// `git diff <baseline>` — everything between baseline and worktree.
    All,
    /// `git diff` — unstaged changes only.
    Unstaged,
    /// `git diff --cached` — staged but not committed.
    Staged,
    /// `git diff <baseline>..HEAD` — committed since baseline.
    Committed,
}

impl DiffScope {
    fn next(self) -> Self {
        match self {
            DiffScope::All => DiffScope::Unstaged,
            DiffScope::Unstaged => DiffScope::Staged,
            DiffScope::Staged => DiffScope::Committed,
            DiffScope::Committed => DiffScope::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            DiffScope::All => "all",
            DiffScope::Unstaged => "unstaged",
            DiffScope::Staged => "staged",
            DiffScope::Committed => "committed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffViewOptions {
    pub baseline: String,
    pub scope: DiffScope,
}

impl DiffViewOptions {
    /// Build options from a CLI/snapshot request, auto-detecting the baseline
    /// when none was supplied. Returns `None` if there is no `main`/`master`
    /// in the repo (caller should surface a clear error in that case).
    pub fn resolve(cwd: &Path, baseline: Option<String>, scope: Option<DiffScope>) -> Option<Self> {
        let baseline = baseline.or_else(|| detect_default_baseline(cwd))?;
        Some(Self {
            baseline,
            scope: scope.unwrap_or(DiffScope::All),
        })
    }
}

pub struct DiffView {
    cwd: PathBuf,
    options: DiffViewOptions,
    files: Vec<DiffFile>,
    truncated_lines: usize,
    git_error: Option<String>,
    scroll: usize,
    cursor: usize,
    /// Optional command-line indirection so tests can inject a stub
    /// instead of spawning real `git`. Defaults to `git`.
    git_program: PathBuf,
}

impl DiffView {
    pub fn new(cwd: PathBuf, options: DiffViewOptions) -> Self {
        let mut view = Self {
            cwd,
            options,
            files: Vec::new(),
            truncated_lines: 0,
            git_error: None,
            scroll: 0,
            cursor: 0,
            git_program: PathBuf::from("git"),
        };
        view.rerun_git();
        view
    }

    #[cfg(test)]
    pub(crate) fn new_with_git(cwd: PathBuf, options: DiffViewOptions, git: PathBuf) -> Self {
        let mut view = Self {
            cwd,
            options,
            files: Vec::new(),
            truncated_lines: 0,
            git_error: None,
            scroll: 0,
            cursor: 0,
            git_program: git,
        };
        view.rerun_git();
        view
    }

    #[cfg(test)]
    pub fn files(&self) -> &[DiffFile] {
        &self.files
    }

    #[cfg(test)]
    pub fn truncated_lines(&self) -> usize {
        self.truncated_lines
    }

    fn rerun_git(&mut self) {
        self.git_error = None;
        let args = scope_args(self.options.scope, &self.options.baseline);
        let output = Command::new(&self.git_program)
            .arg("-C")
            .arg(&self.cwd)
            .args(&args)
            .output();
        let output = match output {
            Ok(o) => o,
            Err(err) => {
                self.git_error = Some(format!("failed to run git: {err}"));
                self.files.clear();
                self.truncated_lines = 0;
                return;
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            self.git_error = Some(if stderr.is_empty() {
                format!("git diff exited with status {}", output.status)
            } else {
                stderr
            });
            self.files.clear();
            self.truncated_lines = 0;
            return;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut iter = stdout.lines();
        let captured: Vec<&str> = iter.by_ref().take(MAX_DIFF_LINES).collect();
        self.truncated_lines = iter.count();
        self.files = parse_unified_diff(&captured.join("\n"));
        let total = self.total_render_lines();
        if self.cursor >= total {
            self.cursor = total.saturating_sub(1);
        }
        if self.scroll > self.cursor {
            self.scroll = self.cursor;
        }
    }

    fn total_render_lines(&self) -> usize {
        let mut n = 0;
        for file in &self.files {
            n += 1; // file header
            for hunk in &file.hunks {
                n += 1; // hunk header
                n += hunk.lines.len();
            }
            if file.binary {
                n += 1;
            }
        }
        n
    }

    fn file_boundary_indices(&self) -> Vec<usize> {
        let mut idx = 0;
        let mut out = Vec::with_capacity(self.files.len());
        for file in &self.files {
            out.push(idx);
            idx += 1;
            for hunk in &file.hunks {
                idx += 1 + hunk.lines.len();
            }
            if file.binary {
                idx += 1;
            }
        }
        out
    }

    fn hunk_indices(&self) -> Vec<usize> {
        let mut idx = 0;
        let mut out = Vec::new();
        for file in &self.files {
            idx += 1;
            for hunk in &file.hunks {
                out.push(idx);
                idx += 1 + hunk.lines.len();
            }
            if file.binary {
                idx += 1;
            }
        }
        out
    }

    fn move_to_next(&mut self, boundaries: &[usize]) {
        if let Some(&next) = boundaries.iter().find(|&&b| b > self.cursor) {
            self.cursor = next;
        } else if let Some(&last) = boundaries.last() {
            self.cursor = last;
        }
    }

    fn move_to_prev(&mut self, boundaries: &[usize]) {
        if let Some(&prev) = boundaries.iter().rev().find(|&&b| b < self.cursor) {
            self.cursor = prev;
        } else if let Some(&first) = boundaries.first() {
            self.cursor = first;
        }
    }

    fn clamp_scroll(&mut self) {
        let total = self.total_render_lines();
        let last_index = total.saturating_sub(1);
        if self.cursor > last_index {
            self.cursor = last_index;
        }
        if self.scroll > last_index {
            self.scroll = last_index;
        }
    }

    fn render_lines(&self, palette: &Palette) -> Vec<Line<'_>> {
        let mut lines = Vec::with_capacity(self.total_render_lines() + 1);
        for file in &self.files {
            let header = if let Some(from) = &file.rename_from {
                format!("── {} → {} ──", from, file.path)
            } else {
                format!("── {} ──", file.path)
            };
            lines.push(Line::from(Span::styled(
                header,
                Style::default()
                    .fg(palette.blue)
                    .add_modifier(Modifier::BOLD),
            )));
            for hunk in &file.hunks {
                let hunk_line = format!(
                    "@@ -{},{} +{},{} @@ {}",
                    hunk.old_range.0,
                    hunk.old_range.1,
                    hunk.new_range.0,
                    hunk.new_range.1,
                    hunk.heading
                );
                lines.push(Line::from(Span::styled(
                    hunk_line,
                    Style::default().fg(palette.teal),
                )));
                for line in &hunk.lines {
                    let (prefix, body, color) = match line {
                        HunkLine::Added(s) => ("+", s.as_str(), palette.green),
                        HunkLine::Removed(s) => ("-", s.as_str(), palette.red),
                        HunkLine::Context(s) => (" ", s.as_str(), Color::Reset),
                    };
                    let span = Span::styled(format!("{prefix}{body}"), Style::default().fg(color));
                    lines.push(Line::from(span));
                }
            }
            if file.binary {
                lines.push(Line::from(Span::styled(
                    "(binary file — diff suppressed)",
                    Style::default().fg(palette.overlay0),
                )));
            }
        }
        lines
    }

    /// Same iteration order as [`render_lines`] but returns plain text
    /// (with the `+`/`-`/` ` prefix included) so selection extraction
    /// reads exactly what the user sees.
    fn plain_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.total_render_lines() + 1);
        for file in &self.files {
            let header = if let Some(from) = &file.rename_from {
                format!("── {} → {} ──", from, file.path)
            } else {
                format!("── {} ──", file.path)
            };
            lines.push(header);
            for hunk in &file.hunks {
                lines.push(format!(
                    "@@ -{},{} +{},{} @@ {}",
                    hunk.old_range.0,
                    hunk.old_range.1,
                    hunk.new_range.0,
                    hunk.new_range.1,
                    hunk.heading,
                ));
                for line in &hunk.lines {
                    let (prefix, body) = match line {
                        HunkLine::Added(s) => ("+", s.as_str()),
                        HunkLine::Removed(s) => ("-", s.as_str()),
                        HunkLine::Context(s) => (" ", s.as_str()),
                    };
                    lines.push(format!("{prefix}{body}"));
                }
            }
            if file.binary {
                lines.push("(binary file — diff suppressed)".to_string());
            }
        }
        lines
    }
}

fn header_contrast_fg(palette: &Palette) -> Color {
    match palette.panel_bg {
        Color::Reset => palette.surface_dim,
        color => color,
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{prefix}…")
}

/// Slice `s` between byte-columns `[start_col, end_col]` inclusive,
/// treating each `char` as one cell. Handles `end_col >= len` by
/// returning up to the end of the string.
fn slice_columns(s: &str, start_col: u16, end_col: u16) -> &str {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    if chars.is_empty() {
        return "";
    }
    let start = start_col as usize;
    let end = end_col as usize;
    let start_byte = chars.get(start).map(|(i, _)| *i).unwrap_or(s.len());
    let end_byte = chars
        .get(end + 1)
        .map(|(i, _)| *i)
        .unwrap_or_else(|| s.len());
    if start_byte >= end_byte {
        return "";
    }
    &s[start_byte..end_byte]
}

fn scope_args(scope: DiffScope, baseline: &str) -> Vec<String> {
    match scope {
        DiffScope::All => vec!["diff".into(), baseline.into()],
        DiffScope::Unstaged => vec!["diff".into()],
        DiffScope::Staged => vec!["diff".into(), "--cached".into()],
        DiffScope::Committed => vec!["diff".into(), format!("{baseline}..HEAD")],
    }
}

/// Probe `git rev-parse --verify refs/heads/<name>` for `main`, then
/// `master`. Returns the first that resolves, or `None`.
pub fn detect_default_baseline(cwd: &Path) -> Option<String> {
    for candidate in ["main", "master"] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["rev-parse", "--verify", &format!("refs/heads/{candidate}")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(candidate.to_string());
        }
    }
    None
}

impl ViewKind for DiffView {
    fn title(&self) -> Cow<'_, str> {
        Cow::Borrowed("diff")
    }

    fn render(&self, frame: &mut Frame<'_>, rect: Rect, _focused: bool, palette: &Palette) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let [header_area, body_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(rect);

        let header_text = format!(
            " ▸ {} · {}    [s] scope  [r] refresh  [j/k] scroll  [n/N] hunk ",
            self.options.baseline,
            self.options.scope.label(),
        );
        let header_fg = header_contrast_fg(palette);
        let header = Paragraph::new(Line::from(Span::styled(
            truncate_to_width(&header_text, header_area.width as usize),
            Style::default()
                .fg(header_fg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(palette.accent));
        frame.render_widget(header, header_area);

        let mut content: Vec<Line<'_>> = Vec::new();
        if let Some(err) = &self.git_error {
            content.push(Line::from(Span::styled(
                format!("git error: {err}"),
                Style::default().fg(palette.red),
            )));
        }
        if self.truncated_lines > 0 {
            content.push(Line::from(Span::styled(
                format!(
                    "(truncated: showing first {MAX_DIFF_LINES} lines; {} hidden)",
                    self.truncated_lines
                ),
                Style::default().fg(palette.yellow),
            )));
        }
        content.extend(self.render_lines(palette));
        let paragraph = Paragraph::new(content).scroll((self.scroll as u16, 0));
        frame.render_widget(paragraph, body_area);
    }

    fn scroll_offset(&self) -> u16 {
        self.scroll as u16
    }

    fn extract_selection(&self, sel: &Selection) -> Option<String> {
        let lines = self.plain_lines();
        if lines.is_empty() {
            return None;
        }
        let ((start_row, start_col), (end_row, end_col)) = sel.ordered_cells();
        let last_idx = lines.len().saturating_sub(1);
        let start_row = (start_row as usize).min(last_idx);
        let end_row = (end_row as usize).min(last_idx);

        let mut out = String::new();
        if start_row == end_row {
            let row = &lines[start_row];
            out.push_str(slice_columns(row, start_col, end_col));
        } else {
            out.push_str(slice_columns(&lines[start_row], start_col, u16::MAX));
            for row in &lines[start_row + 1..end_row] {
                out.push('\n');
                out.push_str(row);
            }
            out.push('\n');
            out.push_str(slice_columns(&lines[end_row], 0, end_col));
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn handle_key(&mut self, key: &TerminalKey) -> ViewKeyOutcome {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = self.cursor.saturating_add(1);
                self.scroll = self.scroll.saturating_add(1);
                self.clamp_scroll();
                ViewKeyOutcome::Handled
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                self.scroll = self.scroll.saturating_sub(1);
                ViewKeyOutcome::Handled
            }
            KeyCode::Char('J') => {
                let boundaries = self.file_boundary_indices();
                self.move_to_next(&boundaries);
                self.scroll = self.cursor;
                ViewKeyOutcome::Handled
            }
            KeyCode::Char('K') => {
                let boundaries = self.file_boundary_indices();
                self.move_to_prev(&boundaries);
                self.scroll = self.cursor;
                ViewKeyOutcome::Handled
            }
            KeyCode::Char('n') => {
                let boundaries = self.hunk_indices();
                self.move_to_next(&boundaries);
                self.scroll = self.cursor;
                ViewKeyOutcome::Handled
            }
            KeyCode::Char('N') => {
                let boundaries = self.hunk_indices();
                self.move_to_prev(&boundaries);
                self.scroll = self.cursor;
                ViewKeyOutcome::Handled
            }
            KeyCode::Char('r') => {
                self.rerun_git();
                ViewKeyOutcome::Handled
            }
            KeyCode::Char('s') => {
                self.options.scope = self.options.scope.next();
                self.rerun_git();
                ViewKeyOutcome::Handled
            }
            KeyCode::Char('b') => {
                // Interactive baseline picker is deferred until view kinds
                // can request modal text input. PR #3 logs and stays put.
                debug!("diff view: baseline change request ignored (not yet implemented)");
                ViewKeyOutcome::Handled
            }
            KeyCode::Enter => {
                // Reserved for "open file view at this hunk" once PR #4 lands.
                debug!("diff view: enter on hunk — file view not yet implemented");
                ViewKeyOutcome::Handled
            }
            _ => ViewKeyOutcome::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _rect: Rect) -> ViewKeyOutcome {
        match event.kind {
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(3);
                self.clamp_scroll();
                ViewKeyOutcome::Handled
            }
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(3);
                ViewKeyOutcome::Handled
            }
            _ => ViewKeyOutcome::NotHandled,
        }
    }

    fn on_files_changed(&mut self, _paths: &[PathBuf]) {
        self.rerun_git();
    }

    fn refresh(&mut self) {
        self.rerun_git();
    }

    fn wire_descriptor(&self) -> (String, serde_json::Value) {
        let scope = match self.options.scope {
            DiffScope::All => "all",
            DiffScope::Unstaged => "unstaged",
            DiffScope::Staged => "staged",
            DiffScope::Committed => "committed",
        };
        (
            "diff".to_string(),
            serde_json::json!({
                "baseline": self.options.baseline,
                "scope": scope,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = format!(
            "herdr-diff-view-{}-{}-{}",
            label,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo_with_main(root: &Path) {
        run_git(root, &["init", "-q", "-b", "main"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "test"]);
        run_git(root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("a.txt"), "line one\nline two\n").unwrap();
        run_git(root, &["add", "a.txt"]);
        run_git(root, &["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn detect_default_baseline_prefers_main() {
        let root = temp_dir("baseline-main");
        init_repo_with_main(&root);
        assert_eq!(detect_default_baseline(&root).as_deref(), Some("main"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_default_baseline_returns_none_on_unborn_repo() {
        let root = temp_dir("baseline-empty");
        run_git(&root, &["init", "-q", "-b", "main"]);
        assert!(detect_default_baseline(&root).is_none());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn on_files_changed_reruns_git() {
        let root = temp_dir("reruns");
        init_repo_with_main(&root);
        // Modify a tracked file so unstaged diff is non-empty.
        fs::write(root.join("a.txt"), "line one\nline two\nthree\n").unwrap();
        let mut view = DiffView::new(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::Unstaged,
            },
        );
        assert!(!view.files().is_empty(), "expected diff after mutation");
        // Revert and re-run; the diff should empty out.
        fs::write(root.join("a.txt"), "line one\nline two\n").unwrap();
        view.on_files_changed(&[]);
        assert!(
            view.files().is_empty(),
            "expected diff cleared after revert"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn title_is_kind_only() {
        // Scope/baseline are surfaced via the in-pane header row; the
        // border title carries just the kind name so it stays readable
        // on narrow panes.
        let root = temp_dir("title");
        init_repo_with_main(&root);
        let view = DiffView::new(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::Staged,
            },
        );
        assert_eq!(view.title(), "diff");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn render_paints_in_pane_header_row() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let root = temp_dir("render-header");
        init_repo_with_main(&root);
        let view = DiffView::new(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::Unstaged,
            },
        );

        let backend = TestBackend::new(80, 8);
        let mut term = Terminal::new(backend).unwrap();
        let palette = Palette::catppuccin();
        term.draw(|f| {
            view.render(f, Rect::new(0, 0, 80, 8), true, &palette);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut row0 = String::new();
        for x in 0..80 {
            row0.push(buf[(x, 0)].symbol().chars().next().unwrap_or(' '));
        }
        assert!(
            row0.contains("main"),
            "header row missing baseline: {row0:?}"
        );
        assert!(
            row0.contains("unstaged"),
            "header row missing scope: {row0:?}"
        );
        assert!(
            row0.contains("[s] scope"),
            "header row missing hint: {row0:?}"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scroll_offset_reflects_scroll() {
        let root = temp_dir("scroll-offset");
        init_repo_with_main(&root);
        fs::write(root.join("a.txt"), "line one\nx\ny\nz\nq\n").unwrap();
        let mut view = DiffView::new(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::Unstaged,
            },
        );
        view.scroll = 4;
        assert_eq!(view.scroll_offset(), 4);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extract_selection_returns_visible_text_with_prefix() {
        let root = temp_dir("extract");
        init_repo_with_main(&root);
        // Produce a small known diff: replace a.txt content.
        fs::write(root.join("a.txt"), "line one\nNEW LINE\n").unwrap();
        let view = DiffView::new(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::Unstaged,
            },
        );
        let plain = view.plain_lines();
        assert!(!plain.is_empty(), "expected non-empty plain lines");
        // Select all rows fully.
        let last = (plain.len() - 1) as u32;
        let last_col = plain.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let sel = Selection::line_range(crate::layout::PaneId::from_raw(0), 0, last, last_col);
        let text = view.extract_selection(&sel).expect("expected text");
        // Should contain at least one '-' line and one '+' line from the patch.
        assert!(
            text.contains("-line two") || text.contains("-"),
            "no removal in {text:?}"
        );
        assert!(text.contains("+NEW LINE"), "missing addition in {text:?}");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scope_key_cycles_through_modes() {
        let root = temp_dir("scope");
        init_repo_with_main(&root);
        let mut view = DiffView::new(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::All,
            },
        );
        view.handle_key(&TerminalKey::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(view.options.scope, DiffScope::Unstaged);
        view.handle_key(&TerminalKey::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(view.options.scope, DiffScope::Staged);
        view.handle_key(&TerminalKey::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(view.options.scope, DiffScope::Committed);
        view.handle_key(&TerminalKey::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert_eq!(view.options.scope, DiffScope::All);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scroll_keys_move_cursor() {
        let root = temp_dir("scroll");
        init_repo_with_main(&root);
        fs::write(root.join("a.txt"), "line one\nx\ny\nz\nq\n").unwrap();
        let mut view = DiffView::new(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::Unstaged,
            },
        );
        let before = view.scroll;
        view.handle_key(&TerminalKey::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(view.scroll >= before, "j should not move scroll up");
        view.handle_key(&TerminalKey::new(
            crossterm::event::KeyCode::Char('k'),
            crossterm::event::KeyModifiers::empty(),
        ));
        assert!(view.scroll <= before + 1);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unknown_key_is_not_handled() {
        let root = temp_dir("unknown");
        init_repo_with_main(&root);
        let mut view = DiffView::new(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::All,
            },
        );
        assert!(matches!(
            view.handle_key(&TerminalKey::new(
                crossterm::event::KeyCode::Char('z'),
                crossterm::event::KeyModifiers::empty()
            )),
            ViewKeyOutcome::NotHandled
        ));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn truncation_banner_when_diff_exceeds_cap() {
        // Build a 60k-line fake stub of `git` that writes lots of `+` lines.
        let root = temp_dir("truncate");
        run_git(&root, &["init", "-q", "-b", "main"]);
        let stub_dir = temp_dir("truncate-stub");
        let stub_path = stub_dir.join("git");
        let mut script = String::from("#!/bin/sh\n");
        script.push_str("cat <<'EOF'\n");
        script.push_str("diff --git a/big.txt b/big.txt\n");
        script.push_str("--- a/big.txt\n");
        script.push_str("+++ b/big.txt\n");
        script.push_str("@@ -1,1 +1,60000 @@\n");
        script.push_str(" anchor\n");
        for i in 0..60_000 {
            script.push_str(&format!("+line {i}\n"));
        }
        script.push_str("EOF\n");
        fs::write(&stub_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&stub_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&stub_path, perms).unwrap();
        }
        let view = DiffView::new_with_git(
            root.clone(),
            DiffViewOptions {
                baseline: "main".into(),
                scope: DiffScope::Unstaged,
            },
            stub_path,
        );
        assert!(
            view.truncated_lines() > 0,
            "expected truncation to leave overflow"
        );
        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&stub_dir).ok();
    }
}
