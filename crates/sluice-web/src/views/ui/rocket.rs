//! The dashboard's components (DESIGN.md, Components): Rust draws each one's host element and
//! everything in it, and a Rocket component of the same name (`assets/components.js`, or
//! `assets/sluice.js` for the board and the drawer) adds its behaviour in light DOM, through a
//! `setup` that enhances what the server drew and never draws content of its own. A page reads
//! whole before its script runs and without it, a stream patch morphs server HTML into server
//! HTML (each host's state is an attribute kept through the patch, `data-preserve-attr`), and a
//! screen reader hears what the server wrote. A page makes a host only through these functions.
use super::{Confirm, esc};
use crate::views::TrustedHtml;
use crate::views::icons::{Icon, icon};

/// A component's host element: its tag, its attributes (props are plain attributes, as the
/// component declares them; `data-*` ones are the page's), and the attributes a stream patch
/// leaves as the script set them.
#[derive(Clone, Debug)]
pub struct Host {
    tag: &'static str,
    attrs: Vec<(String, Option<String>)>,
    keep: Vec<&'static str>,
}
impl Host {
    pub fn new(tag: &'static str) -> Self {
        debug_assert!(COMPONENTS.iter().any(|c| c.tag == tag), "{tag} is listed");
        Self {
            tag,
            attrs: vec![],
            keep: vec![],
        }
    }
    /// An attribute with a value (escaped); an empty `name` value is still written.
    pub fn attr(mut self, name: &str, value: impl AsRef<str>) -> Self {
        self.attrs
            .push((name.to_owned(), Some(value.as_ref().to_owned())));
        self
    }
    /// An attribute with a value, left out when the value is empty.
    pub fn some(self, name: &str, value: impl AsRef<str>) -> Self {
        if value.as_ref().is_empty() {
            self
        } else {
            self.attr(name, value)
        }
    }
    /// A boolean attribute: present or not.
    pub fn flag(mut self, name: &str, on: bool) -> Self {
        if on {
            self.attrs.push((name.to_owned(), None));
        }
        self
    }
    /// An attribute the script owns once the page is drawn: a patch never resets it.
    pub fn keep(mut self, name: &'static str) -> Self {
        self.keep.push(name);
        self
    }
    pub fn open(&self) -> TrustedHtml {
        let mut out = format!("<{}", self.tag);
        for (name, value) in &self.attrs {
            match value {
                Some(v) => out.push_str(&format!(" {name}=\"{}\"", esc(v))),
                None => out.push_str(&format!(" {name}")),
            }
        }
        if !self.keep.is_empty() {
            out.push_str(&format!(" data-preserve-attr=\"{}\"", self.keep.join(" ")));
        }
        out.push('>');
        TrustedHtml::owned(out)
    }
    pub fn close(&self) -> TrustedHtml {
        TrustedHtml::owned(format!("</{}>", self.tag))
    }
    /// The host around `inner`.
    pub fn wrap(&self, inner: &TrustedHtml) -> TrustedHtml {
        TrustedHtml::owned(format!("{}{inner}{}", self.open(), self.close()))
    }
}

