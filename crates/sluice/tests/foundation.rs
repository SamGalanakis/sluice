use sluice_model::error::PublicError;
use sluice_process::host::{guard_home, refuse_live_home, resolve_path};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

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
fn test_mode_rejects_the_live_home() {
    let temp = tempfile::tempdir().unwrap();
    let (account, live) = live_account(temp.path());
    std::os::unix::fs::symlink(&live, temp.path().join("alias")).unwrap();
    for path in [
        live.clone(),
        live.join("runs"),
        live.join("nonexistent/../child"),
        temp.path().join("alias/child"),
    ] {
        let error = refuse_live_home(&path, &account).unwrap_err();
        assert!(error.to_string().contains(REFUSAL), "{path:?}: {error}");
    }
    assert!(refuse_live_home(&temp.path().join("scratch"), &account).is_ok());
    // An account without an installation protects nothing.
    assert!(refuse_live_home(&live, temp.path()).is_ok());
    std::fs::write(
        account.join(".local/share/sluice/install/selection.json"),
        "{",
    )
    .unwrap();
    assert!(refuse_live_home(&temp.path().join("scratch"), &account).is_err());
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
