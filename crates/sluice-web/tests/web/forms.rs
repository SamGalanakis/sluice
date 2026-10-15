//! A page's form as Datastar sends it (`ui::post_form`): its fields url-encoded and its id in
//! the `sluice-form` header, so the answer is the form's (`views::write`).
use axum::body::{Body, to_bytes};
use axum::http::Request;

/// `pairs` as a form's url-encoded body.
pub fn encoded(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}
/// A POST of `pairs` to `uri` from the page's form `form`.
pub fn post(uri: impl AsRef<str>, form: &str, pairs: &[(&str, &str)]) -> Request<Body> {
    Request::post(uri.as_ref())
        .header("content-type", "application/x-www-form-urlencoded")
        .header("datastar-request", "true")
        .header("sluice-form", form)
        .body(Body::from(encoded(pairs)))
        .unwrap()
}
/// A response's body as text: a Datastar answer's events, or a page.
pub async fn text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}
