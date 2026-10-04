//! Native-process and SDK gates own every child and use only a disposable home.
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientConfig, Implementation, ProtocolVersion},
    transport::StreamableHttpClientTransport,
};
use serde_json::{Value, json};
use sluice_runtime::client::CoordinatorClient;
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
/// A workspace binary from the profile directory cargo built this test into (the
/// shared or configured target dir), never a stale `<repo>/target/debug` copy.
fn built(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().and_then(Path::parent).unwrap().join(name)
}
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
pub struct Scratch {
    pub home: tempfile::TempDir,
    pub children: Vec<Child>,
    pub url: String,
    pub client: CoordinatorClient,
    pub groups: Vec<u32>,
}
impl Scratch {
    pub fn new(home: tempfile::TempDir) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, 3065);
        drop(listener);
        let client = CoordinatorClient::new(home.path());
        Self {
            home,
            children: vec![],
            url: format!("http://127.0.0.1:{port}"),
            client,
            groups: vec![],
        }
    }
    pub fn spawn(&mut self, args: &[&str]) -> usize {
        let index = self.children.len();
        let log =
            std::fs::File::create(self.home.path().join(format!("child-{index}.log"))).unwrap();
        let child = Command::new(built("sluice"))
            .args(args)
            .env("SLUICE_HOME", self.home.path())
            .env("SLUICE_FIXTURE", "1")
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("build the absolute worktree binary first");
        eprintln!("scratch child {index} PID {} {args:?}", child.id());
        self.children.push(child);
        index
    }
    pub fn stop(&mut self, index: usize) {
        let child = &mut self.children[index];
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
        }
        child.wait().unwrap();
    }
    pub async fn boot(&mut self) {
        self.spawn(&["coordinator"]);
        let deadline = Instant::now() + Duration::from_secs(20);
        while tokio::net::UnixStream::connect(self.home.path().join("coordinator.sock"))
            .await
            .is_err()
        {
            assert!(
                self.children[0].try_wait().unwrap().is_none(),
                "coordinator exited: {}",
                std::fs::read_to_string(self.home.path().join("child-0.log")).unwrap()
            );
            assert!(Instant::now() < deadline, "coordinator did not boot");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        self.http().await;
    }
    pub async fn http(&mut self) -> usize {
        let port = self.url.rsplit(':').next().unwrap().to_owned();
        let child = self.spawn(&["serve", "--no-runner", "--port", &port]);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(response) = http.get(&self.url).send().await
                && response.status().is_success()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "serve did not boot: {}",
                std::fs::read_to_string(self.home.path().join(format!("child-{child}.log")))
                    .unwrap()
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        child
    }
    pub fn cli(&self, args: &[&str]) -> Value {
        let output = Command::new(built("sluice"))
            .args(args)
            .env("SLUICE_HOME", self.home.path())
            .env("SLUICE_FIXTURE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        for group in &self.groups {
            let _ = Command::new("/usr/bin/kill")
                .args(["-KILL", "--", &format!("-{group}")])
                .status();
        }
        for child in self.children.iter_mut().rev() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let evidence = root()
            .join("target/p6-07-processes")
            .join(self.home.path().file_name().unwrap());
        let _ = std::fs::create_dir_all(&evidence);
        for (index, child) in self.children.iter().enumerate() {
            let _ = std::fs::copy(
                self.home.path().join(format!("child-{index}.log")),
                evidence.join(format!("child-{index}-{}.log", child.id())),
            );
        }
        // The coordinator a `tool` call auto-started and the run units created
        // under this scratch home.
        crate::units::stop_home_units(self.home.path());
    }
}
fn config(version: ProtocolVersion) -> ClientConfig {
    ClientConfig::new(
        rmcp::model::ClientCapabilities::default(),
        Implementation::new("sluice-p6-07", "1"),
    )
    .with_protocol_version(version)
}
async fn sdk(url: &str, version: ProtocolVersion) {
    let client = config(version)
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "{url}/mcp"
        )))
        .await
        .unwrap();
    assert!(client.peer_info().is_some());
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 46);
    let result = client
        .call_tool(CallToolRequestParams::new("projects_list").with_arguments(Default::default()))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    assert!(result.structured_content.is_some());
    client.cancel().await.unwrap();
}
#[tokio::test(flavor = "multi_thread")]
#[ignore = "G1b/G5 native binary gate; build workspace first"]
async fn native_boot_cli_lease_restart_no_runner_and_http_sdk() {
    let mut scratch = Scratch::new(tempfile::tempdir().unwrap());
    scratch.boot().await;
    let reply = scratch.cli(&["tool","rpc",r#"{"command":"project_create","args":{"name":"acceptance","description":"Native interface gate","icon":null,"resources":{},"author":"gate"}}"#]);
    assert!(reply.to_string().contains("acceptance"));
    let sluice_model::commands::CommandReply::Project(identity) =
        serde_json::from_value(reply).unwrap()
    else {
        panic!("project identity")
    };
    let project = identity.project_id.to_string();
    let patch = json!({"command":"plan_patch","args":{"project":{"kind":"id","value":project},"rev":1,"ops":[{"op":"replace","path":"","value":{"steps":{"seed":{"run":"core.echo","in":{"value":{"default":7}}}}}}],"start":true,"dry_run":false,"reason":"no-runner gate","author":"gate"}}).to_string();
    scratch.cli(&["tool", "rpc", &patch]);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let reads = sluice_store::ReadPool::open(scratch.home.path(), 1).unwrap();
    assert_eq!(
        reads
            .snapshot(|c| Ok(c.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))?))
            .await
            .unwrap(),
        0
    );
    // No scheduler is hidden inside --no-runner. The actual loop owns the lease.
    let lease = scratch.client.acquire_scheduler().await.unwrap();
    let conflict = scratch.spawn(&["loop"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while scratch.children[conflict].try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(!scratch.children[conflict].wait().unwrap().success());
    assert!(
        std::fs::read_to_string(scratch.home.path().join(format!("child-{conflict}.log")))
            .unwrap()
            .contains("scheduler")
    );
    drop(lease);
    let deadline = Instant::now() + Duration::from_secs(10);
    while reads
        .snapshot(|c| {
            Ok(c.query_row(
                "SELECT scheduler_owner IS NOT NULL FROM maintenance WHERE singleton=1",
                [],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .await
        .unwrap()
    {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let scheduler = scratch.spawn(&["loop"]);
    let deadline = Instant::now() + Duration::from_secs(15);
    while !reads
        .snapshot(|c| {
            Ok(c.query_row(
                "SELECT scheduler_owner IS NOT NULL FROM maintenance WHERE singleton=1",
                [],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .await
        .unwrap()
    {
        assert!(
            scratch.children[scheduler].try_wait().unwrap().is_none(),
            "loop exited: {}",
            std::fs::read_to_string(scratch.home.path().join(format!("child-{scheduler}.log")))
                .unwrap()
        );
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    for version in [ProtocolVersion::V_2025_03_26, ProtocolVersion::V_2024_11_05] {
        sdk(&scratch.url, version).await;
    }
    stdio_sdk(&scratch).await;
    json_and_sse(&scratch.url).await;
    scratch.stop(1);
    let broker_pid = scratch.children[0].id();
    scratch.http().await;
    assert_eq!(scratch.children[0].id(), broker_pid);
    assert!(scratch.children[0].try_wait().unwrap().is_none());
    sdk(&scratch.url, ProtocolVersion::V_2025_03_26).await;
    scratch.stop(scheduler);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "G5 installed Codex/Claude configurations and actual binaries, scratch only"]
async fn installed_clients_use_cloned_configuration() {
    let mut scratch = Scratch::new(tempfile::tempdir().unwrap());
    scratch.boot().await;
    let evidence = root().join("target/p6-07-clients");
    std::fs::create_dir_all(&evidence).unwrap();
    let output = tokio::process::Command::new("uv")
        .args(["run", "python", "-c", REAL_CLIENTS])
        .arg(scratch.home.path())
        .arg(&scratch.url)
        .arg(&evidence)
        .current_dir(root())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
const REAL_CLIENTS: &str = r#"
import json, os, pathlib, shutil, subprocess, sys, tomllib, select, time
scratch, url, evidence = map(pathlib.Path, (sys.argv[1], '.', sys.argv[3]))
url = sys.argv[2]
assert ':3065' not in url
owner=pathlib.Path.home()
config=tomllib.loads((owner/'.codex/config.toml').read_text())
entry=dict(config['mcp_servers']['sluice']);entry['url']=url+'/mcp'
# Only the owner's Sluice entry is needed. Other servers cannot contact live state.
home=scratch/'clients';home.mkdir();codex_home=home/'.codex';codex_home.mkdir()
(codex_home/'config.toml').write_text('[mcp_servers.sluice]\n'+''.join(k+' = '+json.dumps(v)+'\n' for k,v in entry.items() if isinstance(v,(str,bool,int))))
claude_source=json.loads((owner/'.claude.json').read_text())
claude_entry=dict(claude_source['mcpServers']['sluice']);claude_entry['url']=url+'/mcp'
(home/'.claude.json').write_text(json.dumps({'mcpServers':{'sluice':claude_entry},'hasCompletedOnboarding':True}))
(home/'.claude').mkdir(exist_ok=True)
(home/'.claude/.claude.json').write_text((home/'.claude.json').read_text())
(home/'mcp.json').write_text(json.dumps({'mcpServers':{'sluice':claude_entry}}))
for source,target in [(owner/'.codex/auth.json',codex_home/'auth.json'),(owner/'.claude/.credentials.json',home/'.claude/.credentials.json')]:
 if source.exists():
  target.parent.mkdir(exist_ok=True);shutil.copyfile(source,target);target.chmod(0o600)
env=dict(os.environ,HOME=str(home),CODEX_HOME=str(codex_home),CLAUDE_CONFIG_DIR=str(home/'.claude'),ENABLE_TOOL_SEARCH='false',CLAUDE_CODE_DISABLE_CLAUDE_AI_MCP='1')
# App-server's direct tool RPC exercises the actual Codex MCP client without a model turn.
p=subprocess.Popen([shutil.which('codex'),'app-server'],cwd=home,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
sequence=0
try:
 def rpc(method,params):
  global sequence
  sequence+=1;p.stdin.write(json.dumps({'id':sequence,'method':method,'params':params})+'\n');p.stdin.flush()
  deadline=time.monotonic()+45
  while time.monotonic()<deadline:
   if not select.select([p.stdout],[],[],max(0,deadline-time.monotonic()))[0]:raise TimeoutError(method)
   line=p.stdout.readline()
   if not line:raise RuntimeError('Codex exited')
   data=json.loads(line)
   if data.get('id')==sequence:
    assert 'error' not in data,(method,data)
    return data['result']
 rpc('initialize',{'clientInfo':{'name':'sluice-g5','version':'1'},'capabilities':{'experimentalApi':True}})
 p.stdin.write(json.dumps({'method':'initialized','params':{}})+'\n');p.stdin.flush()
 thread=rpc('thread/start',{'cwd':str(home),'approvalPolicy':'never','sandbox':'read-only','ephemeral':True})['thread']['id']
 inventory=rpc('mcpServerStatus/list',{'threadId':thread,'serverName':'sluice'})
 assert 'projects_list' in json.dumps(inventory),inventory
 result=rpc('mcpServer/tool/call',{'threadId':thread,'server':'sluice','tool':'projects_list','arguments':{}})
 assert result.get('isError')!=True,result
 (evidence/'codex.json').write_text(json.dumps({'inventory':inventory,'result':result},indent=2))
 print('Codex initialized, discovered projects_list, called projects_list')
finally:
 p.kill();p.wait()
# Claude's health command proves transport initialization; a restricted print turn
# supplies tool discovery and the read-only call. No other tools or servers exist.
health=subprocess.run([shutil.which('claude'),'mcp','list'],cwd=home,env=env,text=True,capture_output=True,timeout=60)
(evidence/'claude-health.txt').write_text(health.stdout+health.stderr)
assert health.returncode==0 and 'sluice:' in health.stdout and 'Connected' in health.stdout,health.stdout
prompt='Use ToolSearch to discover sluice projects_list, then call mcp__sluice__projects_list exactly once and state its result. These are the only allowed tools.'
run=subprocess.run([shutil.which('claude'),'-p',prompt,'--strict-mcp-config','--mcp-config',str(home/'mcp.json'),'--tools','default','--permission-mode','dontAsk','--allowedTools','ToolSearch,mcp__sluice__projects_list','--output-format','stream-json','--verbose','--max-turns','5','--no-session-persistence','--debug-file',str(evidence/'claude-debug.log')],cwd=home,env=env,text=True,capture_output=True,timeout=120)
(evidence/'claude.jsonl').write_text(run.stdout)
assert run.returncode==0, 'Claude client failed; see scratch evidence'
assert '"name":"mcp__sluice__projects_list"' in run.stdout.replace(' ',''),'Claude did not call the read-only tool'
print('Claude initialized, discovered and called projects_list')
"#;

#[test]
#[ignore = "private stdio child entry point, requires explicit scratch home"]
fn stdio_child() {
    // Only the parent test spawns this entry point; a plain `--ignored` run has nothing to serve.
    let Some(home) = std::env::var_os("SLUICE_ACCEPTANCE_STDIO_HOME").map(PathBuf::from) else {
        return;
    };
    assert!(home.starts_with(std::env::temp_dir()));
    println!("SLUICE_ACCEPTANCE_STDIO_READY");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(sluice_web::mcp::serve_stdio(
            sluice_web::mcp::McpServer::new(std::sync::Arc::new(CoordinatorClient::new(&home))),
        ))
        .unwrap();
    std::process::exit(0);
}
async fn stdio_sdk(scratch: &Scratch) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "clients::stdio_child",
            "--ignored",
            "--nocapture",
        ])
        .env("SLUICE_ACCEPTANCE_STDIO_HOME", scratch.home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            tokio::time::timeout(Duration::from_secs(10), output.read_line(&mut line))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        if line.trim() == "SLUICE_ACCEPTANCE_STDIO_READY" {
            break;
        }
    }
    let client = config(ProtocolVersion::V_2025_03_26)
        .serve((output, input))
        .await
        .unwrap();
    assert_eq!(client.list_all_tools().await.unwrap().len(), 46);
    let result = client
        .call_tool(CallToolRequestParams::new("projects_list").with_arguments(Default::default()))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    client.cancel().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
async fn json_and_sse(url: &str) {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let response=http.post(format!("{url}/mcp")).header("accept","application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"p6-07-wire","version":"1"}}})).send().await.unwrap();
    assert!(response.status().is_success());
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let initialized = wire_response(response, 1).await;
    assert_eq!(
        initialized["result"]["protocolVersion"],
        json!("2025-03-26")
    );
    let response = http
        .post(format!("{url}/mcp"))
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-03-26")
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let response=http.post(format!("{url}/mcp")).header("accept","application/json, text/event-stream")
        .header("mcp-session-id",&session).header("mcp-protocol-version","2025-03-26")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"projects_list","arguments":{}}})).send().await.unwrap();
    assert!(response.status().is_success());
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let result = wire_response(response, 2).await;
    assert_eq!(result["id"], json!(2));
    assert!(result.get("result").is_some());
    assert!(
        http.delete(format!("{url}/mcp"))
            .header("mcp-session-id", session)
            .header("mcp-protocol-version", "2025-03-26")
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    // Modern stateless discovery uses JSON; legacy session requests above use SSE.
    let response = http
        .post(format!("{url}/mcp"))
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("Mcp-Method", "tools/list")
        .json(&json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"p6-07-json","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}))
        .send()
        .await
        .unwrap();
    if !response.status().is_success() {
        panic!(
            "modern JSON: {} {}",
            response.status(),
            response.text().await.unwrap()
        );
    }
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let result = response.json::<Value>().await.unwrap();
    assert_eq!(result["result"]["ttlMs"], json!(0));
    assert_eq!(result["result"]["cacheScope"], json!("private"));
    assert_eq!(result["result"]["tools"].as_array().unwrap().len(), 46);
}

async fn wire_response(response: reqwest::Response, id: i64) -> Value {
    if response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json")
    {
        return response.json().await.unwrap();
    }
    use futures_util::StreamExt;
    let mut chunks = response.bytes_stream();
    let mut wire = String::new();
    loop {
        let bytes = tokio::time::timeout(Duration::from_secs(10), chunks.next())
            .await
            .unwrap()
            .expect("SSE ended before response")
            .unwrap();
        wire.push_str(std::str::from_utf8(&bytes).unwrap());
        for line in wire.lines() {
            if let Some(json) = line.strip_prefix("data: ")
                && let Ok(value) = serde_json::from_str::<Value>(json)
                && value["id"] == serde_json::json!(id)
            {
                return value;
            }
        }
    }
}