/// A component as its script declares it: its props (as attributes, with their codec), the
/// events it sends and the parts the server draws in it. `/_ui` lists them; a Chromium test
/// holds each one to its script's `manifest()`.
#[derive(Clone, Copy, Debug)]
pub struct Component {
    pub tag: &'static str,
    /// The asset whose script defines it.
    pub script: &'static str,
    pub does: &'static str,
    pub props: &'static [(&'static str, &'static str)],
    pub events: &'static [&'static str],
    pub slots: &'static [&'static str],
}
pub const COMPONENTS: &[Component] = &[
    Component {
        tag: "sluice-tabs",
        script: "components.js",
        does: "An ARIA tablist over its panels: arrows wrap, Home and End; the chosen tab in $$tab, the page's tab signal and, with url, ?tab=; an anchor in a panel opens its tab.",
        props: &[("current", "string"), ("url", "boolean")],
        events: &["sluice-tab"],
        slots: &["tabbar", "panels"],
    },
    Component {
        tag: "sluice-fold",
        script: "components.js",
        does: "Show all and Show less over a long text (said only when it is cut), or a More fold kept per key or opened on a wide screen; folding brings its head back into view.",
        props: &[("remember", "string"), ("wide", "boolean")],
        events: &[],
        slots: &["clip", "toggle", "details"],
    },
    Component {
        tag: "sluice-confirm",
        script: "components.js",
        does: "Its summary opens the page's one dialog with its form; focus starts on the keep button, stays in the dialog and goes back to the opener.",
        props: &[
            ("heading", "string"),
            ("ref-id", "string"),
            ("disabled", "boolean"),
        ],
        events: &["sluice-confirm-open", "sluice-confirm-close"],
        slots: &["summary", "form"],
    },
    Component {
        tag: "sluice-conversation",
        script: "components.js",
        does: "Jump to latest, a thread's page opening at its end, and Mark read: a note is read only when asked, the mark covering what the page drew.",
        props: &[("start", "string")],
        events: &["sluice-read"],
        slots: &["lead", "list"],
    },
    Component {
        tag: "sluice-composer",
        script: "components.js",
        does: "Sends a note or a question as JSON and stays on the page, keeping what is typed through a patch; Ctrl or Cmd with Enter sends.",
        props: &[],
        events: &["sluice-sent"],
        slots: &["form"],
    },
    Component {
        tag: "sluice-answer",
        script: "components.js",
        does: "A question's Answer opens its box (the question's own form, drawn by openui.js from its program), and Close question closes it at once.",
        props: &[],
        events: &[],
        slots: &["actions", "box"],
    },
    Component {
        tag: "sluice-menu",
        script: "components.js",
        does: "A disclosure menu: a click elsewhere or Escape closes it, focus back on its summary; the arrows, Home and End move through its items.",
        props: &[],
        events: &[],
        slots: &["summary", "menu"],
    },
    Component {
        tag: "sluice-copy",
        script: "components.js",
        does: "Copies an id or a SHA: its button, there only with script, says Copied for a moment.",
        props: &[("value", "string")],
        events: &["sluice-copied"],
        slots: &["text", "button"],
    },
    Component {
        tag: "sluice-toggle",
        script: "components.js",
        does: "A display setting applied at once and kept by posting it to /settings: value types (every Types switch follows), the theme or the appearance.",
        props: &[("setting", "string")],
        events: &["sluice-setting"],
        slots: &["control"],
    },
    Component {
        tag: "sluice-search",
        script: "components.js",
        does: "Finds as one types: the board's search and filters point the page's stream at the new query (the address follows), or a list hides what does not match; Escape clears. A form of filters (form[data-applies]) sends itself as a choice in it changes, its Apply hidden.",
        props: &[("mode", "oneOf"), ("base", "string")],
        events: &["sluice-stream-restart"],
        slots: &["form", "list"],
    },
    Component {
        tag: "sluice-banner",
        script: "components.js",
        does: "Says how the page's stream stands (paused, stopped with Reconnect) or that sluice was updated (Reload); hidden while live.",
        props: &[("kind", "oneOf")],
        events: &[],
        slots: &["words", "button"],
    },
    Component {
        tag: "sluice-splitter",
        script: "components.js",
        does: "Resizes the board beside the plan: drag, the arrows (16px, 64px with Shift), Home and End, double-click to reset; the width kept per project.",
        props: &[
            ("frame", "string"),
            ("store", "string"),
            ("min", "number"),
            ("rest", "number"),
        ],
        events: &["sluice-resized"],
        slots: &["grip"],
    },
    Component {
        tag: "sluice-keys",
        script: "components.js",
        does: "The page's keys, listed where it draws them (display preferences): / finds on the page (on its plan from a step or unit page), g then a letter goes to a section, ? shows the list. None while typing in a field, with a modifier held or under an open dialog.",
        props: &[],
        events: &[],
        slots: &["list"],
    },
    Component {
        tag: "sluice-grid",
        script: "components.js",
        does: "Shows the module grid's construction under its modules while its show-grid switch (aria-controls its id) is pressed: each column tinted and numbered, each module's span in its corner; the choice kept in this browser.",
        props: &[("showing", "boolean")],
        events: &["sluice-grid"],
        slots: &["overlay", "modules"],
    },
    Component {
        tag: "sluice-trace",
        script: "components.js",
        does: "Select to trace: a unit's button selects it, opens it in place, lights its chain up and down from the ids its markup carries, draws the rail in the margin, says the chain in its line and fades the rest; again, Clear trace or Escape clears; the arrows move between units.",
        props: &[("selected", "string")],
        events: &["sluice-trace"],
        slots: &["line", "units"],
    },
    Component {
        tag: "sluice-drawer",
        script: "sluice.js",
        does: "Opens a step beside the plan (over it below 1200px, a sheet on a phone) from #step:<id>, a stage's cell or any link with data-step, streams it, writes its tab into ?tab= and closes on Escape or a click away; [ and ] open the step before or after it in the plan's order.",
        props: &[("base", "string")],
        events: &[],
        slots: &["scrim", "drawer"],
    },
];

