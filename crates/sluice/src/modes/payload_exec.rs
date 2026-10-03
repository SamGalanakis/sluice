use std::path::PathBuf;
pub fn early_dispatch(home: PathBuf) {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "internal" && args[1] == "callback" {
        use std::io::Read;
        let mut bytes = Vec::new();
        let result = std::io::stdin()
            .take(sluice_model::rpc::MAX_FRAME_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .and_then(|_| {
                if bytes.len() > sluice_model::rpc::MAX_FRAME_BYTES {
                    return Err(std::io::Error::other("callback exceeds frame limit"));
                }
                let runtime = sluice_runtime::coordinator::executor()?;
                runtime
                    .block_on(sluice_runtime::compose::internal_callback(
                        home.clone(),
                        &bytes,
                    ))
                    .map_err(std::io::Error::other)
            });
        match result {
            Ok(reply) => {
                println!("{}", serde_json::to_string(&reply).expect("RPC reply"));
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
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
