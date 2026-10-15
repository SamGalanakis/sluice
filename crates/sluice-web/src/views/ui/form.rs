//! A form that writes (DESIGN.md, Writes): it posts through Datastar
//! (<https://data-star.dev/reference/actions#post>, `contentType: 'form'`, which sends the
//! closest form's fields and the button that sent it), naming itself in the `sluice-form`
//! header so the answer (`views::write`) finds its notice. While the write is in flight its
//! `data-indicator` signal (<https://data-star.dev/reference/attributes#data-indicator>) is true
//! and `data-attr` says so as `aria-busy`, which the stylesheet draws as a quiet spinner on its
//! buttons. `data-on:submit` keeps the browser's own submission from running
//! (<https://data-star.dev/reference/attributes#data-on>). A write is tried once (`retry:
//! 'never'`, `retryMaxCount: 0`): one that never reached sluice is said in its notice at once
//! (components.js), for the owner to send again; the action's rejection is that, so it is
//! caught. Inside a Rocket component a signal is the
//! component's own unless its attribute says `__root`
//! (<https://data-star.dev/reference/rocket>): the busy signal is the page's.
use super::esc;
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
        "<form id=\"{id}\"{class} method=\"post\" action=\"{action}\" data-on:submit=\"{first}@post(el.getAttribute('action'), {{contentType: 'form', headers: {{'sluice-form': el.id}}, retry: 'never', retryMaxCount: 0}}).catch(() =&gt; {{}})\" data-indicator__root=\"{busy}\" data-attr:aria-busy=\"${busy} &amp;&amp; 'true'\"{extra}>",
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