// ---- the renderers ---------------------------------------------------------------------------

/// A long text cut to its first lines, faded, with "Show all" and "Show less" under it.
pub fn fold_open(class: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}<div class=\"clip {}\">",
        Host::new("sluice-fold").attr("class", "long").open(),
        esc(class)
    ))
}
pub fn fold_close() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "</div><details class=\"fold-toggle more-fold\" data-preserve-attr=\"open\"><summary><span class=\"m-more\">Show all</span><span class=\"m-less\">Show less</span>{}</summary></details></sluice-fold>",
        icon(Icon::ChevronDown, 16, "chev")
    ))
}
/// A fold that opens to more (`details.<class>.more-fold`): its summary's two words ("More",
/// "Less"), or its one line when `less` is empty ("6 steps: 5 done, 1 running"); kept open in
/// this browser under `remember` (a per-project key), or opened on a wide screen with `wide`.
/// What it holds follows, then `more_close`.
pub fn more_open(class: &str, more: &str, less: &str, remember: &str, wide: bool) -> TrustedHtml {
    more_host(
        Host::new("sluice-fold")
            .some("remember", remember)
            .flag("wide", wide),
        class,
        more,
        less,
    )
}
/// The same, hidden until its page's script shows it (`data-<name>` names it there).
pub fn more_open_hidden(class: &str, more: &str, less: &str, name: &str) -> TrustedHtml {
    more_host(
        Host::new("sluice-fold")
            .attr(&format!("data-{name}"), "")
            .flag("hidden", true),
        class,
        more,
        less,
    )
}
fn more_host(host: Host, class: &str, more: &str, less: &str) -> TrustedHtml {
    let words = if less.is_empty() {
        format!("<span>{}</span>", esc(more))
    } else {
        format!(
            "<span class=\"m-more\">{}</span><span class=\"m-less\">{}</span>",
            esc(more),
            esc(less)
        )
    };
    TrustedHtml::owned(format!(
        "{}<details class=\"{} more-fold\" data-preserve-attr=\"open\"><summary>{words}{}</summary>",
        host.open(),
        esc(class),
        icon(Icon::ChevronDown, 16, "chev"),
    ))
}
/// A More's end: `less` a "Read less" there (the reader is at its end), folding it from where
/// the reader is (only with script).
pub fn more_close(less: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}</details></sluice-fold>",
        if less.is_empty() {
            String::new()
        } else {
            format!(
                "<button type=\"button\" class=\"fold-less link-quiet needs-js\">{}</button>",
                esc(less)
            )
        }
    ))
}

/// A key kept in this browser for one thing: "sluice.about.<project>".
pub fn keyed(prefix: &str, id: impl std::fmt::Display) -> String {
    format!("{prefix}.{id}")
}

/// An id or a SHA in data mono with a button that copies it (`label` names what it is for a
/// screen reader: "Copy step id").
pub fn copy(value: &str, label: &str) -> TrustedHtml {
    copy_with(value, label, "")
}
/// The same, its code carrying `class`.
pub fn copy_with(value: &str, label: &str, class: &str) -> TrustedHtml {
    let host = Host::new("sluice-copy").attr("value", value);
    TrustedHtml::owned(format!(
        "{}<code{}>{}</code><button type=\"button\" class=\"copy needs-js\" aria-label=\"{}\" title=\"{}\">{}{}</button>{}",
        host.open(),
        if class.is_empty() {
            String::new()
        } else {
            format!(" class=\"{}\"", esc(class))
        },
        esc(value),
        esc(label),
        esc(label),
        icon(Icon::Copy, 16, "cp-copy"),
        icon(Icon::Check, 16, "cp-done"),
        host.close()
    ))
}

/// The Types switch: value types shown or not, everywhere at once.
pub fn types_toggle() -> TrustedHtml {
    Host::new("sluice-toggle").attr("setting", "types").wrap(&TrustedHtml::owned(
        "<button type=\"button\" class=\"types-toggle\" aria-pressed=\"false\" data-preserve-attr=\"aria-pressed\">Types<span class=\"sw\" aria-hidden=\"true\"></span></button>".into(),
    ))
}
/// A display setting's control in the display preferences (`setting` "theme", "appearance"
/// or "types"):
/// the host before it, then `setting_close`.
pub fn setting_open(setting: &str) -> TrustedHtml {
    Host::new("sluice-toggle").attr("setting", setting).open()
}
pub fn setting_close() -> TrustedHtml {
    Host::new("sluice-toggle").close()
}

