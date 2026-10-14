use sluice_process::tmux::{ApprovedTmux, TmuxManifest};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[tokio::test]
async fn approved_artifact_refuses_substitution_and_confines_every_command() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/private-tmux");
    let prefix = tempfile::tempdir().unwrap();
    fs::create_dir(prefix.path().join("bin")).unwrap();
    fs::create_dir_all(prefix.path().join("share/tmux")).unwrap();
    for relative in ["tmux-manifest.json", "bin/tmux", "share/tmux/config.defs"] {
        fs::copy(source.join(relative), prefix.path().join(relative)).unwrap();
    }
    let approved = ApprovedTmux::load(prefix.path()).await.unwrap();
    let good = approved.manifest().clone();
    let write_manifest = |manifest: &TmuxManifest| {
        fs::write(
            prefix.path().join("tmux-manifest.json"),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
    };
    for change in 0..5 {
        let mut bad = good.clone();
        match change {
            0 => bad.configure_flags.push("--enable-systemd".into()),
            1 => bad.generated_config_lines[0] = "#define HAVE_SYSTEMD 1".into(),
            2 => bad.binary_path = "/usr/bin/tmux".into(),
            3 => bad.source_sha256 = "b".repeat(64),
            _ => bad.libevent.linkage = "dynamic".into(),
        }
        write_manifest(&bad);
        assert!(ApprovedTmux::load(prefix.path()).await.is_err());
    }
    write_manifest(&good);
    let run = tempfile::tempdir().unwrap();
    fs::set_permissions(run.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let server = approved.server_command(run.path(), None).unwrap();
    assert_eq!(
        server.get_args().collect::<Vec<_>>(),
        ["-S", "tmux.sock", "-f", "/dev/null", "-D"]
    );
    assert_eq!(server.get_program(), approved.binary());
    assert_eq!(server.get_current_dir(), Some(run.path()));
    fs::write(run.path().join("tmux.conf"), "set -g history-limit 1000\n").unwrap();
    let configured = approved
        .server_command(run.path(), Some(Path::new("tmux.conf")))
        .unwrap();
    assert!(
        configured
            .get_args()
            .any(|arg| arg == run.path().join("tmux.conf").as_os_str())
    );
    assert!(
        approved
            .server_command(run.path(), Some(Path::new("/etc/passwd")))
            .is_err()
    );
    let client = approved.control_command(run.path(), "fixture").unwrap();
    assert_eq!(
        client.get_args().collect::<Vec<_>>(),
        [
            "-S",
            "tmux.sock",
            "-f",
            "/dev/null",
            "-C",
            "attach-session",
            "-t",
            "fixture"
        ]
    );
    fs::set_permissions(run.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(approved.server_command(run.path(), None).is_err());
    fs::copy("/usr/bin/true", prefix.path().join("bin/tmux")).unwrap();
    let error = ApprovedTmux::load(prefix.path()).await.unwrap_err();
    assert!(error.to_string().contains("SHA-256"));
    let hash = std::process::Command::new("/usr/bin/sha256sum")
        .arg(prefix.path().join("bin/tmux"))
        .output()
        .unwrap();
    assert!(hash.status.success());
    let mut substituted = good.clone();
    substituted.binary_sha256 = String::from_utf8_lossy(&hash.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .into();
    write_manifest(&substituted);
    let error = ApprovedTmux::load(prefix.path()).await.unwrap_err();
    assert!(error.to_string().contains("tmux 3.7c"));
    let config = fs::read_to_string(prefix.path().join("share/tmux/config.defs")).unwrap();
    fs::write(
        prefix.path().join("share/tmux/config.defs"),
        format!("{config} -DENABLE_CGROUPS=1"),
    )
    .unwrap();
    let error = ApprovedTmux::load(prefix.path()).await.unwrap_err();
    assert!(error.to_string().contains("config does not match"));
}
