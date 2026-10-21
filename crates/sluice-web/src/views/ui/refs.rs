//! References (DESIGN.md, Hover cards): a ticket (`FIG-7047`), a commit's sha or a step's id in
//! words a page draws is marked `data-ref`, and pointing at it (or focusing it) opens the one
//! hover card after 320ms, filled from its project's `ref` route; it hides 160ms after the
//! pointer leaves both (`components.js`, `refs`).
use super::esc;

/// A step's id as a reference: in mono, its card its state, its fn and its unit.
pub fn step_ref(step: &str) -> String {
    format!(
        "<code class=\"ref\" data-ref=\"step:{s}\" tabindex=\"0\">{s}</code>",
        s = esc(step)
    )
}
/// A commit's sha as a reference, cut to ten characters: its card the steps that name it.
pub fn sha_ref(sha: &str) -> String {
    format!(
        "<code class=\"ref\" data-ref=\"sha:{}\" tabindex=\"0\">{}</code>",
        esc(sha),
        esc(&sha[..sha.len().min(10)])
    )
}
/// Whether `text` is a commit's sha: 7 to 40 hex digits, at least one a letter and one a digit
/// (a word of all letters is a word).
pub fn is_sha(text: &str) -> bool {
    (7..=40).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && text.bytes().any(|b| b.is_ascii_digit())
        && text.bytes().any(|b| b.is_ascii_alphabetic())
}
/// Whether `word` is a ticket's id: two to ten capitals (or digits after the first), a hyphen
/// and its number ("FIG-7047").
fn is_ticket(word: &str) -> bool {
    let Some((team, number)) = word.split_once('-') else {
        return false;
    };
    (2..=10).contains(&team.len())
        && team.starts_with(|c: char| c.is_ascii_uppercase())
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && (1..=7).contains(&number.len())
        && number.bytes().all(|b| b.is_ascii_digit())
}
/// `html` (already escaped and drawn) with each ticket and sha in its text marked as a
/// reference: a `<code>` holding a sha wears the mark itself; a ticket or a bare sha in words is
/// wrapped. Never inside a tag, a link or a block of code.
pub fn refs(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    // how deep inside a link or a `pre`: their words stay as they are
    let mut inside = 0usize;
    while !rest.is_empty() {
        if let Some(tag_end) = rest.strip_prefix('<').and_then(|r| r.find('>')) {
            let tag = &rest[..tag_end + 2];
            let name = tag[1..]
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            let closing = tag.starts_with("</");
            if matches!(name.as_str(), "a" | "pre") {
                if closing {
                    inside = inside.saturating_sub(1);
                } else {
                    inside += 1;
                }
            }
            // a `<code>` whose whole text is a sha
            if name == "code" && !closing && inside == 0 && !tag.contains("class=") {
                let after = &rest[tag.len()..];
                if let Some(end) = after.find("</code>")
                    && is_sha(&after[..end])
                {
                    out.push_str(&sha_ref(&after[..end]));
                    rest = &after[end + "</code>".len()..];
                    continue;
                }
            }
            out.push_str(tag);
            rest = &rest[tag.len()..];
            continue;
        }
        // the text up to the next tag
        let end = rest.find('<').unwrap_or(rest.len());
        let (text, after) = rest.split_at(end.max(1).min(rest.len()));
        if inside > 0 {
            out.push_str(text);
        } else {
            mark_words(text, &mut out);
        }
        rest = after;
    }
    out
}
/// `text`'s words, each ticket or sha among them marked.
fn mark_words(text: &str, out: &mut String) {
    let mut word_start: Option<usize> = None;
    let mut last = 0;
    let wordy = |c: char| c.is_ascii_alphanumeric() || c == '-';
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    for (k, &(i, c)) in bytes.iter().enumerate() {
        if wordy(c) {
            word_start.get_or_insert(i);
        }
        let ends = !wordy(c) || k + 1 == bytes.len();
        if let (true, Some(start)) = (ends, word_start) {
            let stop = if wordy(c) { i + c.len_utf8() } else { i };
            // a hyphen ending a sentence's word is not the word's
            let word = text[start..stop].trim_end_matches('-');
            let stop = start + word.len();
            if is_ticket(word) {
                out.push_str(&text[last..start]);
                out.push_str(&format!(
                    "<span class=\"ref\" data-ref=\"t:{word}\" tabindex=\"0\">{word}</span>"
                ));
                last = stop;
            } else if is_sha(word) {
                out.push_str(&text[last..start]);
                out.push_str(&sha_ref(word));
                last = stop;
            }
            word_start = None;
        }
    }
    out.push_str(&text[last..]);
}
