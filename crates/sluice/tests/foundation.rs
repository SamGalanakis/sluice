#[path = "../../../tests/support/mod.rs"]
mod support;

use sluice_model::{error::PublicError, rpc::FnInvocation};
use sluice_process::host::{guard_home, resolve_path};
use std::{
    path::Path,
    process::{Command, Output},
    time::Duration,
};
use support::{
    chrome::Chrome,
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
fn every_dispatch_mode_fails_without_touching_a_scratch_home() {
    let home = ScratchHome::new().unwrap();
    let id = "019a2b3c-4d5e-7f01-8234-56789abcdef0";
    for args in [
        vec!["coordinator", "--maintenance"],
        vec!["serve", "--no-runner", "--port", "0"],
        vec!["loop"],
        vec![
            "guardian",
            "--run",
            id,
            "--attempt",
            id,
            "--socket",
            "control.sock",
        ],
        vec![
            "payload-exec",
            "--run",
            id,
            "--attempt",
            id,
            "--socket",
            "control.sock",
        ],
        vec!["tool", "projects_list", "{}"],
        vec!["me", "--json"],
        vec!["doctor", "--json"],
        vec![
            "import-python-home",
            "scratch-source",
            "scratch-destination",
        ],
        vec!["agent", "hook", "--engine", "codex", "--event", "Stop"],
    ] {
        let output = run(home.path(), &args);
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
        vec!["tool"],
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
#[test]
fn protected_home_is_rejected_before_even_help_parsing() {
    for path in [
        "/home/sam/.sluice",
        "/home/sam/.sluice/runs",
        "/home/sam/.sluice/nonexistent/../child",
    ] {
        let output = run(Path::new(path), &["--help"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("refusing the owner's real SLUICE_HOME")
        );
        assert!(ScratchHome::validate(Path::new(path)).is_err());
    }
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
    std::os::unix::fs::symlink(sluice_process::host::OWNER_HOME, temp.path().join("alias2"))
        .unwrap();
    let output = run(&temp.path().join("alias2"), &["doctor"]);
    assert_eq!(output.status.code(), Some(1));
}
#[test]
fn test_support_has_injectable_clock_reserved_port_and_typed_browser_stub() {
    let clock = ManualClock::default();
    assert_eq!(clock.now(), Duration::ZERO);
    clock.advance(Duration::from_millis(10)).unwrap();
    assert_eq!(clock.now(), Duration::from_millis(10));
    let system = SystemClock::default();
    assert!(system.now() <= system.now());
    let listener = free_port().unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), 0);
    assert!(std::net::TcpListener::bind(listener.local_addr().unwrap()).is_err());
    let error = match Chrome::open("http://127.0.0.1") {
        Ok(_) => panic!("stub started a browser"),
        Err(e) => e,
    };
    assert!(error.to_string().contains("not implemented in this build"));
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
