//! The dashboard's icons: Lucide (lucide-static 1.52.0, ISC, `assets/icons/LICENSE`). Each is
//! the published SVG, embedded unmodified, and inlined in the page so it takes `currentColor`.
use super::TrustedHtml;
use std::sync::LazyLock;

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        /// A Lucide icon, named as Lucide names it.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon { $($variant),* }
        impl Icon {
            const ALL: &[Icon] = &[$(Icon::$variant),*];
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
    SquareArrowOutUpRight => "square-arrow-out-up-right",
    TriangleAlert => "triangle-alert",
    LayoutDashboard => "layout-dashboard",
    Workflow => "workflow",
    Columns2 => "columns-2",
    GripVertical => "grip-vertical",
    Search => "search",
    X => "x",
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

/// The icon as an inline `<svg>`, `size` px square, on Lucide's own 24-unit grid and stroke,
/// drawn in `currentColor` and hidden from assistive tech: the control it sits in carries the
/// name. `class` is added after `icon`.
pub fn icon(icon: Icon, size: u16, class: &str) -> TrustedHtml {
    let sep = if class.is_empty() { "" } else { " " };
    TrustedHtml::owned(format!(
        "<svg class=\"icon{sep}{class}\" width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\">{}</svg>",
        BODIES[icon as usize]
    ))
}
