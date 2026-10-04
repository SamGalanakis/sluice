//! Review reproductions use only fake engines and scratch-owned user services.
#[path = "../../../tests/support/home.rs"]
mod home;
use std::{path::Path, process::Command};
fn probe(name: &str) {
    let home = home::ScratchHome::new().unwrap();
    home::ScratchHome::validate(home.path()).unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let exe = std::env::current_exe().unwrap();
    let binary = exe.parent().unwrap().parent().unwrap().join("sluice");
    assert!(
        binary.is_file(),
        "Build the workspace binaries before the host gates"
    );
    let output = Command::new("/usr/bin/python3")
        .arg(repo.join("crates/sluice-runtime/tests/fixtures/exec_host.py"))
        .arg(name)
        .env("SLUICE_EXEC_TEST_ROOT", home.root())
        .env("SLUICE_EXEC_TEST_REPO", &repo)
        .env("SLUICE_EXEC_TEST_BIN", binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{name}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
#[ignore = "real systemd guardian with a fake composed engine"]
fn exec_cancel_sidecar_cleans_whole_payload() {
    probe("cancel_sidecar");
}
#[test]
#[ignore = "real systemd guardian with a Python fixture"]
fn exec_terminal_tool_refused() {
    probe("stale_tool");
}
#[test]
#[ignore = "real systemd guardian with pinned Python fixtures"]
fn exec_pinned_completion_survives_changed_sibling_signature() {
    probe("changed_sibling_signature");
}
#[test]
#[ignore = "real systemd guardian with a direct Python fixture"]
fn exec_project_direct_call_acquires_section() {
    probe("direct_section");
}
