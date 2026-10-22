//! The dashboard's components (DESIGN.md, Components): Rust draws each one's host element and
//! everything in it, and a Rocket component of the same name (`assets/components.js`, or
//! `assets/sluice.js` for the board and the drawer) adds its behaviour in light DOM, through a
//! `setup` that enhances what the server drew and never draws content of its own. A page reads
//! whole before its script runs and without it, a stream patch morphs server HTML into server
//! HTML (each host's state is an attribute kept through the patch, `data-preserve-attr`), and a
//! screen reader hears what the server wrote. A page makes a host only through these functions.
use super::esc;
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
    /// An attribute the script owns once the page is drawn: a patch never resets it (as it never
    /// resets Rocket's own `data-rocket-host`).
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
        // Rocket marks a host it has set up `data-rocket-host` and finds an element's host (for
        // its actions, `@post` among them) by that mark: a patch must never take it away
        // (https://data-star.dev/reference/rocket)
        out.push_str(" data-preserve-attr=\"data-rocket-host");
        for name in &self.keep {
            out.push(' ');
            out.push_str(name);
        }
        out.push_str("\">");
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
        does: "An ARIA tablist over its panels: arrows wrap, Home and End; the chosen tab in $$tab and the page's tab signal; an anchor in a panel opens its tab.",
        props: &[("current", "string")],
        events: &["sluice-tab"],
        slots: &["tabs", "panels"],
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
        tag: "sluice-conversation",
        script: "components.js",
        does: "Jump to latest, and a thread's page opening at its end.",
        props: &[("start", "string")],
        events: &[],
        slots: &["lead", "list"],
    },
    Component {
        tag: "sluice-answer",
        script: "components.js",
        does: "A question's Answer opens its box (the question's own form, drawn by openui.js from its program).",
        props: &[],
        events: &[],
        slots: &["actions", "box"],
    },
    Component {
        tag: "sluice-menu",
        script: "components.js",
        does: "A disclosure menu (the project switcher, display preferences, an item's Details behind its ⋯): its summary says aria-expanded, a click elsewhere or Escape closes it, focus back on its summary; the arrows, Home and End move through its items. A sub-menu (display preferences' Theme and Keyboard shortcuts) opens in the same menu, one level at a time: Enter or ArrowRight opens its row, its back control, ArrowLeft or Backspace returns to the row.",
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
        tag: "sluice-search",
        script: "components.js",
        does: "A list found as one types: what does not match hides, a group with none left too, and a status says how many match; Escape clears.",
        props: &[],
        events: &[],
        slots: &["list"],
    },
    Component {
        tag: "sluice-banner",
        script: "components.js",
        does: "Says how the page's stream stands (paused, stopped with Reconnect, the stream's own @get) or that sluice was updated (Reload); hidden while live.",
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
        tag: "sluice-steps",
        script: "components.js",
        does: "A unit's step view: a click on a row opens its step (in the drawer on the plan), the arrows, Home and End move between its rows, and the row in focus, clicked or open in the drawer is the one selected.",
        props: &[("selected", "string")],
        events: &[],
        slots: &["rows"],
    },
    Component {
        tag: "sluice-pulse",
        script: "components.js",
        does: "A pulse strip (At work's rows, a running step's in a drilldown): pointing at a mark (its minute the target, the strip's full height), tapping it or moving the arrows to it shows what its agent did that minute in the page's popover, asked of the server (sluice-pulse, the roster's debounced @get); its left edge fades while its run began before the window and says when it began. Escape or a tap elsewhere closes it.",
        props: &[("unit", "string"), ("step", "string"), ("from", "number")],
        events: &["sluice-pulse"],
        slots: &["marks"],
    },
    Component {
        tag: "sluice-drawer",
        script: "sluice.js",
        does: "Opens a step beside the plan (over it below 1200px, a sheet on a phone) from #step:<id>, a stage's cell or any link with data-step, streams it and closes on Escape or a click away; [ and ] open the step before or after it in the plan's order.",
        props: &[("base", "string")],
        events: &[],
        slots: &["scrim", "drawer"],
    },
    Component {
        tag: "sluice-day",
        script: "components.js",
        does: "The day line (/day): its rule at now and each running run's bar to it, moved on with each tick of the page's clock.",
        props: &[],
        events: &[],
        slots: &["ticks", "rows"],
    },
    Component {
        tag: "sluice-board",
        script: "board.js",
        does: "A project's page (#project-board): the Plan · Both · Board switch shows its view at once and keeps it per project, the owner's Plan folds survive patches and moves using project-scoped session memory, the board's column fits the window with its More below cue, a step the live plan moves on is said once in #announce, and a form with data-focus-after hands the focus on once its write is drawn.",
        props: &[],
        events: &[],
        slots: &["plan", "board"],
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
/// the reader is (`sluice-fold`).
pub fn more_close(less: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}</details></sluice-fold>",
        if less.is_empty() {
            String::new()
        } else {
            format!(
                "<button type=\"button\" class=\"fold-less link-quiet\">{}</button>",
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
        "{}<code{}>{}</code><button type=\"button\" class=\"copy\" aria-label=\"{}\" title=\"{}\">{}{}</button>{}",
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

/// The Types switch: value types shown or not, everywhere at once (the page's `types` signal,
/// kept by `/settings`).
pub fn types_toggle() -> TrustedHtml {
    TrustedHtml::owned(
        "<button type=\"button\" class=\"types-toggle\" data-attr:aria-pressed=\"String($types)\" data-on:click=\"$types = !$types; @post('/settings')\">Types<span class=\"sw\" aria-hidden=\"true\"></span></button>".into(),
    )
}

/// A menu's host (the project switcher, display preferences, an item's Details): its
/// `<details>` with its summary and `.menu` go between this and `menu_close`.
pub fn menu_open() -> TrustedHtml {
    Host::new("sluice-menu").open()
}
pub fn menu_close() -> TrustedHtml {
    Host::new("sluice-menu").close()
}

/// The page's stream line ("Updates paused. Reconnecting…", "Updates stopped at 14:02." with
/// Reconnect, which opens `stream` again) and the build line ("sluice was updated · Reload"),
/// each hidden until it applies.
pub fn banners(stream: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}{}",
        banner(
            "stream",
            "stream-state",
            "Updates paused. Reconnecting…",
            true,
            stream
        ),
        banner("release", "release-state", "", true, "")
    ))
}
/// One banner: `kind` "stream" with its words, or "release"; `id` empty for a copy that is not
/// the page's own (the gallery's), `stream` the page's stream's address (none for a copy).
pub fn banner(kind: &str, id: &str, words: &str, hidden: bool, stream: &str) -> TrustedHtml {
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
            "<span class=\"stream-words\">{}</span> <button type=\"button\" class=\"stream-retry\"{}>Reconnect</button>",
            esc(words),
            if stream.is_empty() {
                String::new()
            } else {
                format!(
                    " data-reopens-stream data-on:click=\"{}\"",
                    esc(&super::stream_reopen(stream))
                )
            }
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

/// A list found as one types: its items carry their words in `data-find`, its field
/// (`search_field`) first.
pub fn search_open() -> TrustedHtml {
    Host::new("sluice-search").open()
}
pub fn search_close() -> TrustedHtml {
    Host::new("sluice-search").close()
}
/// A filter's field.
pub fn search_field(id: &str, placeholder: &str, label: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"q-field fn-find\">{}<input type=\"search\" id=\"{}\" placeholder=\"{}\" aria-label=\"{}\" autocomplete=\"off\" spellcheck=\"false\" data-preserve-attr=\"value\" data-find></div>",
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
                // closing is the address without its step: the close a plain link to "#", the
                // scrim and Escape the same move (`sluice-drawer` follows the hash)
                "<div class=\"scrim\" hidden data-on:click=\"location.hash = ''\"></div><aside id=\"drawer\" class=\"drawer\" hidden tabindex=\"-1\" aria-labelledby=\"d-title\" data-on:keydown__window=\"evt.key === 'Escape' &amp;&amp; location.hash.startsWith('#step:') &amp;&amp; !document.querySelector('dialog[open]') &amp;&amp; (location.hash = '')\"><div class=\"d-top\"><a class=\"btn icon big close\" href=\"#\" aria-label=\"Close\" title=\"Close\">{}</a></div><div id=\"step-detail\"></div></aside>",
                icon(Icon::X, 20, "")
            )))
    ))
}

/// A note's Mark read: a form posting the watermark of what the page drew of its thread.
pub fn mark_read(
    url: &str,
    thread: &str,
    through: impl std::fmt::Display,
    words: &str,
    label: &str,
) -> TrustedHtml {
    let id = super::form_id(&format!("read {url} {thread}"));
    TrustedHtml::owned(format!(
        "{open}<input type=\"hidden\" name=\"thread\" value=\"{}\"><input type=\"hidden\" name=\"through\" value=\"{through}\"><button{}>{}</button>{said}</form>",
        esc(thread),
        if label.is_empty() {
            String::new()
        } else {
            format!(" aria-label=\"{}\"", esc(label))
        },
        esc(words),
        open = super::post_form(&id, "mark-read", url),
        said = super::form_said(&id),
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
/// A question's answer area: its Answer and Close, and the box Answer opens.
pub fn answer_open() -> TrustedHtml {
    Host::new("sluice-answer").open()
}
pub fn answer_close() -> TrustedHtml {
    Host::new("sluice-answer").close()
}
