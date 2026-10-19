mod acceptance;
mod cli;
mod compose;
mod cutover;
mod foundation;
mod g1a;
mod identities;
mod install;
mod me;
mod park;
mod record_reader;
mod release;
mod schema_migrate;
mod scratch;
mod ship;
mod status_units;
mod tool_contracts;
mod tool_descriptions;

#[path = "../../../../tests/support/executable.rs"]
mod executable;
#[path = "../fixtures/install_support.rs"]
mod install_support;
#[allow(dead_code)]
#[path = "../../../../tests/support/mod.rs"]
mod support;