/// A menu's host (the project switcher, display preferences, the plan's More): its
/// `<details>` with its summary and `.menu` go between this and `menu_close`.
pub fn menu_open() -> TrustedHtml {
    Host::new("sluice-menu").open()
}
pub fn menu_close() -> TrustedHtml {
    Host::new("sluice-menu").close()
}

/// The page's stream line ("Updates paused. Reconnecting…", "Updates stopped at 14:02." with
/// Reconnect) and the build line ("sluice was updated · Reload"), each hidden until it applies.
pub fn banners() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}{}",
        banner(
            "stream",
            "stream-state",
            "Updates paused. Reconnecting…",
            true
        ),
        banner("release", "release-state", "", true)
    ))
}
/// One banner: `kind` "stream" with its words, or "release"; `id` empty for a copy that is not
/// the page's own (the gallery's).
pub fn banner(kind: &str, id: &str, words: &str, hidden: bool) -> TrustedHtml {
    let host = Host::new("sluice-banner")
        .attr("kind", kind)
        .some("id", id)
        .attr(
            "class",
            if kind == "stream" {
                "stream-state attn"
            } else {
                "stream-state release-state"
            },
        )
        .attr("role", "status")
        .attr("aria-live", "polite")
        .flag("hidden", hidden);
    host.wrap(&TrustedHtml::owned(if kind == "stream" {
        format!(
            "<span class=\"stream-words\">{}</span> <button type=\"button\" class=\"stream-retry\">Reconnect</button>",
            esc(words)
        )
    } else {
        "sluice was updated · <button type=\"button\" class=\"release-reload\">Reload</button>"
            .into()
    }))
}

/// The handle between the plan and the board beside it, which resizes the board.
pub fn splitter(project: &str) -> TrustedHtml {
    splitter_for(
        "project-board",
        "board-pane",
        &keyed("sluice.boardw", project),
        320,
        560,
    )
}
/// A splitter that sets `--board-w` on `frame` for the pane `pane` (at least `min` wide,
/// leaving `rest` beside it), its width kept under `store`.
pub fn splitter_for(frame: &str, pane: &str, store: &str, min: u32, rest: u32) -> TrustedHtml {
    Host::new("sluice-splitter")
        .attr("class", "splitter")
        .attr("frame", frame)
        .attr("store", store)
        .attr("min", min.to_string())
        .attr("rest", rest.to_string())
        .attr("role", "separator")
        .attr("aria-orientation", "vertical")
        .attr("aria-controls", pane)
        .attr("aria-label", "Board width")
        .attr("aria-valuemin", min.to_string())
        .attr("aria-valuemax", "1040")
        .attr("aria-valuenow", "400")
        .attr("tabindex", "0")
        .attr("title", "Drag to resize the board; double-click to reset")
        .keep("aria-valuemin")
        .keep("aria-valuemax")
        .keep("aria-valuenow")
        .keep("aria-valuetext")
        .wrap(&icon(Icon::GripVertical, 16, "grip"))
}

/// A search: `mode` "stream" around the board's tools form, pointing the page's stream at its
/// query under `base` (the project's address); `mode` "filter" around a list whose items carry
/// their words in `data-find`, its field (`search_field`) first.
pub fn search_open(mode: &str, base: &str) -> TrustedHtml {
    Host::new("sluice-search")
        .attr("mode", mode)
        .some("base", base)
        .open()
}
pub fn search_close() -> TrustedHtml {
    Host::new("sluice-search").close()
}
/// A filter's field, there only with script.
pub fn search_field(id: &str, placeholder: &str, label: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"q-field fn-find needs-js\">{}<input type=\"search\" id=\"{}\" placeholder=\"{}\" aria-label=\"{}\" autocomplete=\"off\" spellcheck=\"false\" data-preserve-attr=\"value\" data-find></div>",
        icon(Icon::Search, 16, "q-icon"),
        esc(id),
        esc(placeholder),
        esc(label)
    ))
}

/// The page's keys (`sluice-keys`) around their list.
pub fn keys_open() -> TrustedHtml {
    Host::new("sluice-keys").open()
}
pub fn keys_close() -> TrustedHtml {
    Host::new("sluice-keys").close()
}

/// The drawer's tag: a project page's board region ends where it starts.
pub const DRAWER: &str = "sluice-drawer";
/// The step drawer, empty until a step opens in it (`base` the project's address). Its
/// stream's binding sits after it, outside the host: a component re-applies Datastar to what
/// it holds after every patch inside it, which would open the step's stream again each time.
pub fn drawer(base: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}<div id=\"drawer-stream\" hidden></div>",
        Host::new("sluice-drawer")
            .attr("id", "step-drawer")
            .attr("base", base)
            .wrap(&TrustedHtml::owned(format!(
                "<div class=\"scrim\" hidden></div><aside id=\"drawer\" class=\"drawer\" hidden tabindex=\"-1\" aria-labelledby=\"d-title\"><div class=\"d-top\"><button type=\"button\" class=\"close\" aria-label=\"Close\">{}</button></div><div id=\"step-detail\"></div></aside>",
                icon(Icon::X, 20, "")
            )))
    ))
}

