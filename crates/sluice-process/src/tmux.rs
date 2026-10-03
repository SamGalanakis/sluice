//! Approved release artifact and argv for servers confined to one private run directory.
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

pub const VERSION: &str = "3.7c";
pub const SOURCE_URL: &str = "https://github.com/tmux/tmux/releases/download/3.7c/tmux-3.7c.tar.gz";
pub const SOURCE_SHA256: &str = "7c60cae9a0e25288e2e24750aafc9e8800fc7fd4555e447e1b29ee4201cfb3bf";
pub const LIBEVENT_URL: &str = "https://github.com/libevent/libevent/releases/download/release-2.1.12-stable/libevent-2.1.12-stable.tar.gz";
pub const LIBEVENT_SHA256: &str =
    "92e6de1be9ec176428fd2367677e61ceffc2ee1cb119035037a27d346b0403bb";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticLibevent {
    pub version: String,
    pub source_url: String,
    pub source_sha256: String,
    pub configure_flags: Vec<String>,
    pub linkage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicNcurses {
    pub linkage: String,
    pub packages: Vec<String>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TmuxManifest {
    pub schema_version: u32,
    pub version: String,
    pub source_url: String,
    pub source_sha256: String,
    pub configure_flags: Vec<String>,
    pub generated_config_lines: Vec<String>,
    pub binary_path: PathBuf,
    pub binary_sha256: String,
    pub libevent: StaticLibevent,
    pub ncurses: DynamicNcurses,
    pub parser: String,
    pub build_host: String,
    pub built_at: String,
}

/// Construct only by verifying a release prefix. A host tmux is never an artifact.
#[derive(Debug)]
pub struct ApprovedTmux {
    binary: PathBuf,
    manifest: TmuxManifest,
}

impl ApprovedTmux {
    pub async fn load(prefix: &Path) -> io::Result<Self> {
        let prefix = fs::canonicalize(prefix)?;
        let bytes = fs::read(prefix.join("tmux-manifest.json"))?;
        let manifest: TmuxManifest =
            sluice_model::rpc::decode_json(&bytes).map_err(|e| invalid(e.to_string()))?;
        manifest.validate()?;
        let binary = fs::canonicalize(prefix.join(&manifest.binary_path))?;
        if !binary.starts_with(&prefix) {
            return Err(invalid("tmux binary escapes the release prefix"));
        }
        let config = fs::read_to_string(prefix.join("share/tmux/config.defs"))?;
        let lines: Vec<String> = config.lines().map(str::to_owned).collect();
        if lines != manifest.generated_config_lines {
            return Err(invalid("generated config does not match the manifest"));
        }
        let mut hash = Command::new("/usr/bin/sha256sum");
        hash.arg("--").arg(&binary);
        let output = checked_output(hash).await?;
        let digest = String::from_utf8_lossy(&output.stdout);
        if digest.split_whitespace().next() != Some(&manifest.binary_sha256) {
            return Err(invalid(
                "tmux binary SHA-256 does not match the manifest; rebuild the artifact",
            ));
        }
        let mut version = Command::new(&binary);
        version.arg("-V");
        let output = checked_output(version).await?;
        if output.stdout != b"tmux 3.7c\n" {
            return Err(invalid("approved tmux must report exactly tmux 3.7c"));
        }
        // ldd also checks the dynamic ncurses/tinfo runtime dependencies.
        let mut linkage = Command::new("/usr/bin/ldd");
        linkage.arg(&binary);
        let output = checked_output(linkage).await?;
        let libraries = String::from_utf8_lossy(&output.stdout);
        if libraries.contains("libevent")
            || libraries.contains("libsystemd")
            || libraries.contains("not found")
            || !libraries.contains("libtinfo")
        {
            return Err(invalid(
                "tmux needs static libevent, no libsystemd, and available dynamic tinfo/ncurses",
            ));
        }
        Ok(Self { binary, manifest })
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn manifest(&self) -> &TmuxManifest {
        &self.manifest
    }

    /// Socket names are short and relative to a caller-owned mode-0700 directory.
    /// Append a tmux command such as `start-server` or `new-session` as separate argv.
    pub fn server_command(&self, run_dir: &Path, config: Option<&Path>) -> io::Result<Command> {
        let mut command = self.command(run_dir, config)?;
        command.arg("-D");
        Ok(command)
    }

    pub fn control_command(&self, run_dir: &Path, session: &str) -> io::Result<Command> {
        if session.is_empty() || session.starts_with('-') {
            return Err(invalid("a private session name is required"));
        }
        let mut command = self.command(run_dir, None)?;
        command.args(["-C", "attach-session", "-t", session]);
        Ok(command)
    }

    /// A regular client, for setup/inspection on this same private socket.
    pub fn client_command(&self, run_dir: &Path) -> io::Result<Command> {
        self.command(run_dir, None)
    }

    fn command(&self, run_dir: &Path, config: Option<&Path>) -> io::Result<Command> {
        let run_dir = fs::canonicalize(run_dir)?;
        let metadata = fs::metadata(&run_dir)?;
        if !metadata.is_dir()
            || metadata.permissions().mode() & 0o777 != 0o700
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(invalid(
                "private tmux run directory must be owned by this user with mode 0700",
            ));
        }
        let config = match config {
            Some(path) => {
                let path = fs::canonicalize(run_dir.join(path))?;
                if !path.starts_with(&run_dir) || !fs::metadata(&path)?.is_file() {
                    return Err(invalid(
                        "explicit tmux config must be a file in the private run directory",
                    ));
                }
                path
            }
            None => PathBuf::from("/dev/null"),
        };
        let mut command = Command::new(&self.binary);
        command
            .current_dir(run_dir)
            .args(["-S", "tmux.sock", "-f"])
            .arg(config)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        Ok(command)
    }
}

impl TmuxManifest {
    fn validate(&self) -> io::Result<()> {
        if self.schema_version != 1
            || self.version != VERSION
            || self.source_url != SOURCE_URL
            || self.source_sha256 != SOURCE_SHA256
            || self.binary_path != Path::new("bin/tmux")
            || self.build_host.is_empty()
            || self.built_at.is_empty()
            || self.binary_sha256.len() != 64
            || !self
                .binary_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(invalid(
                "unapproved tmux manifest version, provenance, binary path or digest",
            ));
        }
        validate_flags(
            &self.configure_flags,
            &["--disable-systemd", "--disable-cgroups"],
            2,
        )?;
        if self.generated_config_lines.len() != 1
            || !self.generated_config_lines[0].starts_with("DEFS = ")
            || self.generated_config_lines[0].contains("HAVE_SYSTEMD")
            || self.generated_config_lines[0].contains("ENABLE_CGROUPS")
        {
            return Err(invalid(
                "tmux generated DEFS must omit both integration macros",
            ));
        }
        let event = &self.libevent;
        if event.version != "2.1.12-stable"
            || event.source_url != LIBEVENT_URL
            || event.source_sha256 != LIBEVENT_SHA256
            || event.linkage != "static"
        {
            return Err(invalid("unapproved libevent provenance or linkage"));
        }
        validate_flags(
            &event.configure_flags,
            &["--disable-shared", "--enable-static", "--disable-openssl"],
            1,
        )?;
        if self.ncurses.linkage != "dynamic"
            || self.ncurses.packages != ["ncursesw", "tinfo"]
            || self.ncurses.version.is_empty()
            || self.parser != "release cmd-parse.c"
        {
            return Err(invalid("unapproved ncurses linkage or parser provenance"));
        }
        Ok(())
    }
}

fn validate_flags(flags: &[String], required: &[&str], paths: usize) -> io::Result<()> {
    if flags.len() != required.len() + paths
        || !required
            .iter()
            .all(|flag| flags.iter().filter(|s| *s == flag).count() == 1)
        || flags.iter().filter(|s| s.starts_with("--prefix=")).count() != 1
        || (paths == 2
            && flags
                .iter()
                .filter(|s| s.starts_with("--sysconfdir="))
                .count()
                != 1)
    {
        return Err(invalid("unapproved configure flags"));
    }
    Ok(())
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Bound external probes; dropping a timed-out probe kills its direct child.
pub(crate) async fn checked_output(command: Command) -> io::Result<Output> {
    let description = format!("{command:?}");
    let mut command = tokio::process::Command::from(command);
    command.stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("probe timed out: {description}"),
            )
        })??;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{description}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> TmuxManifest {
        TmuxManifest {
            schema_version: 1,
            version: VERSION.into(),
            source_url: SOURCE_URL.into(),
            source_sha256: SOURCE_SHA256.into(),
            configure_flags: vec![
                "--disable-systemd".into(),
                "--disable-cgroups".into(),
                "--prefix=/release".into(),
                "--sysconfdir=/release/etc".into(),
            ],
            generated_config_lines: vec!["DEFS = -DPACKAGE_VERSION=3.7c".into()],
            binary_path: "bin/tmux".into(),
            binary_sha256: "a".repeat(64),
            libevent: StaticLibevent {
                version: "2.1.12-stable".into(),
                source_url: LIBEVENT_URL.into(),
                source_sha256: LIBEVENT_SHA256.into(),
                configure_flags: vec![
                    "--disable-shared".into(),
                    "--enable-static".into(),
                    "--disable-openssl".into(),
                    "--prefix=/release/libevent".into(),
                ],
                linkage: "static".into(),
            },
            ncurses: DynamicNcurses {
                linkage: "dynamic".into(),
                packages: vec!["ncursesw".into(), "tinfo".into()],
                version: "6.4".into(),
            },
            parser: "release cmd-parse.c".into(),
            build_host: "host".into(),
            built_at: "2026-10-03".into(),
        }
    }

    #[test]
    fn rejects_enabled_integration_and_foreign_artifacts() {
        let good = manifest();
        good.validate().unwrap();
        for change in 0..5 {
            let mut bad = good.clone();
            match change {
                0 => bad.configure_flags.push("--enable-systemd".into()),
                1 => bad.generated_config_lines[0] = "#define HAVE_SYSTEMD 1".into(),
                2 => bad.binary_path = "/usr/bin/tmux".into(),
                3 => bad.source_sha256 = "b".repeat(64),
                _ => bad.libevent.linkage = "dynamic".into(),
            }
            assert!(bad.validate().is_err());
        }
    }

    #[tokio::test]
    async fn rejects_substituted_binary_and_config() {
        let prefix = tempfile::tempdir().unwrap();
        fs::create_dir(prefix.path().join("bin")).unwrap();
        fs::create_dir_all(prefix.path().join("share/tmux")).unwrap();
        fs::copy("/usr/bin/true", prefix.path().join("bin/tmux")).unwrap();
        let mut record = manifest();
        let config = record.generated_config_lines.join("\n");
        fs::write(prefix.path().join("share/tmux/config.defs"), &config).unwrap();
        let write_manifest = |record: &TmuxManifest| {
            fs::write(
                prefix.path().join("tmux-manifest.json"),
                serde_json::to_vec(record).unwrap(),
            )
            .unwrap();
        };
        write_manifest(&record);
        let error = ApprovedTmux::load(prefix.path()).await.unwrap_err();
        assert!(error.to_string().contains("SHA-256"));
        let mut hash = Command::new("/usr/bin/sha256sum");
        hash.arg(prefix.path().join("bin/tmux"));
        let output = checked_output(hash).await.unwrap();
        record.binary_sha256 = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .into();
        write_manifest(&record);
        let error = ApprovedTmux::load(prefix.path()).await.unwrap_err();
        assert!(error.to_string().contains("tmux 3.7c"));
        fs::write(
            prefix.path().join("share/tmux/config.defs"),
            format!("{config} -DENABLE_CGROUPS=1"),
        )
        .unwrap();
        let error = ApprovedTmux::load(prefix.path()).await.unwrap_err();
        assert!(error.to_string().contains("config does not match"));
    }

    #[test]
    fn all_commands_name_only_the_private_socket() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let tmux = ApprovedTmux {
            binary: "/release/bin/tmux".into(),
            manifest: manifest(),
        };
        let server = tmux.server_command(dir.path(), None).unwrap();
        let args: Vec<_> = server.get_args().collect();
        assert_eq!(args, ["-S", "tmux.sock", "-f", "/dev/null", "-D"]);
        assert_eq!(server.get_program(), "/release/bin/tmux");
        assert_eq!(server.get_current_dir(), Some(dir.path()));
        fs::write(dir.path().join("tmux.conf"), "set -g history-limit 1000\n").unwrap();
        let configured = tmux
            .server_command(dir.path(), Some(Path::new("tmux.conf")))
            .unwrap();
        assert!(
            configured
                .get_args()
                .any(|arg| arg == dir.path().join("tmux.conf").as_os_str())
        );
        assert!(
            tmux.server_command(dir.path(), Some(Path::new("/etc/passwd")))
                .is_err()
        );
        let client = tmux.control_command(dir.path(), "fixture").unwrap();
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
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(tmux.server_command(dir.path(), None).is_err());
    }
}
