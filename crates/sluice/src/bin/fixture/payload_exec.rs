pub const CLI_NAME: Option<&str> = None;
use std::process::ExitCode;
pub fn matches() -> bool {
    std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("payload-exec"))
}
pub fn run() -> ExitCode {
    use std::path::PathBuf;
    let args = std::env::args_os().skip(2);
    sluice_process::launcher::payload_exec_main(args.collect(), |args| {
        match args.first().and_then(|s| s.to_str()) {
            Some("fn-result") if args.len() == 1 => {
                use std::io::Write;
                let dir = PathBuf::from(
                    std::env::var_os("SLUICE_RUN_DIR")
                        .ok_or_else(|| std::io::Error::other("missing run dir"))?,
                );
                let mut count = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("dispatch-count"))?;
                count.write_all(b"dispatch\n")?;
                while !dir.join("finish").exists() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                println!("{{\"ok\":true,\"outputs\":{{\"fixture\":true}}}}");
                Ok(0)
            }
            Some("write-marker") if args.len() == 2 => {
                std::fs::write(&args[1], b"dispatched")?;
                Ok(0)
            }
            Some("exec") if args.len() >= 2 => {
                let mut command = std::process::Command::new(&args[1]);
                command.args(&args[2..]).stdin(std::process::Stdio::null());
                sluice_process::launcher::exec_payload(&mut command)
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unknown fixture payload",
            )),
        }
    })
}
