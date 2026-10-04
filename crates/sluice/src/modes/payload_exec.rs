use std::path::PathBuf;
pub fn early_dispatch(home: PathBuf) {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().and_then(|s| s.to_str()) == Some("payload-exec")
        && args
            .get(1)
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.len() == 64)
    {
        sluice_process::launcher::payload_exec_main(args[1..].to_vec(), |args| {
            if args.first().and_then(|s| s.to_str()) != Some("runtime") {
                return Err(std::io::Error::other("unknown payload dispatcher"));
            }
            let runtime = sluice_runtime::coordinator::executor()?;
            runtime.block_on(sluice_runtime::execution::payload_entry(home))
        });
    }
}
