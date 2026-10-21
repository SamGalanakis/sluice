//! A form that writes (DESIGN.md, Writes): it posts through Datastar
//! (<https://data-star.dev/reference/actions#post>, `contentType: 'form'`, which sends the
//! closest form's fields and the button that sent it), naming itself in the `sluice-form`
//! header so the answer (`views::write`) finds its notice. While the write is in flight its
//! `data-indicator` signal (<https://data-star.dev/reference/attributes#data-indicator>) is true
//! and `data-attr` says so as `aria-busy`: the button that sent it (the submit event's
//! `submitter`, marked `data-pressed` until the action settles) turns a quiet ring, and the
//! form's other buttons wait. `data-on:submit` keeps the browser's own submission from running
//! (<https://data-star.dev/reference/attributes#data-on>). A write is tried once (`retry:
//! 'never'`, `retryMaxCount: 0`): one that never reached sluice rejects the action, and the
//! rejection is said in the form's notice at once, for the owner to send again. Inside a
//! Rocket component a signal is the
//! component's own unless its attribute says `__root`
//! (<https://data-star.dev/reference/rocket>): the busy signal is the page's.
use super::esc;
use crate::views::TrustedHtml;
use std::hash::{DefaultHasher, Hash, Hasher};

/// The opening tag of a form with `id` and `class` that posts its fields to `action`.
pub fn post_form(id: &str, class: &str, action: &str) -> String {
    post_form_with(id, class, action, "", "")
}
/// `post_form` that first runs `first` (a statement ending in `;`, as written in HTML) and has
/// more attributes (`extra`, each with its leading space), both already escaped.
pub fn post_form_with(id: &str, class: &str, action: &str, first: &str, extra: &str) -> String {
    let busy = busy_signal(id);
    format!(
        "<form id=\"{id}\"{class} method=\"post\" action=\"{action}\" data-on:submit=\"{first}evt.submitter?.setAttribute('data-pressed', ''); @post(el.getAttribute('action'), {{contentType: 'form', headers: {{'sluice-form': el.id}}, retry: 'never', retryMaxCount: 0}}).catch(() =&gt; {UNSENT}).finally(() =&gt; evt.submitter?.removeAttribute('data-pressed'))\" data-indicator__root=\"{busy}\" data-attr:aria-busy=\"${busy} &amp;&amp; 'true'\"{extra}>",
        first = if first.is_empty() {
            String::new()
        } else {
            format!("{first} ")
        },
        id = esc(id),
        class = if class.is_empty() {
            String::new()
        } else {
            format!(" class=\"{}\"", esc(class))
        },
        action = esc(action),
    )
}
/// What a form's notice says when its write never reached sluice (sluice cannot say it): its
/// action's rejection, as an alert.
const UNSENT: &str = "{ const said = document.getElementById(el.id + '-said'); if (said) { said.setAttribute('role', 'alert'); said.textContent = 'Sluice did not answer: is it running? Try again in a moment.'; } }";
/// The notice a form's answer fills (`views::write::notice`), empty until then: it goes last
/// in the form.
pub fn form_said(id: &str) -> String {
    crate::views::write::notice_html(id, "")
}
/// An id for a form that has none of its own, from what makes it the one it is (its action
/// and the items it acts on): the same form is drawn with the same id on every render, so a
/// stream's patch keeps it.
pub fn form_id(seed: &str) -> String {
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    format!("w-{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}
/// A message form's local signals (`_`, never sent: the form's fields are): its words and its
/// "ask" box, bound to them so the answer to a message sent empties them (`write::sent`).
pub fn draft_signals(id: &str) -> (String, String) {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    let h = hasher.finish() & 0xff_ffff_ffff;
    (format!("_draft{h:010x}"), format!("_ask{h:010x}"))
}
/// The local signal (`_`, never sent) that says the form's write is in flight.
fn busy_signal(id: &str) -> String {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    format!("_busy{:010x}", hasher.finish() & 0xff_ffff_ffff)
}
/// The `@get` that opens a page's one long-lived stream at `url`
/// (<https://data-star.dev/reference/actions#get>): retried as long as the page is open, and
/// cancelled when its element goes (`requestCancellation: 'cleanup'`) or when the same stream is
/// asked for again (`'auto'`'s same URL and method, which `'cleanup'` keeps).
pub fn stream_get(url: &str) -> String {
    get(url, ", requestCancellation: 'cleanup'")
}
/// The same `@get` from an element that reopens the page's stream on its signals as they are
/// now (the plan's find, Mark as seen, Reconnect): it ends the open one (`'auto'`), and lives on
/// when its own element goes. The element carries `data-reopens-stream`, so the stream line
/// (`sluice-banner`) counts it as the page's.
pub fn stream_reopen(url: &str) -> String {
    get(url, "")
}
fn get(url: &str, more: &str) -> String {
    format!(
        "@get('{}', {{retry: 'always', retryMaxCount: 10, retryMaxWait: 30000{more}}})",
        url.replace('\\', "\\\\").replace('\'', "\\'")
    )
}

/// The sidebar's find (DESIGN.md, The frame): a search field reading "Find steps" with its `/`,
/// a form getting `action` with `q`. `value` is the find it holds; `attrs` more attributes on
/// its input (the plan's: its `q` signal and the stream it reopens), already escaped; `more`
/// what the form carries besides (hidden fields, the clear link).
pub fn find_field(
    action: &str,
    value: &str,
    placeholder: &str,
    label: &str,
    attrs: &str,
) -> TrustedHtml {
    find_form(action, value, placeholder, label, attrs, "")
}
/// `find_field` with more in the form after its input.
pub fn find_form(
    action: &str,
    value: &str,
    placeholder: &str,
    label: &str,
    attrs: &str,
    more: &str,
) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<form class=\"palette\" method=\"get\" action=\"{}\" role=\"search\" aria-label=\"{}\">{}<input type=\"search\" name=\"q\" value=\"{}\" placeholder=\"{}\" title=\"{} (/)\" aria-label=\"{}\" data-find autocomplete=\"off\" spellcheck=\"false\" enterkeyhint=\"search\"{attrs}><kbd aria-hidden=\"true\">/</kbd>{more}</form>",
        super::esc(action),
        super::esc(label),
        crate::views::icons::icon(crate::views::icons::Icon::Search, 16, ""),
        super::esc(value),
        super::esc(placeholder),
        super::esc(placeholder),
        super::esc(label),
    ))
}
