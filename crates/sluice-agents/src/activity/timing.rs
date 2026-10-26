//! Wall time partitions use the union of transcript intervals. Concurrent tools never
//! double-count the run's wall time; command rankings sum individual command durations.
use super::{Engine, Homes, Window, outline};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommandTime {
    pub command: String,
    pub count: usize,
    pub seconds: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TimeSplit {
    pub model_s: f64,
    pub commands_s: f64,
    pub tools_s: f64,
    pub idle_s: f64,
    pub top_commands: Vec<CommandTime>,
}
impl TimeSplit {
    /// One short sentence for a running row. Detailed seconds remain in step_get.
    pub fn compact(&self) -> String {
        let total = self.model_s + self.commands_s + self.tools_s + self.idle_s;
        let percent = |s: f64| if total > 0.0 { s * 100.0 / total } else { 0.0 };
        let commands = self
            .top_commands
            .iter()
            .take(2)
            .map(|c| format!("{} {}", c.command, duration(c.seconds)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "model {:.0}% / commands {:.0}%{}{} / idle {:.0}%",
            percent(self.model_s),
            percent(self.commands_s),
            if commands.is_empty() {
                String::new()
            } else {
                format!(" ({commands})")
            },
            if self.tools_s > 0.0 {
                format!(" / tools {:.0}%", percent(self.tools_s))
            } else {
                String::new()
            },
            percent(self.idle_s)
        )
    }
}
fn duration(seconds: f64) -> String {
    if seconds >= 60.0 {
        format!("{:.0}m", seconds / 60.0)
    } else {
        format!("{seconds:.0}s")
    }
}

/// Read through the activity cache: unchanged transcripts read no bytes and growing ones
/// read only their suffix. `now_ms` is explicit so a live run and a finished run share the
/// same accounting. Missing, malformed or untimed transcripts have no time split.
pub fn time_split(homes: &Homes, run: &str, window: Window, now_ms: u64) -> Option<TimeSplit> {
    let outline = outline(homes, run, window)?;
    if !matches!(outline.engine, Engine::Codex | Engine::Claude) || outline.skipped > 0 {
        return None;
    }
    let end = window.until_ms.unwrap_or(now_ms).min(now_ms);
    let start = window.since_ms;
    if end < start || outline.turns.is_empty() {
        return None;
    }
    // Boundaries carry counts for active turns, commands and other tools. Commands take
    // precedence over other tools where their intervals overlap.
    let mut edges = vec![(start, [0i64; 3]), (end, [0; 3])];
    let mut add = |from: u64, through: u64, kind: usize| {
        let (a, b) = (from.max(start), through.min(end));
        if b > a {
            let mut plus = [0; 3];
            plus[kind] = 1;
            let mut minus = [0; 3];
            minus[kind] = -1;
            edges.push((a, plus));
            edges.push((b, minus));
        }
    };
    let mut commands = BTreeMap::<String, (usize, u64)>::new();
    for turn in &outline.turns {
        let a = turn.started_ms?;
        let b = if turn.closed { turn.ended_ms? } else { end };
        add(a, b, 0);
        for call in &turn.calls {
            let a = call.started_ms?;
            let b = call.ended_ms.unwrap_or(if turn.closed { b } else { end });
            let command = call
                .args
                .iter()
                .find(|(k, _)| matches!(k.as_str(), "command" | "cmd"))
                .map(|(_, v)| v.as_str());
            add(a, b, if command.is_some() { 1 } else { 2 });
            let samples = if call.command_times.is_empty() {
                command
                    .map(|c| (c.to_owned(), Some(b.saturating_sub(a) as f64 / 1000.0)))
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                call.command_times.clone()
            };
            for (command, seconds) in samples {
                let seconds = seconds.or_else(|| {
                    (call.command_times.len() == 1).then_some(b.saturating_sub(a) as f64 / 1000.0)
                });
                let Some(seconds) = seconds else {
                    continue;
                };
                let name = normalise(&command);
                if !name.is_empty() && b > start && a < end {
                    let entry = commands.entry(name).or_default();
                    entry.0 += 1;
                    entry.1 += (seconds
                        .min(b.min(end).saturating_sub(a.max(start)) as f64 / 1000.0)
                        * 1000.0) as u64;
                }
            }
        }
    }
    edges.sort_unstable_by_key(|(at, _)| *at);
    let mut last = start;
    let mut counts = [0i64; 3];
    let mut millis = [0u64; 4];
    for (at, delta) in edges {
        let kind = if counts[1] > 0 {
            1
        } else if counts[2] > 0 {
            2
        } else if counts[0] > 0 {
            0
        } else {
            3
        };
        millis[kind] += at.saturating_sub(last);
        for i in 0..3 {
            counts[i] += delta[i];
        }
        last = at;
    }
    let mut top_commands: Vec<_> = commands
        .into_iter()
        .map(|(command, (count, ms))| CommandTime {
            command,
            count,
            seconds: ms as f64 / 1000.0,
        })
        .collect();
    top_commands.sort_by(|a, b| {
        b.seconds
            .total_cmp(&a.seconds)
            .then(a.command.cmp(&b.command))
    });
    top_commands.truncate(5);
    Some(TimeSplit {
        model_s: millis[0] as f64 / 1000.0,
        commands_s: millis[1] as f64 / 1000.0,
        tools_s: millis[2] as f64 / 1000.0,
        idle_s: millis[3] as f64 / 1000.0,
        top_commands,
    })
}
fn normalise(command: &str) -> String {
    command
        .split_whitespace()
        .skip_while(|w| w.contains('=') || matches!(*w, "env" | "sudo"))
        .take(2)
        .map(|w| w.trim_matches(['\'', '"']))
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(48)
        .collect()
}

/// Native Codex can wrap a command in a JavaScript exec tool. Read the literal command
/// argument, never execute it. A wrapper without a literal command stays an other tool.
pub(super) fn wrapped_commands(tool: &str, input: &Value) -> Vec<String> {
    if !matches!(tool, "functions.exec" | "exec") {
        return vec![];
    }
    let Some(text) = input.as_str() else {
        return vec![];
    };
    if !text.contains("exec_command") {
        return vec![];
    }
    let mut found = vec![];
    for key in ["cmd:", "cmd :", "\"cmd\":"] {
        let mut rest = text;
        while let Some((_, tail)) = rest.split_once(key) {
            let tail = tail.trim_start();
            let Some(quote) = tail.chars().next() else {
                break;
            };
            rest = tail;
            if !matches!(quote, '\'' | '"' | '`') {
                continue;
            }
            let mut escaped = false;
            let mut command = String::new();
            for (i, c) in tail[1..].char_indices() {
                if escaped {
                    command.push(match c {
                        'n' | 'r' | 't' => ' ',
                        c => c,
                    });
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == quote {
                    found.push(command);
                    rest = &tail[i + 2..];
                    break;
                } else {
                    command.push(c);
                }
            }
            if rest == tail {
                break;
            }
        }
    }
    found
}
/// Native exec wrappers retain each shell tool's measured wall time as JSON output.
/// Match all command results before assigning them: a partially printed batch is unknown.
pub(super) fn command_durations(result: &str) -> Vec<f64> {
    result
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| v["wall_time_seconds"].as_f64())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .collect()
}

/// The shell API returns a durable session identifier when it yields. Native exec wrappers
/// preserve the structured tool result as a JSON text block in their transcript output.
pub(super) fn result_session(output: &Value) -> Option<String> {
    if let Some(id) = output.get("session_id").filter(|v| !v.is_null()) {
        return Some(
            id.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| id.to_string()),
        );
    }
    if let Some(parts) = output.as_array() {
        return parts.iter().find_map(|p| result_session(&p["text"]));
    }
    if let Some(text) = output.as_str() {
        if let Ok(value) = serde_json::from_str::<Value>(text) {
            return if value.is_string() {
                None
            } else {
                result_session(&value)
            };
        }
        if let Some((_, rest)) = text.split_once("Process running with session ID ") {
            return rest.split_whitespace().next().map(str::to_owned);
        }
    }
    None
}
pub(super) fn poll_session(tool: &str, input: &Value) -> Option<String> {
    if tool.ends_with("write_stdin") {
        return input.get("session_id").map(|id| {
            id.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| id.to_string())
        });
    }
    if !matches!(tool, "exec" | "functions.exec") {
        return None;
    }
    let text = input.as_str()?;
    if !text.contains("write_stdin") {
        return None;
    }
    for key in ["session_id:", "\"session_id\":"] {
        if let Some((_, tail)) = text.split_once(key) {
            let id: String = tail
                .trim_start()
                .trim_start_matches('\"')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}
