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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_lucide_on_the_grid_and_stroke_it_is_drawn_with() {
        for &each in Icon::ALL {
            let source = each.source();
            assert!(
                source.starts_with("<!-- @license lucide-static v1.52.0 - ISC -->"),
                "{each:?}"
            );
            let tag = &source[source.find("<svg").unwrap()..];
            let tag = &tag[..tag.find('>').unwrap()];
            for attr in [
                "viewBox=\"0 0 24 24\"",
                "fill=\"none\"",
                "stroke=\"currentColor\"",
                "stroke-width=\"2\"",
                "stroke-linecap=\"round\"",
                "stroke-linejoin=\"round\"",
            ] {
                assert!(tag.contains(attr), "{each:?} lacks {attr}");
            }
            let html = icon(each, 20, "x").to_string();
            assert!(html.starts_with("<svg class=\"icon x\" width=\"20\" height=\"20\""));
            assert!(html.ends_with("/></svg>") && !html.contains('\n'), "{html}");
        }
    }
}
