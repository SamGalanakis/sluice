#[path = "../../../tests/support/mod.rs"]
mod support;

use sluice_model::{error::PublicError, rpc::FnInvocation};
use sluice_process::host::{guard_home, resolve_path};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};
use support::{
    clock::{Clock, ManualClock, SystemClock},
    free_port::free_port,
    home::ScratchHome,
};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home)
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn registered_modes_cover_cli_and_pending_modes_leave_home_untouched() {
    let home = ScratchHome::new().unwrap();
    use clap::CommandFactory;
    use sluice::modes::{MODES, PENDING_MODES};
    let mut registered = MODES
        .iter()
        .map(|m| m.name.split_whitespace().next().unwrap())
        .chain(PENDING_MODES.iter().map(|m| m.name))
        .collect::<Vec<_>>();
    registered.sort_unstable();
    let mut cli_modes = sluice::cli::Cli::command()
        .get_subcommands()
        .map(|m| m.get_name().to_owned())
        .collect::<Vec<_>>();
    cli_modes.sort_unstable();
    assert_eq!(registered, cli_modes);
    for pending in PENDING_MODES {
        let args = pending.args;
        let output = run(home.path(), args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let error: PublicError = serde_json::from_slice(&output.stderr).unwrap();
        assert!(
            error.to_string().contains("not implemented in this build"),
            "{args:?}"
        );
        assert!(output.stdout.is_empty());
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }
    assert_eq!(std::fs::read_dir(home.root()).unwrap().count(), 1);
}
#[test]
fn cli_parsing_rejects_bad_arguments_and_lists_modes() {
    let home = ScratchHome::new().unwrap();
    for args in [
        vec!["serve", "--port", "invalid"],
        vec!["guardian"],
        vec!["agent", "hook", "--engine", "unknown", "--event", "Stop"],
    ] {
        assert_eq!(run(home.path(), &args).status.code(), Some(2));
    }
    let help = run(home.path(), &["--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("payload-exec")
    );
}
/// A scratch account whose live installation selects a scratch "live" home.
fn live_account(root: &Path) -> (PathBuf, PathBuf) {
    let account = root.join("account");
    let live = root.join("live");
    let install = account.join(".local/share/sluice/install");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir(&live).unwrap();
    std::fs::write(
        install.join("selection.json"),
        serde_json::json!({"generation":1,"release_path":root.join("release"),"home_path":live})
            .to_string(),
    )
    .unwrap();
    (account, live)
}
fn sluice_as(account: &Path, home: Option<&Path>, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sluice"));
    command
        .env("HOME", account)
        .env_remove("SLUICE_INSTALL_DIR")
        .args(args);
    match home {
        Some(home) => command.env("SLUICE_HOME", home),
        None => command.env_remove("SLUICE_HOME"),
    };
    command
}
const REFUSAL: &str = "refusing the live installation's selected home";
#[test]
fn test_mode_rejects_the_live_home_before_even_help_parsing() {
    let temp = tempfile::tempdir().unwrap();
    let (account, live) = live_account(temp.path());
    std::os::unix::fs::symlink(&live, temp.path().join("alias")).unwrap();
    for path in [
        live.clone(),
        live.join("runs"),
        live.join("nonexistent/../child"),
        temp.path().join("alias/child"),
    ] {
        let output = sluice_as(&account, Some(&path), &["--help"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{path:?}");
        assert!(
            String::from_utf8(output.stderr).unwrap().contains(REFUSAL),
            "{path:?}"
        );
    }
    // Without SLUICE_HOME the home is the installation's selection: refused too.
    let output = sluice_as(&account, None, &["doctor"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8(output.stderr).unwrap().contains(REFUSAL));
    // Outside test mode the selected home is the home, not a refusal.
    for home in [Some(live.as_path()), None] {
        let output = sluice_as(&account, home, &["--help"])
            .env_remove("SLUICE_TEST")
            .output()
            .unwrap();
        assert!(output.status.success(), "{home:?}: {output:?}");
    }
}
#[test]
fn without_sluice_home_or_a_selection_only_install_runs() {
    let temp = tempfile::tempdir().unwrap();
    let account = temp.path().join("account");
    std::fs::create_dir(&account).unwrap();
    let output = sluice_as(&account, None, &["doctor"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error: PublicError = serde_json::from_slice(&output.stderr).unwrap();
    assert!(error.to_string().contains("no Sluice home"), "{error}");
    let output = sluice_as(&account, None, &["install", "status"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}
#[test]
fn home_guard_resolves_symlinks_and_parent_components_in_path_order() {
    let temp = tempfile::tempdir().unwrap();
    let protected = temp.path().join("owner");
    std::fs::create_dir(&protected).unwrap();
    std::os::unix::fs::symlink(&protected, temp.path().join("alias")).unwrap();
    assert!(guard_home(&temp.path().join("alias/child"), &protected).is_err());
    assert!(guard_home(&protected.join("missing/../child"), &protected).is_err());
    let external = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(external.path(), temp.path().join("external")).unwrap();
    assert_eq!(
        resolve_path(&temp.path().join("external/../next")).unwrap(),
        external.path().parent().unwrap().join("next")
    );
}
#[test]
fn test_support_has_injectable_clock_and_reserved_port() {
    let clock = ManualClock::default();
    assert_eq!(clock.now(), Duration::ZERO);
    clock.advance(Duration::from_millis(10)).unwrap();
    assert_eq!(clock.now(), Duration::from_millis(10));
    let system = SystemClock::default();
    assert!(system.now() <= system.now());
    let listener = free_port().unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 0);
    assert!(std::net::TcpListener::bind(listener.local_addr().unwrap()).is_err());
}
#[test]
fn fixture_binary_is_inert_and_checks_scratch_home() {
    let home = ScratchHome::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fixture"))
        .env("SLUICE_HOME", home.path())
        .arg("fn")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("not implemented in this build")
    );
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}
#[test]
fn process_dispatch_is_injected_with_send_futures() {
    use sluice_process::FnHost;
    fn send<T: Send>(_: T) {}
    let invocation:FnInvocation=sluice_model::rpc::decode_json(br#"{"project":"019a2b3c-4d5e-7f01-8234-56789abcdef0","step":null,"run":"019a2b3c-4d5e-7f01-8234-56789abcdef0","attempt":"019a2b3c-4d5e-7f01-8234-56789abcdef0","invocation":"019a2b3c-4d5e-7f01-8234-56789abcdef0","name":"fake","inputs":{}}"#).unwrap();
    let host = sluice_process::guardian::UnimplementedFnHost;
    let mut future = std::pin::pin!(host.invoke(invocation));
    send(&mut future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match std::future::Future::poll(future.as_mut(), &mut context) {
        std::task::Poll::Ready(Err(error)) => {
            assert!(error.to_string().contains("not implemented in this build"))
        }
        _ => panic!("fn host stub did not fail immediately"),
    }
}
