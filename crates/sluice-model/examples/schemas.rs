use sluice_model::{commands::*, error::*, events::*, gates::*, ids::*, plan::*, rpc::*, types::*};
include!("../tests/registry.rs.inc");
fn main() {
    let mut schemas = serde_json::Map::new();
    macro_rules! add {
        ($t:ty) => {
            schemas.insert(
                stringify!($t).into(),
                serde_json::to_value(schemars::schema_for!($t)).expect("schema serialization"),
            );
        };
    }
    every_contract!(add);
    println!(
        "{}",
        serde_json::to_string_pretty(&schemas).expect("schema document")
    );
}
