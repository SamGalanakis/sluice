//! The board's document (`docs("board")`): the hand-written part of a project's board, one
//! markdown text a board's `Doc()` draws, read with line numbers and edited by line ranges.

use crate::commands::DocEdit;

/// The text with `\r\n` line ends as `\n`, as it is stored.
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The text's lines, as `str::lines` counts them (a last line end adds no line).
pub fn line_count(text: &str) -> usize {
    text.lines().count()
}

/// The text with right-aligned line numbers and a tab before each line, as `cat -n` prints
/// it: the numbers `board_doc_edit` takes.
pub fn numbered(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8 * line_count(text));
    for (n, line) in text.lines().enumerate() {
        out.push_str(&format!("{:>6}\t{line}\n", n + 1));
    }
    out
}

/// `text` with `edits` applied: each replaces lines `start..=end` (1-based, of `text`) with
/// its own lines; `end = start - 1` inserts before `start`, `start = lines + 1` appends. The
/// edits are applied bottom-up, so every number refers to `text`. A range outside the text,
/// two edits that overlap (or insert at the same place) or no edit at all is refused, naming
/// the edit, and nothing is applied. A last line end is kept.
pub fn apply_edits(text: &str, edits: &[DocEdit]) -> Result<String, String> {
    if edits.is_empty() {
        return Err("edits is empty: give at least one {start, end, text}".into());
    }
    let lines: Vec<&str> = text.lines().collect();
    let count = lines.len() as u64;
    let name = |i: usize, e: &DocEdit| format!("edits[{i}] (start {}, end {})", e.start, e.end);
    for (i, e) in edits.iter().enumerate() {
        let problem = if e.start == 0 {
            Some("lines count from 1".to_owned())
        } else if e.start > count + 1 {
            Some(format!(
                "start is past the end: the document has {count} line{}, so start is at most {}",
                if count == 1 { "" } else { "s" },
                count + 1
            ))
        } else if e.end + 1 < e.start {
            Some("end is before start - 1 (end = start - 1 inserts before start)".to_owned())
        } else if e.end > count {
            Some(format!("end is past the last line ({count})"))
        } else {
            None
        };
        if let Some(problem) = problem {
            return Err(format!("{}: {problem}", name(i, e)));
        }
    }
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|&i| (edits[i].start, edits[i].end));
    for pair in order.windows(2) {
        let (a, b) = (&edits[pair[0]], &edits[pair[1]]);
        let both_insert = a.end + 1 == a.start && b.end + 1 == b.start;
        if b.start <= a.end || (both_insert && a.start == b.start) {
            return Err(format!(
                "{} overlaps {}: edits must not overlap",
                name(pair[1], b),
                name(pair[0], a)
            ));
        }
    }
    let mut out: Vec<&str> = lines.clone();
    // bottom-up: a later range first, and at one start the replacement before the insert
    for &i in order.iter().rev() {
        let e = &edits[i];
        let from = (e.start - 1) as usize;
        let to = e.end as usize;
        out.splice(from..to.max(from), e.text.lines());
    }
    let mut joined = out.join("\n");
    if !joined.is_empty() && (text.ends_with('\n') || text.is_empty()) {
        joined.push('\n');
    }
    Ok(joined)
}