/// A note's Mark read: the watermark of what the page drew of its thread (only with script).
pub fn mark_read(
    url: &str,
    thread: &str,
    through: impl std::fmt::Display,
    words: &str,
    label: &str,
) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<button type=\"button\" class=\"mark-read needs-js\" data-read-url=\"{}\" data-thread=\"{}\" data-through=\"{through}\"{}>{}</button>",
        esc(url),
        esc(thread),
        if label.is_empty() {
            String::new()
        } else {
            format!(" aria-label=\"{}\"", esc(label))
        },
        esc(words)
    ))
}
/// A conversation's host: what the page draws before it (a note's head, a thread's line) goes
/// inside, so its Mark read is the conversation's. `start` "end": a page of its own opens at
/// its newest message.
pub fn conversation_open(id: &str, start: &str) -> TrustedHtml {
    Host::new("sluice-conversation")
        .some("id", id)
        .some("start", start)
        .open()
}
pub fn conversation_close() -> TrustedHtml {
    Host::new("sluice-conversation").close()
}
/// "Mark all read" over a page's notes, and the line that says when a mark did not go through.
pub fn mark_all() -> TrustedHtml {
    TrustedHtml::owned(
        "<button type=\"button\" class=\"mark-all needs-js\" data-mark-all>Mark all read</button>"
            .into(),
    )
}

/// A question's answer area: its Answer and Close, and the box Answer opens.
pub fn answer_open() -> TrustedHtml {
    Host::new("sluice-answer").open()
}
pub fn answer_close() -> TrustedHtml {
    Host::new("sluice-answer").close()
}

/// The message box at a conversation's end.
pub fn composer_open() -> TrustedHtml {
    Host::new("sluice-composer").open()
}
pub fn composer_close() -> TrustedHtml {
    Host::new("sluice-composer").close()
}

impl Confirm {
    /// The confirmation as a page draws it: the server's `<details>` (its summary the opener,
    /// its form what is confirmed) in a `sluice-confirm`, whose summary opens the shared
    /// dialog with the form in it; without script the details open the same form inline.
    pub fn html(&self) -> TrustedHtml {
        let hidden: String = self
            .hidden
            .iter()
            .map(|(n, v)| {
                format!(
                    "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
                    esc(n),
                    esc(v)
                )
            })
            .collect();
        // the box is the owner's while open: a stream patch never empties it (`data-ignore-morph`)
        let reason = self.reason.map_or(String::new(), |placeholder| format!(
            "<label class=\"confirm-reason\">{}<textarea name=\"message\" rows=\"3\" maxlength=\"16384\" placeholder=\"{}\" data-ignore-morph>{}</textarea></label>",
            esc(if self.reason_label.is_empty() { "Reason (optional)" } else { self.reason_label }),
            esc(placeholder),
            esc(&self.reason_value)
        ));
        let host = Host::new("sluice-confirm")
            .attr("heading", &self.title)
            .some("ref-id", &self.id)
            .flag("disabled", self.disabled);
        let id = |id: &str| {
            if id.is_empty() {
                String::new()
            } else {
                format!(" id=\"{}\"", esc(id))
            }
        };
        TrustedHtml::owned(format!(
            "{open}<details class=\"confirm-flow\"><summary{opener_id}{disabled}>{opener}</summary><form method=\"post\" action=\"{action}\"{form_id}>{hidden}<p class=\"confirm-copy\">{lead}{copy}</p>{reason}<div class=\"confirm-actions\"><button class=\"{tone}\">{confirm}</button><button type=\"button\" data-keep>{keep}</button></div>{after}</form></details>{close}",
            open = host.open(),
            close = host.close(),
            opener_id = id(&self.opener_id),
            disabled = if self.disabled {
                " aria-disabled=\"true\""
            } else {
                ""
            },
            opener = esc(&self.opener),
            action = esc(&self.action),
            form_id = id(&self.form_id),
            lead = self.lead,
            copy = esc(&self.copy),
            tone = if self.danger { "danger" } else { "primary" },
            confirm = esc(&self.confirm),
            keep = esc(self.keep),
            after = self.after,
        ))
    }
}
