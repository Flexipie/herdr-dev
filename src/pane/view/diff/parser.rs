/// One file's worth of diff: the path, an optional rename source, whether
/// it's binary, and the list of hunks. Binary entries carry no hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub hunks: Vec<DiffHunk>,
    pub binary: bool,
    pub rename_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_range: (u32, u32),
    pub new_range: (u32, u32),
    pub heading: String,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// Parse the output of `git diff` (unified format). Tolerant of malformed
/// input — unrecognized lines are dropped rather than panicking.
pub fn parse_unified_diff(raw: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut current_file: Option<DiffFile> = None;
    let mut current_hunk: Option<DiffHunk> = None;

    let flush_hunk = |file: &mut DiffFile, hunk: &mut Option<DiffHunk>| {
        if let Some(h) = hunk.take() {
            file.hunks.push(h);
        }
    };

    for line in raw.split('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(mut file) = current_file.take() {
                flush_hunk(&mut file, &mut current_hunk);
                files.push(file);
            }
            let path = parse_diff_git_header(rest).unwrap_or_else(|| rest.to_string());
            current_file = Some(DiffFile {
                path,
                hunks: Vec::new(),
                binary: false,
                rename_from: None,
            });
            continue;
        }

        let Some(file) = current_file.as_mut() else {
            continue;
        };

        if let Some(rest) = line.strip_prefix("rename from ") {
            file.rename_from = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename to ") {
            file.path = rest.to_string();
            continue;
        }
        if line.starts_with("Binary files ") && line.ends_with(" differ") {
            flush_hunk(file, &mut current_hunk);
            file.binary = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ b/") {
            // Trust the b/ side as the authoritative path when present.
            file.path = rest.to_string();
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if line.starts_with("index ")
            || line.starts_with("new file mode ")
            || line.starts_with("deleted file mode ")
            || line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || line.starts_with("similarity index ")
            || line.starts_with("dissimilarity index ")
            || line.starts_with("copy from ")
            || line.starts_with("copy to ")
        {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@") {
            flush_hunk(file, &mut current_hunk);
            if let Some(hunk) = parse_hunk_header(rest) {
                current_hunk = Some(hunk);
            }
            continue;
        }

        let Some(hunk) = current_hunk.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix('+') {
            hunk.lines.push(HunkLine::Added(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            hunk.lines.push(HunkLine::Removed(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            hunk.lines.push(HunkLine::Context(rest.to_string()));
        }
        // Other lines (e.g. `\ No newline at end of file`, trailing empty
        // line from split('\n'), or any malformed input) are silently
        // dropped — the parser is intentionally tolerant.
    }

    if let Some(mut file) = current_file.take() {
        flush_hunk(&mut file, &mut current_hunk);
        files.push(file);
    }

    files
}

/// Extract the `b/<path>` side of `diff --git a/X b/Y` as our file path.
/// Returns `None` if the shape doesn't match — caller falls back to the
/// raw header.
fn parse_diff_git_header(rest: &str) -> Option<String> {
    let (_, b_part) = rest.split_once(" b/")?;
    Some(b_part.to_string())
}

/// Parse the tail of a hunk header, e.g. ` -1,5 +1,7 @@ heading`.
fn parse_hunk_header(rest: &str) -> Option<DiffHunk> {
    let rest = rest.trim_start();
    let (old_part, after_old) = rest.strip_prefix('-')?.split_once(' ')?;
    let after_old = after_old.trim_start();
    let (new_part, after_new) = after_old.strip_prefix('+')?.split_once(' ')?;
    let after_new = after_new.trim_start();
    let heading = after_new.strip_prefix("@@").unwrap_or(after_new);
    let heading = heading.trim_start().to_string();
    Some(DiffHunk {
        old_range: parse_range(old_part)?,
        new_range: parse_range(new_part)?,
        heading,
        lines: Vec::new(),
    })
}

fn parse_range(part: &str) -> Option<(u32, u32)> {
    if let Some((start, count)) = part.split_once(',') {
        Some((start.parse().ok()?, count.parse().ok()?))
    } else {
        Some((part.parse().ok()?, 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff_yields_no_files() {
        assert!(parse_unified_diff("").is_empty());
        assert!(parse_unified_diff("\n\n").is_empty());
    }

    #[test]
    fn single_file_single_hunk() {
        let raw = "\
diff --git a/foo.txt b/foo.txt
index 0000001..0000002 100644
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,4 @@
 context one
-removed line
+added line
+another added
 context two
";
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "foo.txt");
        assert!(!f.binary);
        assert!(f.rename_from.is_none());
        assert_eq!(f.hunks.len(), 1);
        let h = &f.hunks[0];
        assert_eq!(h.old_range, (1, 3));
        assert_eq!(h.new_range, (1, 4));
        assert_eq!(h.lines.len(), 5);
        assert_eq!(h.lines[0], HunkLine::Context("context one".into()));
        assert_eq!(h.lines[1], HunkLine::Removed("removed line".into()));
        assert_eq!(h.lines[2], HunkLine::Added("added line".into()));
        assert_eq!(h.lines[3], HunkLine::Added("another added".into()));
        assert_eq!(h.lines[4], HunkLine::Context("context two".into()));
    }

    #[test]
    fn multiple_files() {
        let raw = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old
+new
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-foo
+bar
";
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.rs");
        assert_eq!(files[1].path, "b.rs");
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[1].hunks.len(), 1);
    }

    #[test]
    fn rename_header_detected() {
        let raw = "\
diff --git a/old.txt b/new.txt
similarity index 95%
rename from old.txt
rename to new.txt
";
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rename_from.as_deref(), Some("old.txt"));
        assert_eq!(files[0].path, "new.txt");
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn binary_marker_sets_binary_flag() {
        let raw = "\
diff --git a/img.png b/img.png
index 0..1
Binary files a/img.png and b/img.png differ
";
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        assert!(files[0].binary);
        assert!(files[0].hunks.is_empty());
        assert_eq!(files[0].path, "img.png");
    }

    #[test]
    fn malformed_input_does_not_panic() {
        let raw = "\
diff --git a/foo.txt b/foo.txt
@@ -1,3 +1,4 @@
 ctx
+";
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks.len(), 1);
    }

    #[test]
    fn hunk_heading_captured() {
        let raw = "\
diff --git a/lib.rs b/lib.rs
--- a/lib.rs
+++ b/lib.rs
@@ -10,1 +10,2 @@ fn outer()
 inner
+added
";
        let files = parse_unified_diff(raw);
        assert_eq!(files[0].hunks[0].heading, "fn outer()");
    }

    #[test]
    fn single_value_range_treated_as_count_one() {
        let raw = "\
diff --git a/x b/x
@@ -5 +5 @@
-a
+b
";
        let files = parse_unified_diff(raw);
        assert_eq!(files[0].hunks[0].old_range, (5, 1));
        assert_eq!(files[0].hunks[0].new_range, (5, 1));
    }
}
