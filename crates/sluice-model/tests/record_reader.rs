use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest, Next, NextResult, RecordPage, Settles},
    events::Event,
    ids::{ProjectId, ProjectSelector},
    rpc::decode_json,
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Server {
    children: Vec<Child>,
    home: tempfile::TempDir,
    binary: PathBuf,
    port: u16,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, 3065);
        let binary = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("sluice");
        assert!(
            binary.is_file(),
            "build this worktree's sluice binary first"
        );
        let mut server = Self {
            children: vec![],
            home: tempfile::tempdir().unwrap(),
            binary,
            port,
        };
        server.spawn(&["coordinator"]);
        let deadline = Instant::now() + Duration::from_secs(20);
        while UnixStream::connect(server.home.path().join("coordinator.sock")).is_err() {
            server.wait(deadline);
        }
        drop(listener);
        server.spawn(&["serve", "--no-runner", "--port", &port.to_string()]);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            server.wait(deadline);
        }
        server
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env("SLUICE_HOME", self.home.path())
            .env("SLUICE_FIXTURE", "1")
            .stdin(Stdio::null());
        command
    }
    fn spawn(&mut self, args: &[&str]) {
        self.children.push(
            self.command()
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
    }
    fn wait(&mut self, deadline: Instant) {
        for child in &mut self.children {
            assert!(child.try_wait().unwrap().is_none(), "scratch child exited");
        }
        assert!(
            Instant::now() < deadline,
            "scratch server startup timed out"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    fn http(&self, tool: &str, args: Value) -> Vec<u8> {
        let body = serde_json::to_string(&args).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream,
            "POST /api/tools/{tool} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.port, body.len()
        ).unwrap();
        let mut response = vec![];
        stream.read_to_end(&mut response).unwrap();
        let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let headers = std::str::from_utf8(&response[..split]).unwrap();
        assert!(
            headers.starts_with("HTTP/1.1 200 "),
            "{}",
            String::from_utf8_lossy(&response)
        );
        let mut body = &response[split + 4..];
        if headers
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            let mut decoded = vec![];
            loop {
                let end = body.windows(2).position(|w| w == b"\r\n").unwrap();
                let size = usize::from_str_radix(
                    std::str::from_utf8(&body[..end])
                        .unwrap()
                        .split(';')
                        .next()
                        .unwrap(),
                    16,
                )
                .unwrap();
                body = &body[end + 2..];
                if size == 0 {
                    return decoded;
                }
                decoded.extend_from_slice(&body[..size]);
                assert_eq!(&body[size..size + 2], b"\r\n");
                body = &body[size + 2..];
            }
        }
        assert!(
            headers
                .to_ascii_lowercase()
                .contains(&format!("content-length: {}", body.len())),
            "{headers}"
        );
        body.to_vec()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        for child in self.children.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn assert_messages(result: &NextResult, id: ProjectId) {
    assert!(!result.timed_out);
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.notes.len(), 1);
    for (record, body) in [
        (&result.notes[0], "note after rename"),
        (&result.records[0], "question after rename"),
    ] {
        assert_eq!(record.project, Some(id));
        let Event::Message(message) = &record.event else {
            panic!("expected message")
        };
        assert_eq!(message.body, body);
        assert_eq!(message.id.0, record.seq.0);
        assert!(!message.at.is_empty());
        assert!(!record.at.is_empty());
    }
}

#[test]
#[ignore = "native next/HTTP gate; build this worktree binary with cargo build --workspace first"]
fn native_next_and_http_read_message_records_across_project_rename() {
    let server = Server::new();
    let created: Value = decode_json(&server.http(
        "project_create",
        json!({"name":"record-at", "description":"scratch timestamp regression"}),
    ))
    .unwrap();
    let id: ProjectId = serde_json::from_value(created["project_id"].clone()).unwrap();
    let page: RecordPage =
        decode_json(&server.http("log_read", json!({"project":format!("id:{id}")}))).unwrap();
    let cursor = page.last_seq;
    server.http(
        "project_update",
        json!({"project":format!("id:{id}"), "new_name":"renamed-record-at"}),
    );
    for (body, needs_reply) in [
        ("note after rename", false),
        ("question after rename", true),
    ] {
        server.http("message_post", json!({"project":"renamed-record-at", "thread":"step-record-at", "from":"worker", "to":"orchestrator", "body":body, "needs_reply":needs_reply}));
    }

    let request = CommandRequest::Next(Next {
        projects: vec![ProjectSelector::Id(id)],
        since_seq: cursor,
        me: "orchestrator".into(),
        timeout_seconds: 1,
        all: false,
        settle_seconds: 0,
        settle_max_seconds: 0,
        settles: Settles::Full,
    });
    let output = server
        .command()
        .args(["tool", "rpc", &serde_json::to_string(&request).unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let CommandReply::Next(native) = decode_json(&output.stdout).unwrap() else {
        panic!("expected next reply")
    };
    assert_messages(&native, id);
    let http: NextResult = decode_json(&server.http("next", json!({"projects":[format!("id:{id}")], "since_seq":cursor, "timeout":1, "settle":0, "settle_max":0}))).unwrap();
    assert_eq!(http, native);
    let log: RecordPage = decode_json(&server.http(
        "log_read",
        json!({"project":"renamed-record-at", "since_seq":cursor, "kinds":["message"]}),
    ))
    .unwrap();
    assert_eq!(
        log.records,
        vec![native.notes[0].clone(), native.records[0].clone()]
    );
}
