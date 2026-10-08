Dashboard assets, compiled into the binary (each page module under `src/views/` embeds its
own) and served at `/static/<name>?v=<fingerprint>`: `style.css` and `settings.css`, sluice's
scripts (`sluice.js`, `nav.js`, `kit.js`, `inbox.js`, `openui.js`, `board.js`), the logo and favicon, and the pinned
third-party scripts (Datastar 1.0.4, OpenUI lang-core 0.3.0, zod 4.6.5).

`icons/` holds the dashboard's icons: Lucide (lucide-static 1.52.0), each SVG as published,
embedded by `src/views/icons.rs` and inlined in pages, not served. `icons/LICENSE` is Lucide's
licence (ISC, with the MIT notice for its Feather-derived icons); the release ships it with the
assets.

The kit (`kit.js` and the kit section of `style.css`, drawn by `src/views/ui.rs`,
`templates/kit.html` and `templates/conversation.html`; shown whole at `/_ui`) adapts ideas and
CSS from these MIT-licensed projects, with no code of theirs at run time: knadh/oat (semantic
HTML and CSS-variable styling for tabs, cards, badges and the dialog), hunvreus/basecoat
(shadcn's visual polish as plain classes: tabs, empty state, item rows) and
github/tab-container-element (the ARIA tabs keyboard model: arrows, Home and End, a panel as a
tab stop).
