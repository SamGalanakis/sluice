//! Chromium's NUL-delimited CDP pipe. All processes and the profile belong to this guard.
#![allow(dead_code)] // Shared support is also compiled by the foundation target.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, argument: i32) -> i32;
    fn dup2(old: i32, new: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn setsid() -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}
pub struct Chrome {
    child: Child,
    pipe: UnixStream,
    buffer: Vec<u8>,
    sequence: u64,
    session: Option<String>,
    _profile: tempfile::TempDir,
    pub events: Vec<Value>,
}
impl Chrome {
    pub fn binary() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("SLUICE_CHROME") {
            return Ok(path.into());
        }
        let cache = PathBuf::from(std::env::var_os("HOME").ok_or("HOME missing")?)
            .join(".cache/ms-playwright");
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(cache) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("chromium-") {
                    let path = entry.path().join("chrome-linux64/chrome");
                    if path.is_file() {
                        found.push(path);
                    }
                }
            }
        }
        found.sort();
        found
            .pop()
            .ok_or_else(|| "Chromium required; set SLUICE_CHROME".into())
    }
    pub fn open(url: &str) -> Result<Self> {
        let profile = tempfile::tempdir()?;
        let (pipe, remote) = UnixStream::pair()?;
        pipe.set_write_timeout(Some(Duration::from_secs(30)))?;
        let fd = remote.as_raw_fd();
        let mut command = Command::new(Self::binary()?);
        command
            .args([
                "--headless",
                "--remote-debugging-pipe",
                "--no-sandbox",
                "--disable-gpu",
                "--no-first-run",
            ])
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(proxy) = std::env::var("HTTPS_PROXY") {
            command.arg(format!("--proxy-server={proxy}"));
        }
        // Only async-signal-safe syscalls run between fork and exec. Duplicate above
        // the reserved descriptors first, including when the socket already is 3/4.
        unsafe {
            command.pre_exec(move || {
                let high = fcntl(fd, 0, 10); // F_DUPFD, without CLOEXEC.
                if high < 0 || dup2(high, 3) < 0 || dup2(high, 4) < 0 || setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                close(high);
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(remote);
        let mut chrome = Self {
            child,
            pipe,
            buffer: Vec::new(),
            sequence: 0,
            session: None,
            _profile: profile,
            events: vec![],
        };
        let target = chrome.send("Target.createTarget", json!({"url":"about:blank"}))?["targetId"]
            .as_str()
            .ok_or("missing target")?
            .to_owned();
        chrome.session = Some(
            chrome.send(
                "Target.attachToTarget",
                json!({"targetId":target,"flatten":true}),
            )?["sessionId"]
                .as_str()
                .ok_or("missing session")?
                .to_owned(),
        );
        chrome.send("Page.enable", json!({}))?;
        chrome.send("Runtime.enable", json!({}))?;
        chrome.send("Page.addScriptToEvaluateOnNewDocument", json!({"source":
            "window.browserErrors=[];addEventListener('error',e=>browserErrors.push(e.message));addEventListener('unhandledrejection',e=>browserErrors.push(String(e.reason)))"}))?;
        chrome.navigate(url)?;
        Ok(chrome)
    }
    pub fn navigate(&mut self, url: &str) -> Result<()> {
        if url.contains("127.0.0.1:3065") || url.contains("localhost:3065") {
            return Err("live port forbidden".into());
        }
        let reply = self.send("Page.navigate", json!({"url":url}))?;
        if let Some(error) = reply.get("errorText") {
            return Err(error.to_string().into());
        }
        Ok(())
    }
    pub fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        self.send_timeout(method, params, Duration::from_secs(30))
    }
    pub fn send_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        self.sequence += 1;
        let mut request = json!({"id":self.sequence,"method":method,"params":params});
        if !method.starts_with("Target.") {
            request["sessionId"] = json!(self.session);
        }
        self.pipe.write_all(&serde_json::to_vec(&request)?)?;
        self.pipe.write_all(&[0])?;
        let deadline = Instant::now() + timeout;
        loop {
            while let Some(end) = self.buffer.iter().position(|b| *b == 0) {
                let bytes: Vec<_> = self.buffer.drain(..=end).collect();
                let reply: Value = serde_json::from_slice(&bytes[..end])?;
                if reply["id"].as_u64() == Some(self.sequence) {
                    if let Some(error) = reply.get("error") {
                        return Err(format!("{method}: {error}").into());
                    }
                    return Ok(reply["result"].clone());
                }
                if reply.get("method").is_some() {
                    self.events.push(reply);
                }
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| format!("{method} timed out"))?;
            self.pipe.set_read_timeout(Some(remaining))?;
            let mut chunk = [0; 65536];
            let count = self.pipe.read(&mut chunk)?;
            if count == 0 {
                return Err("Chromium exited".into());
            }
            self.buffer.extend_from_slice(&chunk[..count]);
        }
    }
    pub fn eval(&mut self, expression: &str) -> Result<Value> {
        let reply = self.send(
            "Runtime.evaluate",
            json!({"expression":expression,"awaitPromise":true,"returnByValue":true}),
        )?;
        if let Some(error) = reply.get("exceptionDetails") {
            return Err(error.to_string().into());
        }
        Ok(reply["result"]["value"].clone())
    }
    pub fn wait(&mut self, expression: &str) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let value = self.eval(expression)?;
            if !matches!(value, Value::Null | Value::Bool(false))
                && value != json!(0)
                && value != json!("")
            {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err(format!("wait timed out: {expression}").into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    /// The page at `width` in `theme`: "light" or "dark" (Sluice Light or Dark), or a theme's
    /// id ("nord-dark"); the system's scheme is emulated to match.
    pub fn viewport(&mut self, width: u32, theme: &str) -> Result<()> {
        let id = match theme {
            "light" | "dark" => format!("sluice-{theme}"),
            id => id.to_owned(),
        };
        let scheme = if id.ends_with("-dark") {
            "dark"
        } else {
            "light"
        };
        self.send("Emulation.setDeviceMetricsOverride", json!({"width":width,"height":1000,"screenWidth":width,"screenHeight":1000,"deviceScaleFactor":1,"mobile":false}))?;
        self.send(
            "Emulation.setEmulatedMedia",
            json!({"features":[{"name":"prefers-color-scheme","value":scheme}]}),
        )?;
        self.eval(&format!(
            "document.documentElement.dataset.theme={};document.fonts.ready",
            json!(id)
        ))?;
        self.eval("window.scrollTo(0,0);new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))")?;
        Ok(())
    }
    pub fn screenshot(&mut self, path: &Path) -> Result<()> {
        let height = self.eval("Math.max(1000,document.documentElement.scrollHeight)")?;
        let width = self.eval("innerWidth")?;
        let data = self.send("Page.captureScreenshot", json!({"format":"png","captureBeyondViewport":true,"clip":{"x":0,"y":0,"width":width,"height":height,"scale":1}}))?;
        let encoded = data["data"].as_str().ok_or("missing PNG")?;
        let mut bytes = Vec::new();
        let mut bits = 0u32;
        let mut count = 0;
        for byte in encoded.bytes().take_while(|b| *b != b'=') {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return Err("invalid PNG base64".into()),
            };
            bits = (bits << 6) | u32::from(value);
            count += 6;
            if count >= 8 {
                count -= 8;
                bytes.push((bits >> count) as u8);
            }
        }
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("not PNG".into());
        }
        std::fs::create_dir_all(path.parent().ok_or("screenshot parent missing")?)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}
impl Drop for Chrome {
    fn drop(&mut self) {
        // setsid above makes this exactly the process group owned by this guard.
        unsafe {
            kill(-(self.child.id() as i32), 9);
        }
        let _ = self.child.wait();
    }
}
