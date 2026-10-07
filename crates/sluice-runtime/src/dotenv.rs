//! `.env` files (SPEC §5.4): `<home>/.env`, then `<home>/projects/<id>/.env`, layered over the
//! environment a run inherits, with the run's own `SLUICE_*` variables set after them. Values
//! are secrets: nothing here logs or records them.
use sluice_model::ids::ProjectId;
use std::{collections::BTreeMap, path::Path};

/// The `KEY=value` lines of a .env file: blank lines and `#` comments are skipped, an
/// `export ` prefix is dropped and one pair of matching quotes around a value is removed.
/// Returns the values and the 1-based numbers of the lines that are none of these.
pub fn parse(text: &str) -> (BTreeMap<String, String>, Vec<usize>) {
    let mut values = BTreeMap::new();
    let mut bad = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            bad.push(i + 1);
            continue;
        };
        let key = key.trim();
        let valid = !key.is_empty()
            && key.bytes().enumerate().all(|(i, b)| {
                b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
            });
        if !valid {
            bad.push(i + 1);
            continue;
        }
        let value = value.trim();
        let bytes = value.as_bytes();
        let quoted = bytes.len() >= 2
            && (bytes[0] == b'"' || bytes[0] == b'\'')
            && bytes[bytes.len() - 1] == bytes[0];
        let unquoted = if quoted {
            &value[1..value.len() - 1]
        } else {
            value
        };
        values.insert(key.to_owned(), unquoted.to_owned());
    }
    (values, bad)
}

/// The values of one .env file: none when it does not exist or cannot be read; malformed
/// lines are skipped (`verify` reports them).
pub fn read(path: &Path) -> BTreeMap<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).0,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path=%path.display(), error=%e, "cannot read .env");
            }
            BTreeMap::new()
        }
    }
}

/// What a run in `project` (or a home-level call, without one) adds to its environment: the
/// home's .env, then the project's, a later file winning.
pub fn run_environment(home: &Path, project: Option<ProjectId>) -> BTreeMap<String, String> {
    let mut values = read(&home.join(".env"));
    if let Some(project) = project {
        values.extend(read(
            &home.join("projects").join(project.to_string()).join(".env"),
        ));
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_exports_quotes_and_reports_bad_lines() {
        let (values, bad) = parse(
            "# comment\n\nTOKEN=abc\nexport QUOTED=\"two words\"\nSINGLE='x'\nEMPTY=\nnot a line\n1BAD=x\nEQ=a=b\n",
        );
        assert_eq!(values["TOKEN"], "abc");
        assert_eq!(values["QUOTED"], "two words");
        assert_eq!(values["SINGLE"], "x");
        assert_eq!(values["EMPTY"], "");
        assert_eq!(values["EQ"], "a=b");
        assert_eq!(bad, vec![7, 8]);
    }
}
