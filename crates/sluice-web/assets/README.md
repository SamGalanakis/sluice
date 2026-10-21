Dashboard assets, compiled into the binary (each page module under `src/views/` embeds its
own) and served at `/static/<name>?v=<fingerprint>`: `style.css` (the one stylesheet: Paper and Carbon
are its `light-dark()` tokens), sluice's
scripts (`components.js`, `sluice.js`, `nav.js`, `openui.js`, `board.js`), the logo and favicon
(with its two glance variants a plan's tab wears: `favicon-ask.svg`, the mark with a teal dot
while a question waits on the owner, and `favicon-look.svg`, with an ochre square while
something needs a look; each the owner's favicon unchanged with the one shape after it), and the pinned
third-party scripts (Datastar 1.0.4, OpenUI lang-core 0.3.0, zod 4.6.5). OpenUI and zod are
the ES-module bundles jsDelivr's `/npm/<package>@<version>/+esm` endpoint built; each keeps its
header naming the bundler and the original file, less the CDN's own note.

`fonts/` holds the two faces the stylesheet's `@font-face` rules use, Inter and JetBrains
Mono, served at `/static/fonts/<file>?v=<fingerprint>` with `Content-Type: font/woff2` and
cached for good. Each family's
directory holds its woff2 files, exactly as fontsource 5.3.0 publishes them
(`https://cdn.jsdelivr.net/npm/@fontsource/<family>@5.3.0/files/<file>`, or
`@fontsource-variable/` for a `-wght-` file), and the package's `LICENSE` (the SIL Open Font
License 1.1 for both). `fonts/SHA256SUMS` records each file's sha256
(`sha256sum -c SHA256SUMS` in `fonts/` checks them, as `tests/fonts.rs` does); `build.rs`
reads it to list the faces and to version each `url(/static/fonts/<file>)` in the sheet with
the file's fingerprint. A new face is a file here, its line in the manifest and its rule.
`scripts/build-release` installs each family's licence and the manifest in the release as
`licenses/fonts/<family>/LICENSE` and `licenses/fonts/SHA256SUMS`.

`icons/` holds the dashboard's icons: Lucide (lucide-static 1.52.0), each SVG as published,
embedded by `src/views/icons.rs` and inlined in pages, not served. `icons/LICENSE` is Lucide's
licence (ISC, with the MIT notice for its Feather-derived icons); the release ships it with the
assets.

`sketches/` holds the empty states' pen sketches (cenote's: the empty sheet, the ruled lines,
the bubble, the lens, the spiral), each an SVG of strokes that `src/views/ui/sketch.rs` inlines
and the stylesheet draws on along the pen's clock.

The kit (its Rocket components in `components.js` and the kit section of `style.css`,
drawn by `src/views/ui.rs` and `src/views/ui/rocket.rs`,
`templates/kit.html` and `templates/conversation.html`; shown whole at `/_ui`) adapts ideas and
CSS from these MIT-licensed projects, with no code of theirs at run time: knadh/oat (semantic
HTML and CSS-variable styling for tabs, cards, badges and the dialog), hunvreus/basecoat
(shadcn's visual polish as plain classes: tabs, empty state, item rows) and
github/tab-container-element (the ARIA tabs keyboard model: arrows, Home and End, a panel as a
tab stop).
