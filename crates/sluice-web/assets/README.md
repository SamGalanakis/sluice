Dashboard assets, compiled into the binary (each page module under `src/views/` embeds its
own) and served at `/static/<name>?v=<fingerprint>`: `style.css` and `settings.css`, sluice's
scripts (`sluice.js`, `nav.js`, `inbox.js`, `openui.js`, `board.js`), the logo and favicon, and the pinned
third-party scripts (Datastar 1.0.4, OpenUI lang-core 0.3.0, zod 4.6.5).

`icons/` holds the dashboard's icons: Lucide (lucide-static 1.52.0), each SVG as published,
embedded by `src/views/icons.rs` and inlined in pages, not served. `icons/LICENSE` is Lucide's
licence (ISC, with the MIT notice for its Feather-derived icons); the release ships it with the
assets.
