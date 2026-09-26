"""linear.update against a local fake of the Linear GraphQL endpoint."""

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest
from helpers import run_fn

ISSUE = {"id": "uuid-1", "identifier": "FIG-1", "state": {"name": "In Progress"},
         "team": {"states": {"nodes": [{"id": "st-1", "name": "In Progress"},
                                       {"id": "st-2", "name": "In Review"}]}}}


@pytest.fixture
def linear():
    requests: list[dict] = []
    status = {"code": 200}

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append({"auth": self.headers.get("Authorization"), "body": body})
            q = body["query"]
            if "issueUpdate" in q:
                data = {"issueUpdate": {"success": True,
                                        "issue": {"state": {"name": "In Review"}}}}
            elif "commentCreate" in q:
                data = {"commentCreate": {"success": True, "comment": {"id": "c-9"}}}
            else:
                data = {"issue": ISSUE if body["variables"]["id"] == "FIG-1" else None}
            raw = json.dumps({"data": data}).encode()
            self.send_response(status["code"])
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def log_message(self, *a):
            pass

    srv = HTTPServer(("127.0.0.1", 0), Handler)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    env = {"SLUICE_LINEAR_URL": f"http://127.0.0.1:{srv.server_port}/graphql",
           "LINEAR_API_KEY": "lin_test_key"}
    yield env, requests, status
    srv.shutdown()


def test_sets_state_and_comments(tmp_path, linear):
    env, requests, _ = linear
    r = run_fn("linear.update", {"issue": "FIG-1", "state": "in review",
                                 "comment": "PR: https://example/pr/1"}, tmp_path, env)
    assert r.code == 0, r.stderr
    assert r.out == {"issue_id": "uuid-1", "identifier": "FIG-1", "state": "In Review",
                     "comment_id": "c-9"}
    assert [q["auth"] for q in requests] == ["lin_test_key"] * 3
    lookup, update, comment = (q["body"] for q in requests)
    assert lookup["variables"] == {"id": "FIG-1"}
    assert "issueUpdate" in update["query"]
    assert update["variables"] == {"id": "uuid-1", "stateId": "st-2"}
    assert "commentCreate" in comment["query"]
    assert comment["variables"] == {"issueId": "uuid-1", "body": "PR: https://example/pr/1"}


def test_comment_only_does_not_touch_state(tmp_path, linear):
    env, requests, _ = linear
    r = run_fn("linear.update", {"issue": "FIG-1", "comment": "note"}, tmp_path, env)
    assert r.code == 0, r.stderr
    assert r.out["state"] == "In Progress"
    assert not any("issueUpdate" in q["body"]["query"] for q in requests)


def test_unknown_state_fails_before_any_mutation(tmp_path, linear):
    env, requests, _ = linear
    r = run_fn("linear.update", {"issue": "FIG-1", "state": "Shipped", "comment": "x"},
               tmp_path, env)
    assert r.code == 1
    assert "no state 'Shipped'" in r.error["message"]
    assert len(requests) == 1


def test_missing_issue_and_missing_key_fail(tmp_path, linear):
    env, _, _ = linear
    r = run_fn("linear.update", {"issue": "FIG-404", "comment": "x"}, tmp_path, env)
    assert r.code == 1 and "not found" in r.error["message"]
    r = run_fn("linear.update", {"issue": "FIG-1", "comment": "x"}, tmp_path,
               {**env, "LINEAR_API_KEY": ""})
    assert r.code == 1 and "LINEAR_API_KEY" in r.error["message"]


def test_server_errors_are_transient(tmp_path, linear):
    env, _, status = linear
    status["code"] = 503
    r = run_fn("linear.update", {"issue": "FIG-1", "comment": "x"}, tmp_path, env)
    assert r.code == 75
