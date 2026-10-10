#!/usr/bin/env python3
"""Test-only projection for the atomic lanes; production sources are never patched.

Keep commands, RPC, MCP and CLI plan decoding verbatim. The removed compiler's
unserialized carrier fields use hand-built opaque types, and Event::PlanEdit is
projected to B's pinned changes field. This cannot prove storage or edit execution.
Run the normal crate targets again on the integration branch.
"""
from pathlib import Path
import shutil, subprocess, sys, tomllib
root=Path(__file__).resolve().parents[4]
work=root/'target/plan-tools-projection'
if work.exists():
 raise SystemExit(f'{work} already exists; do not share this test projection between processes')
work.mkdir(parents=True)
print(f'Test-only source projection: {work}',flush=True)
def put(path,text):
 p=work/path;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text)
def copy(path):
 p=work/path;p.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(root/path,p)
def section(path,start,end):
 s=(root/path).read_text();return s[s.index(start):s.index(end,s.index(start))]
manifest=(root/'Cargo.toml').read_text();a=manifest.index('members = [');b=manifest.index('\n]',a)+2
manifest=manifest[:a]+'members = ["crates/sluice-model", "crates/sluice-store", "crates/sluice-runtime", "crates/sluice-web", "crates/sluice"]'+manifest[b:]
put('Cargo.toml',manifest)
copy('Cargo.lock');copy('.cargo/config.toml')
shutil.copytree(root/'docs',work/'docs')
# No compiler algorithms are needed to decode commands, previews and read replies.
for name in ['commands','rpc','ids','error','types','plan_rows']:
 copy(f'crates/sluice-model/src/{name}.rs')
