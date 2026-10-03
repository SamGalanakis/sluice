use super::{Mode, ModeFuture};
use std::path::PathBuf;
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        use sluice_model::{RuntimeApi, error::PublicError, rpc::decode_json};
        let Mode::Tool { name, json } = mode else {
            unreachable!()
        };
        let value = json.as_deref().unwrap_or("{}");
        let request = if name == "rpc" {
            decode_json(value.as_bytes())?
        } else {
            let args: sluice_model::rpc::JsonValue = decode_json(value.as_bytes())?;
            let mut object = serde_json::json!({"command":name});
            if args.as_value() != &serde_json::json!({}) {
                object["args"] = args.into_value();
            }
            decode_json(&serde_json::to_vec(&object).expect("JSON"))?
        };
        let client = sluice_runtime::client::ensure_coordinator(
            &home,
            &std::env::current_exe().map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })?,
        )
        .await?;
        println!(
            "{}",
            serde_json::to_string(&client.command(request).await?).expect("reply")
        );
        Ok(())
    })
}
