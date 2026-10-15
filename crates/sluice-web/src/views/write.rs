//! A write's answer (DESIGN.md, Writes). Every form on the dashboard posts through Datastar
//! (`ui::post_form`: `@post` with `contentType: 'form'`), so a write is a short request beside
//! the page's one long-lived stream (the Tao of Datastar's CQRS): the stream draws what the
//! write changed, and the write's own answer says only what the stream cannot, its form's
//! notice. A write that went through clears that notice (and closes the confirmation it was
//! sent from); one sluice refused fills it. Datastar applies an answer only when its status is
//! 200, so a refusal to a form is a 200 that says so; a client that is not a page's form (a
//! script, a test) gets the JSON error with its status instead.
use super::ui::esc;
use crate::streams::StreamEvent;
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse},
};
use datastar::prelude::{ElementPatchMode, ExecuteScript, PatchElements};
use futures_util::stream;
use sluice_model::error::PublicError;
use std::convert::Infallible;

/// The header a page's form names itself in (`ui::post_form`), so its answer finds its notice.
pub const FORM: &str = "sluice-form";

/// The form that sent this request, when a page's form did: its id, as `ui::post_form` drew it.
pub fn form(headers: &HeaderMap) -> Option<String> {
    let id = headers.get(FORM)?.to_str().ok()?;
    (!id.is_empty()
        && id.len() <= 120
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| id.to_owned())
}
/// The answer to a write sluice took: the form's notice emptied and its confirmation closed;
/// what changed comes with the page's stream. A client that is not a page's form gets 204.
pub fn done(headers: &HeaderMap) -> Response {
    match form(headers) {
        Some(id) => events([notice(&id, ""), close(headers)]),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
/// The confirmation the request's form was sent from, closed: a confirmation's form sits in
/// the page's one dialog while it is open. Nothing for a form that is not in one.
pub fn close(headers: &HeaderMap) -> StreamEvent {
    let id = form(headers).unwrap_or_default();
    StreamEvent::Script(ExecuteScript::new(format!(
        "document.getElementById('{id}')?.closest('dialog[open]')?.close()"
    )))
}
/// Form `form`'s notice saying `words` as a status (a write it took).
pub fn status(form: &str, words: &str) -> StreamEvent {
    whole(format!(
        "<p id=\"{}-said\" class=\"form-said\" role=\"status\">{}</p>",
        esc(form),
        esc(words)
    ))
}
/// A notice put in place whole (`replace`, not morphed): a confirmation's form sits in the
/// page's one dialog, which no patch morphs (`data-ignore-morph`), while it is open.
fn whole(html: String) -> StreamEvent {
    StreamEvent::Elements(PatchElements::new(html).mode(ElementPatchMode::Replace))
}
/// The answer to a write sluice took whose form says so: `words`, as a status.
pub fn said(headers: &HeaderMap, words: &str) -> Response {
    match form(headers) {
        Some(id) => events([status(&id, words)]),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
/// The answer to a message sluice took from its form: the form says `words` (as a status) and
/// is emptied for the next one, the message itself coming with the page's stream. A client that
/// is not a page's form gets 204.
pub fn sent(headers: &HeaderMap, words: &str) -> Response {
    match form(headers) {
        Some(id) => events([
            status(&id, words),
            StreamEvent::Script(ExecuteScript::new(format!(
                "document.getElementById('{id}')?.reset()"
            ))),
        ]),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
/// The answer to a write sluice refused: `words` in its form's notice, as an alert. A client
/// that is not a page's form gets `error` as JSON with its status.
pub fn refused(headers: &HeaderMap, words: &str, error: PublicError) -> Response {
    match form(headers) {
        Some(id) => events([notice(&id, words)]),
        None => crate::http::error_response(error),
    }
}
/// The answer to a write whose refusal is `error` alone: its message is what the form says.
pub fn failed(headers: &HeaderMap, error: PublicError) -> Response {
    let words = error.to_string();
    refused(headers, &words, error)
}
/// `events` as one short Datastar answer.
pub fn events(events: impl IntoIterator<Item = StreamEvent>) -> Response {
    let events: Vec<_> = events
        .into_iter()
        .map(|e| Ok::<_, Infallible>(e.axum_event()))
        .collect();
    Sse::new(stream::iter(events)).into_response()
}
/// A form's notice (`ui::post_form`'s `<id>-said`): `words` as an alert, or empty.
pub fn notice(form: &str, words: &str) -> StreamEvent {
    whole(notice_html(form, words))
}
/// A form's notice as drawn: empty until a write says something.
pub fn notice_html(form: &str, words: &str) -> String {
    if words.is_empty() {
        format!(
            "<p id=\"{}-said\" class=\"form-said\" role=\"status\"></p>",
            esc(form)
        )
    } else {
        format!(
            "<p id=\"{}-said\" class=\"form-said\" role=\"alert\">{}</p>",
            esc(form),
            esc(words)
        )
    }
}
