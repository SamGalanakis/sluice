"""jev.* fns against the real TypeSafe API. The key comes from the environment or the repo's
.env (TYPESAFE_API_KEY); without one these tests are skipped."""

import os
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
FNS = Path(__file__).resolve().parents[1]


def _key():
    if os.environ.get("TYPESAFE_API_KEY"):
        return os.environ["TYPESAFE_API_KEY"]
    env = REPO / ".env"
    if env.exists():
        for line in env.read_text().splitlines():
            if line.startswith("TYPESAFE_API_KEY="):
                return line.split("=", 1)[1].strip()
    return None


KEY = _key()
needs_key = pytest.mark.skipif(not KEY, reason="no TYPESAFE_API_KEY in env or .env")
TICKET = "Help! My payouts have been failing for 3 days and I have payroll tomorrow."


def _call(call_fn, name, inp, key=KEY):
    return call_fn(FNS / name, inp, env={"TYPESAFE_API_KEY": key or ""})


@needs_key
def test_choice_picks_the_obvious_option(call_fn):
    code, out, err = _call(call_fn, "jev.choice", {
        "state": TICKET, "instructions": "Which team should handle this?",
        "options": {"billing": "Payments, payouts, refunds", "sales": "Pricing, new accounts",
                    "design": "Logos and branding"}})
    assert code == 0, err
    assert out["choice"] == "billing"
    assert set(out["probabilities"]) == {"billing", "sales", "design"}
    assert out["probabilities"]["billing"] > 0.5
    assert out["confident"] == (out["confidence"] >= 0.8)
    assert out["model"].startswith("jev")


@needs_key
def test_choice_accepts_a_plain_list_and_threshold(call_fn):
    code, out, err = _call(call_fn, "jev.choice", {
        "state": TICKET, "instructions": "Is the customer asking about money or about colours?",
        "options": ["money", "colours"], "min_confidence": 0.0})
    assert code == 0, err
    assert out["choice"] == "money"
    assert out["confident"] is True  # any confidence clears a 0.0 threshold


@needs_key
def test_noul_separates_yes_from_no(call_fn):
    _, urgent, err = _call(call_fn, "jev.noul", {
        "state": TICKET, "instructions": "Is this urgent?",
        "yes": "Time-sensitive", "no": "Can wait"})
    assert urgent is not None, err
    _, calm, err = _call(call_fn, "jev.noul", {
        "state": "Just curious whether you have a dark mode, no rush at all.",
        "instructions": "Is this urgent?"})
    assert calm is not None, err
    assert urgent["noul"] > 0.5 > calm["noul"]


@needs_key
def test_score_uses_the_given_levels(call_fn):
    code, out, err = _call(call_fn, "jev.score", {
        "state": TICKET, "instructions": "How stressed is the customer?",
        "levels": ["Calm", "Worried", "Panicking"]})
    assert code == 0, err
    assert 0.0 <= out["score"] <= 2.0
    assert out["legend"] == {"0": "Calm", "1": "Worried", "2": "Panicking"}
    assert out["score"] > 0.5  # clearly not calm


@needs_key
def test_ask_answers_several_questions_in_one_call(call_fn):
    code, out, err = _call(call_fn, "jev.ask", {
        "state": TICKET,
        "questions": {
            "team": {"type": "choice", "instructions": "Which team?",
                     "criteria": {"billing": None, "sales": None}},
            "urgent": {"type": "noul", "instructions": "Is this urgent?"}}})
    assert code == 0, err
    assert set(out["answers"]) == {"team", "urgent"}
    assert out["answers"]["team"]["choice"] == "billing"
    assert out["usage"]["input_tokens"] > 0


def test_missing_key_fails_without_calling_out(call_fn):
    code, out, err = _call(call_fn, "jev.noul", {"state": "x", "instructions": "y?"}, key="")
    assert code == 1 and out is None
    assert "TYPESAFE_API_KEY is not set" in err


def test_bad_key_fails_with_the_api_error_and_does_not_echo_it(call_fn):
    bad = "apikey_not_a_real_key_0000"
    code, out, err = _call(call_fn, "jev.noul", {"state": "x", "instructions": "y?"}, key=bad)
    assert code == 1 and out is None
    assert "typesafe 401" in err
    assert bad not in err


def test_score_rejects_too_few_levels_before_calling(call_fn):
    code, out, err = _call(call_fn, "jev.score",
                           {"state": "x", "instructions": "y?", "levels": ["only"]}, key="k")
    assert code == 1 and out is None
    assert "levels must have 2 to 10 entries" in err
