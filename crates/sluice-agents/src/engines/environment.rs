use std::collections::BTreeMap;

/// Explicit engine inputs. Private tmux servers never inherit the guardian environment.
pub fn host_environment() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for name in [
        "HOME",
        "LANG",
        "LC_ALL",
        "TERM",
        "SHELL",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "DEVIN_API_KEY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "SLUICE_HOME",
        "SLUICE_BIN",
        "SLUICE_PROJECT_ID",
        "SLUICE_PROJECT",
        "SLUICE_STEP",
        "SLUICE_RUN_ID",
        "SLUICE_RUN_DIR",
        "SLUICE_PROJECT_DIR",
        "SLUICE_FN_DIR",
        "SLUICE_PREV_RUN",
        "SLUICE_CONTROL_SOCKET",
        "SLUICE_RUN_CAPABILITY",
    ] {
        if let Ok(value) = std::env::var(name) {
            env.insert(name.into(), value);
        }
    }
    let path = std::env::var("SLUICE_HOST_PATH")
        .or_else(|_| std::env::var("PATH"))
        .unwrap_or_else(|_| "/usr/bin:/bin".into());
    env.insert("PATH".into(), path.clone());
    env.insert("SLUICE_HOST_PATH".into(), path);
    env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
    env.entry("TERM".into())
        .or_insert_with(|| "xterm-256color".into());
    if let Some(home) = env.get("HOME").cloned() {
        for (key, suffix) in [
            ("XDG_CONFIG_HOME", ".config"),
            ("XDG_DATA_HOME", ".local/share"),
            ("XDG_CACHE_HOME", ".cache"),
            ("XDG_STATE_HOME", ".local/state"),
        ] {
            env.entry(key.into())
                .or_insert_with(|| format!("{home}/{suffix}"));
        }
    }
    env
}
