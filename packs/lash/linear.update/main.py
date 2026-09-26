# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""linear.update: move an issue to a state and/or comment on it (Linear GraphQL)."""

import json
import os
import urllib.error
import urllib.request

from sluice.fn import Transient, run

DEFAULT_URL = "https://api.linear.app/graphql"

ISSUE_QUERY = """query Issue($id: String!) {
  issue(id: $id) { id identifier state { name } team { states { nodes { id name } } } }
}"""
SET_STATE = """mutation SetState($id: String!, $stateId: String!) {
  issueUpdate(id: $id, input: {stateId: $stateId}) { success issue { state { name } } }
}"""
ADD_COMMENT = """mutation AddComment($issueId: String!, $body: String!) {
  commentCreate(input: {issueId: $issueId, body: $body}) { success comment { id } }
}"""


def gql(query: str, variables: dict) -> dict:
    key = os.environ.get("LINEAR_API_KEY")
    if not key:
        raise RuntimeError("LINEAR_API_KEY is not set")
    req = urllib.request.Request(
        os.environ.get("SLUICE_LINEAR_URL", DEFAULT_URL),
        data=json.dumps({"query": query, "variables": variables}).encode(),
        headers={"Content-Type": "application/json", "Authorization": key},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            doc = json.loads(resp.read())
    except urllib.error.HTTPError as e:
        if e.code == 429 or e.code >= 500:
            raise Transient(f"Linear HTTP {e.code}") from e
        raise RuntimeError(f"Linear HTTP {e.code}: {e.read()[:500]!r}") from e
    except urllib.error.URLError as e:
        raise Transient(f"Linear unreachable: {e.reason}") from e
    if doc.get("errors"):
        msgs = "; ".join(str(err.get("message")) for err in doc["errors"])
        if "ratelimit" in msgs.lower().replace(" ", "").replace("_", ""):
            raise Transient(f"Linear rate limit: {msgs}")
        raise RuntimeError(f"Linear GraphQL error: {msgs}")
    return doc["data"]


def main(inp, ctx):
    state, comment = inp.get("state"), inp.get("comment")
    if not state and not comment:
        raise ValueError("linear.update needs a state, a comment, or both")
    issue = gql(ISSUE_QUERY, {"id": inp["issue"]})["issue"]
    if not issue:
        raise RuntimeError(f"Linear issue {inp['issue']} not found")
    out = {"issue_id": issue["id"], "identifier": issue["identifier"],
           "state": (issue.get("state") or {}).get("name"), "comment_id": None}
    if state:
        states = issue["team"]["states"]["nodes"]
        match = [s for s in states if s["name"].lower() == state.lower()]
        if not match:
            names = ", ".join(s["name"] for s in states)
            raise ValueError(f"no state {state!r} in {issue['identifier']}'s team ({names})")
        res = gql(SET_STATE, {"id": issue["id"], "stateId": match[0]["id"]})["issueUpdate"]
        if not res["success"]:
            raise RuntimeError(f"Linear refused the state change on {issue['identifier']}")
        out["state"] = res["issue"]["state"]["name"]
    if comment:
        res = gql(ADD_COMMENT, {"issueId": issue["id"], "body": comment})["commentCreate"]
        if not res["success"]:
            raise RuntimeError(f"Linear refused the comment on {issue['identifier']}")
        out["comment_id"] = res["comment"]["id"]
    return out


if __name__ == "__main__":
    run(main)
