//! The dashboard's icons: Lucide (lucide-static 1.52.0, ISC, `assets/icons/LICENSE`). Each is
//! the published SVG, embedded unmodified, as a `<symbol>` in one sprite every page carries
//! (`sprite`); each use of it is a small `<svg><use href="#i-…"></svg>` that takes
//! `currentColor`, so a page with a thousand glyphs does not repeat their shapes.
use super::TrustedHtml;
use std::sync::LazyLock;

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        /// A Lucide icon, named as Lucide names it.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon { $($variant),* }
        impl Icon {
            const ALL: &[Icon] = &[$(Icon::$variant),*];
            fn name(self) -> &'static str {
                match self {
                    $(Icon::$variant => $file,)*
                }
            }
            fn source(self) -> &'static str {
                match self {
                    $(Icon::$variant => include_str!(concat!("../../assets/icons/", $file, ".svg")),)*
                }
            }
        }
    };
}
icons! {
    Settings => "settings",
    SlidersHorizontal => "sliders-horizontal",
    Inbox => "inbox",
    ChevronDown => "chevron-down",
    Check => "check",
    CircleDashed => "circle-dashed",
    LoaderCircle => "loader-circle",
    CircleCheck => "circle-check",
    CircleDot => "circle-dot",
    RotateCw => "rotate-cw",
    CircleX => "circle-x",
    CirclePause => "circle-pause",
    CircleSlash => "circle-slash",
    CircleStop => "circle-stop",
    SquareArrowOutUpRight => "square-arrow-out-up-right",
    TriangleAlert => "triangle-alert",
    LayoutDashboard => "layout-dashboard",
    Workflow => "workflow",
    Columns2 => "columns-2",
    GripVertical => "grip-vertical",
    Search => "search",
    X => "x",
    ArrowLeft => "arrow-left",
    FileText => "file-text",
}

/// Each icon's shapes (what sits inside its `<svg>`), on one line, in `Icon` order.
static BODIES: LazyLock<Vec<String>> =
    LazyLock::new(|| Icon::ALL.iter().map(|icon| body(icon.source())).collect());

fn body(source: &str) -> String {
    let open = source.find("<svg").expect("a Lucide icon is an svg");
    let start = open + source[open..].find('>').expect("its svg tag closes") + 1;
    let end = source.rfind("</svg>").expect("its svg ends");
    source[start..end].lines().map(str::trim).collect()
}

/// The icons whose glyph is drawn solid (`glyph`'s succeeded and failed): the circle filled in
/// `currentColor`, the mark cut out of it in the card's colour. A rule cannot reach into a
/// `<use>`, so the sprite carries these as symbols of their own.
const SOLID: [Icon; 2] = [Icon::CircleCheck, Icon::CircleX];

/// Every icon's shapes as a `<symbol>`, once per page (the layout puts it first in the body).
pub fn sprite() -> TrustedHtml {
    static SPRITE: LazyLock<String> = LazyLock::new(|| {
        let mut out = String::from(
            "<svg class=\"sprite\" width=\"0\" height=\"0\" aria-hidden=\"true\" focusable=\"false\" style=\"position:absolute\">",
        );
        for icon in Icon::ALL {
            let body = &BODIES[*icon as usize];
            out.push_str(&format!(
                "<symbol id=\"i-{}\" viewBox=\"0 0 24 24\">{body}</symbol>",
                icon.name()
            ));
            if SOLID.contains(icon) {
                let solid = body
                    .replace("<circle ", "<circle style=\"fill:currentColor\" ")
                    .replace("<path ", "<path style=\"stroke:var(--card)\" ");
                out.push_str(&format!(
                    "<symbol id=\"i-{}-solid\" viewBox=\"0 0 24 24\">{solid}</symbol>",
                    icon.name()
                ));
            }
        }
        out.push_str("</svg>");
        out
    });
    TrustedHtml::owned(SPRITE.clone())
}

fn draw(name: &str, size: u16, class: &str) -> TrustedHtml {
    let sep = if class.is_empty() { "" } else { " " };
    TrustedHtml::owned(format!(
        "<svg class=\"icon{sep}{class}\" width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><use href=\"#i-{name}\"/></svg>"
    ))
}
/// The icon, `size` px square, on Lucide's own 24-unit grid and stroke, drawn in
/// `currentColor` and hidden from assistive tech: the control it sits in carries the name.
/// `class` is added after `icon`.
pub fn icon(icon: Icon, size: u16, class: &str) -> TrustedHtml {
    draw(icon.name(), size, class)
}
/// The icon drawn solid (see `SOLID`); any other icon as it is.
pub fn solid(icon: Icon, size: u16, class: &str) -> TrustedHtml {
    if SOLID.contains(&icon) {
        draw(&format!("{}-solid", icon.name()), size, class)
    } else {
        draw(icon.name(), size, class)
    }
}