events=(root/'crates/sluice-model/src/events.rs').read_text().replace('ops: Vec<PatchOperation>,','changes: Vec<crate::plan_rows::PlanChange>,')
put('crates/sluice-model/src/events.rs',events)
put('crates/sluice-model/src/lib.rs','''pub mod commands; pub mod rpc; pub mod ids; pub mod error; pub mod types; pub mod events; pub mod plan_rows;
pub use commands::RuntimeApi;
pub fn validate_declaration_shape(map: &rpc::JsonMap) -> Result<(), Vec<types::PathError>> { types::validate_json_map(map, "spec") }
pub mod plan { #[derive(Debug, Clone, PartialEq)] pub struct Plan; }
pub mod gates { #[derive(Debug, Clone, Default, PartialEq)] pub struct StateSnapshot; }
pub mod units { #[derive(Debug, Clone, PartialEq)] pub struct PruneSet; }
pub mod edit { #[derive(Debug, Clone, PartialEq)] pub struct InputChanges; }
''')
copy('crates/sluice-model/Cargo.toml')
shutil.copytree(root/'crates/sluice-model/tests',work/'crates/sluice-model/tests')
copy('crates/sluice-runtime/src/dispatch_ext/plan_values.rs');copy('crates/sluice-runtime/src/dispatch_ext/plan_tools.rs');copy('crates/sluice-runtime/src/docs.rs')
put('crates/sluice-store/src/lib.rs',(root/'crates/sluice/tests/support/plan_store_signatures.rs').read_text())
put('crates/sluice-store/Cargo.toml','''[package]
name="sluice-store"
version.workspace=true
edition.workspace=true
[dependencies]
sluice-model.workspace=true
rusqlite.workspace=true
''')
reply=section('crates/sluice-runtime/src/compose.rs','pub fn reply_value(','\npub struct Dispatcher')
put('crates/sluice-runtime/src/lib.rs','''pub mod docs;
pub mod dispatch_ext { #[path="plan_values.rs"] pub mod plan_values; #[path="plan_tools.rs"] pub mod plan_tools; }
pub mod calls { pub fn public(error: sluice_store::StoreError) -> sluice_model::error::PublicError { error.0 } }
pub mod execution { pub trait ExecutionHost: Send + Sync {} }
pub mod coordinator {
    pub struct Coordinator<H>(std::marker::PhantomData<H>);
    impl<H> Coordinator<H> {
        pub fn home(&self) -> &std::path::Path { unreachable!() }
        pub fn reads(&self) -> &sluice_store::ReadPool { unreachable!() }
    }
}
pub mod naming {
    use std::collections::BTreeMap;
    pub struct UnitNaming { pub recipe: String }
    pub struct Naming { pub units: BTreeMap<String, UnitNaming> }
    pub struct ProjectNaming { pub naming: Naming }
    pub fn for_project(_: &rusqlite::Connection, _: &std::path::Path, _: sluice_model::ids::ProjectId) -> sluice_store::Result<std::sync::Arc<ProjectNaming>> { unreachable!() }
    pub fn recipe_generation(_: &std::path::Path, _: sluice_model::ids::ProjectId) -> sluice_model::plan_rows::RecipeGeneration { unreachable!() }
}
pub mod compose { use sluice_model::{commands::CommandReply, error::PublicError}; use serde_json::json;
'''+reply+'\n}\n')
put('crates/sluice-runtime/Cargo.toml','''[package]
name="sluice-runtime"
version.workspace=true
edition.workspace=true
[dependencies]
sluice-model.workspace=true
sluice-store.workspace=true
rusqlite.workspace=true
serde_json.workspace=true
''')
copy('crates/sluice-web/src/mcp.rs');copy('crates/sluice-web/src/tool_args.rs')
# Keep HTTP error mapping verbatim; the actual endpoint adapter lives in mcp.rs.
error=section('crates/sluice-web/src/http.rs','pub fn error_response(','fn loopback_host(')
put('crates/sluice-web/src/http.rs','''use axum::{http::StatusCode,response::{Response,IntoResponse}};
use sluice_model::error::PublicError;
pub const MAX_BODY: usize = 1024*1024;
'''+error.split('fn status_error(')[0])
put('crates/sluice-web/src/lib.rs','pub mod mcp; pub mod tool_args; pub mod http;\n')
put('crates/sluice-web/Cargo.toml','''[package]
name="sluice-web"
version.workspace=true
edition.workspace=true
[dependencies]
sluice-model.workspace=true
sluice-runtime.workspace=true
serde_json.workspace=true
schemars.workspace=true
futures-util.workspace=true
rmcp.workspace=true
tokio.workspace=true
tokio-util.workspace=true
axum.workspace=true
''')
copy('crates/sluice/src/plan_tools.rs')
put('crates/sluice/src/lib.rs','pub mod plan_tools;\n')
put('crates/sluice/Cargo.toml','''[package]
name="sluice"
version.workspace=true
edition.workspace=true
[dependencies]
sluice-model.workspace=true
sluice-runtime.workspace=true
sluice-web.workspace=true
serde_json.workspace=true
tokio.workspace=true
[dev-dependencies]
axum.workspace=true
tower.workspace=true
''')
copy('crates/sluice/tests/tool_contracts.rs')
# The schema generator uses retained legacy schemas verbatim and regenerates owned wire types.
old=(root/'docs/rust/schemas.json').resolve()
put('crates/sluice-model/examples/plan_tool_schemas.rs','''use sluice_model::{commands::*,plan_rows::*,events::*,rpc::*};
fn main() { let mut schemas: serde_json::Map<String,serde_json::Value> = serde_json::from_str(include_str!("../../../docs/rust/schemas.json")).unwrap();
for key in ["PlanPatch","PatchOperation","PlanPatchData","PreparedEdit","PlanDocument","Snapshot"] { schemas.remove(key); }
macro_rules! add { ($t:ty) => { schemas.insert(stringify!($t).into(),serde_json::to_value(schemars::schema_for!($t)).unwrap()); }; }
'''+ '\n'.join('add!('+t+');' for t in ['RpcRequest','Event','Record','ChangeBatch','RecordPage','NextResult','RpcReply','RpcResult','CommandRequest','CommandReply','EditOptions','StepUpdate','EditPreview','EditResult','InputEditResult','PruneResult','PlanRead','StepGet','UnitGet','PlanEditRequest','UnitUpdate','UnitRemove','PlanViewQuery','PlanHistoryQuery','StepChanges','PreviewScope','PlanOp','PlanChange','PlanGetResult','PlanReadResult','StepGetResult','UnitGetResult','PlanHistoryPage','CompactStep','FullStep','StepReference','StepView','UnitView','PlanEditEvent','HistoryEvent','HistoryRecord','PauseValue','ConsumerKind','RefKind','SourceKind','EdgeKind','StateEpoch','RecipeGeneration','CatalogGeneration','ValidationTokens'])+'''
println!("{}",serde_json::to_string_pretty(&schemas).unwrap()); }
''')
def run(args,**kw):
 print('+ '+' '.join(args),flush=True);subprocess.run(args,cwd=work,check=True,**kw)
try:
 cargo=['cargo','--config',f'build.target-dir="{root}/target"']
 # Let Cargo adjust local package edges while retaining the repository's registry pins.
 run(cargo+['check','--offline','-p','sluice-model','--lib'])
 def registry_pins(path):
  data=tomllib.loads(path.read_text())
  return {(p['name'],p['version'],p.get('source')) for p in data['package'] if 'source' in p}
 assert registry_pins(work/'Cargo.lock') <= registry_pins(root/'Cargo.lock'), 'projection changed a registry pin'
 if '--schemas-only' not in sys.argv:
  run(cargo+['clippy','--locked','-p','sluice-model','--lib','--test','command_plan_rows','--','-D','warnings'])
  run(cargo+['clippy','--locked','-p','sluice','--lib','--test','tool_contracts','--','-D','warnings'])
  run(cargo+['test','--locked','-p','sluice-model','--test','command_plan_rows'])
  run(cargo+['test','--locked','-p','sluice','--test','tool_contracts'])
 if '--schemas' in sys.argv or '--schemas-only' in sys.argv:
  dest=root/'docs/rust/schemas.json'; candidate=work/'schemas.new.json'
  with candidate.open('w') as out: run(cargo+['run','--locked','-p','sluice-model','--example','plan_tool_schemas'],stdout=out)
  shutil.copyfile(candidate,dest)
finally:
 shutil.rmtree(work)
