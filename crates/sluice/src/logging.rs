//! Log lines of the long-running modes, to stderr: the user journal for their units.
//!
//! Only `coordinator`, `serve`, `loop` and `guardian` log; every other mode (the CLI, the
//! agent payload, a fn run) writes nothing here, so what a run captures is unchanged. The
//! level is info unless `SLUICE_LOG` (EnvFilter syntax, e.g. `warn` or
//! `info,sluice_store=debug`) says otherwise.
use tracing_subscriber::EnvFilter;

/// The modes that log, by their registered names.
pub const LOGGED_MODES: &[&str] = &["coordinator", "serve", "loop", "guardian"];

/// Whether `mode` logs.
pub fn logs(mode: &str) -> bool {
    LOGGED_MODES.contains(&mode)
}

/// Install the stderr subscriber for `mode`, if it logs, and say what started. Returns
/// whether it was installed.
pub fn init(mode: &str, home: &std::path::Path) -> bool {
    if !logs(mode) {
        return false;
    }
    let configured = std::env::var("SLUICE_LOG").ok();
    let (filter, refused) = match configured.as_deref().map(EnvFilter::try_new) {
        None => (EnvFilter::new("info"), None),
        Some(Ok(filter)) => (filter, None),
        Some(Err(error)) => (EnvFilter::new("info"), Some(error.to_string())),
    };
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false);
    // The journal stamps every line itself; elsewhere (a fixture deploy's log file, a
    // terminal) the line carries its own time.
    let installed = if std::env::var_os("JOURNAL_STREAM").is_some() {
        builder.without_time().try_init().is_ok()
    } else {
        builder.try_init().is_ok()
    };
    if !installed {
        return false;
    }
    if let Some(error) = refused {
        tracing::warn!(value = configured.as_deref().unwrap_or_default(), %error, "SLUICE_LOG ignored; logging at info");
    }
    tracing::info!(
        mode,
        release = %sluice_runtime::install::release_id("working-tree"),
        home = %home.display(),
        pid = std::process::id(),
        "sluice started"
    );
    true
}
