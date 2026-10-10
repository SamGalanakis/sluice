//! `cutover-rehearsal rehearse --home <copy> --deadline <time> [--sha <sha>] [--report <file>]`
//! plays the cutover's deadline on a private copy with this (old) release's code and prints
//! the cutover report as JSON, one `stopped …` line per run on stderr; it exits 0 at zero
//! blockers with no refused cancel, 1 otherwise, 2 on an error.
//!
//! `cutover-rehearsal oracle --home <copy> [--out <file>]` prints every plan's documents,
//! revision by revision, replayed with this release's patch semantics, and what would block
//! converting each (`{"projects": [...]}`).

use cutover_rehearsal::{legacy, rehearse, report};
use std::{path::PathBuf, process::ExitCode};

fn usage() -> ExitCode {
    eprintln!(
        "usage: cutover-rehearsal rehearse --home <copy> --deadline <time> [--sha <sha>] [--report <file>]\n       cutover-rehearsal oracle --home <copy> [--out <file>]"
    );
    ExitCode::from(2)
}

fn flags(args: &[String]) -> Option<std::collections::BTreeMap<String, String>> {
    let mut out = std::collections::BTreeMap::new();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        out.insert(flag.strip_prefix("--")?.to_owned(), args.next()?.clone());
    }
    Some(out)
}

fn write(out: Option<&String>, text: &str) -> std::io::Result<()> {
    match out {
        Some(path) => std::fs::write(path, text),
        None => {
            println!("{text}");
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((mode, rest)) = args.split_first() else {
        return usage();
    };
    let Some(flags) = flags(rest) else {
        return usage();
    };
    let Some(home) = flags.get("home").map(PathBuf::from) else {
        return usage();
    };
    match mode.as_str() {
        "rehearse" => {
            let Some(deadline) = flags.get("deadline").cloned() else {
                return usage();
            };
            let options = rehearse::Options {
                deadline,
                sha: flags.get("sha").cloned().unwrap_or_default(),
            };
            // The catalog the old coordinator builds for a home (`client::run_home`).
            let catalog = if std::env::var_os("SLUICE_FIXTURE").is_some() {
                sluice_runtime::dispatch::Catalog::fixtures()
            } else {
                sluice_runtime::dispatch::Catalog::core()
            };
            let runtime = match sluice_runtime::coordinator::executor() {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("cutover-rehearsal: {error}");
                    return ExitCode::from(2);
                }
            };
            match runtime.block_on(rehearse::rehearse(&home, catalog, &options)) {
                Ok((rehearsal, _)) => {
                    for run in &rehearsal.report.stopped {
                        eprintln!("{}", report::line(run));
                    }
                    for refusal in &rehearsal.report.refused {
                        eprintln!(
                            "refused {} {}: {}",
                            refusal.project, refusal.step, refusal.error
                        );
                    }
                    for blocker in &rehearsal.blockers {
                        eprintln!(
                            "blocker {} {} {}",
                            blocker.kind,
                            blocker.identity,
                            blocker.project.as_deref().unwrap_or("-")
                        );
                    }
                    let text = serde_json::to_string_pretty(&rehearsal.report)
                        .expect("the report serializes");
                    if let Err(error) = write(flags.get("report"), &text) {
                        eprintln!("cutover-rehearsal: {error}");
                        return ExitCode::from(2);
                    }
                    if rehearsal.ok() {
                        ExitCode::SUCCESS
                    } else {
                        eprintln!(
                            "rehearsal failed: {} cancels refused, {} blockers left",
                            rehearsal.report.refused.len(),
                            rehearsal.blockers.len()
                        );
                        ExitCode::from(1)
                    }
                }
                Err(error) => {
                    eprintln!("cutover-rehearsal: {error}");
                    ExitCode::from(2)
                }
            }
        }
        "oracle" => {
            let database = home.join("sluice.db");
            let replayed = rusqlite::Connection::open_with_flags(
                &database,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .and_then(|sql| legacy::replay(&sql));
            match replayed {
                Ok(projects) => {
                    let text =
                        serde_json::to_string_pretty(&serde_json::json!({"projects": projects}))
                            .expect("the oracle serializes");
                    match write(flags.get("out"), &text) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("cutover-rehearsal: {error}");
                            ExitCode::from(2)
                        }
                    }
                }
                Err(error) => {
                    eprintln!("cutover-rehearsal: {}: {error}", database.display());
                    ExitCode::from(2)
                }
            }
        }
        _ => usage(),
    }
}
