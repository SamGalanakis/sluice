//! Avatars (DESIGN.md, Avatars): a printed mark for each actor, seeded by its id so it is the
//! same on every page. An agent is a rounded square: a worker (a step's run) a moiré of two
//! ring screens in agent ink over a faint circle or square, the orchestrator (a named session,
//! `@lead`) the same over a diamond, wearing a ring. The owner is a person: a circle, a
//! line-screen bust in the pen's ink. Sluice itself is its mark. Sizes: `Sm` 16px (a row's
//! said line), `Md` 20px (a message), `Lg` 26px (the sidebar's foot).
use super::esc;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvSize {
    Sm,
    Md,
    Lg,
}
impl AvSize {
    fn class(self) -> &'static str {
        match self {
            AvSize::Sm => " sm",
            AvSize::Md => "",
            AvSize::Lg => " lg",
        }
    }
}

/// Who an id is, as an avatar draws it: the owner, Sluice, a named session (the orchestrator)
/// or a worker (anything else: a step, a unit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    Owner,
    Sluice,
    Orchestrator,
    Worker,
}
impl Actor {
    pub fn of(id: &str) -> Actor {
        match id {
            "owner" | "you" => Actor::Owner,
            "sluice" => Actor::Sluice,
            _ if id.starts_with('@') || id == sluice_model::identity::LEGACY_ORCHESTRATOR => {
                Actor::Orchestrator
            }
            _ => Actor::Worker,
        }
    }
}

/// FNV-1a, as the mock seeds its marks.
fn hash(text: &str) -> u32 {
    let mut h: u32 = 2_166_136_261;
    for unit in text.encode_utf16() {
        h ^= u32::from(unit);
        h = h.wrapping_mul(16_777_619);
    }
    h
}
/// A linear congruential sequence in [0, 1).
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(self.0) / 4_294_967_296.0
    }
}

/// `id`'s avatar at `size`, named for a screen reader unless `hidden` (its name is beside it).
pub fn avatar(id: &str, size: AvSize, hidden: bool) -> String {
    let kind = Actor::of(id);
    let label = if hidden {
        " aria-hidden=\"true\"".to_owned()
    } else {
        format!(
            " role=\"img\" aria-label=\"{}\"",
            esc(&match kind {
                Actor::Owner => "You".to_owned(),
                Actor::Sluice => "Sluice".to_owned(),
                Actor::Orchestrator => format!("{id}, a session"),
                Actor::Worker => format!("{id}, a worker"),
            })
        )
    };
    let size = size.class();
    match kind {
        Actor::Owner => format!(
            "<span class=\"av person{size}\"{label}><svg viewBox=\"0 0 20 20\"><use href=\"#av-person\"/></svg></span>"
        ),
        Actor::Sluice => format!("<span class=\"av sys{size}\"{label}></span>"),
        Actor::Orchestrator | Actor::Worker => {
            let mut r = Rng(hash(id));
            let ax = 6.0 + r.next() * 8.0;
            let ay = 6.0 + r.next() * 8.0;
            let bx = 20.0 - ax + (r.next() - 0.5) * 3.0;
            let by = 20.0 - ay + (r.next() - 0.5) * 3.0;
            let figure = if kind == Actor::Orchestrator {
                "diamond"
            } else if r.next() < 0.5 {
                "circle"
            } else {
                "square"
            };
            let pct = |v: f64| format!("{:.1}%", v * 5.0);
            format!(
                "<span class=\"av agent {figure}{}{size}\"{label} style=\"--ax:{};--ay:{};--bx:{};--by:{}\"></span>",
                if kind == Actor::Orchestrator {
                    " orch"
                } else {
                    ""
                },
                pct(ax),
                pct(ay),
                pct(bx),
                pct(by),
            )
        }
    }
}

/// The owner's bust, the sprite's `#av-person` (`icons::sprite`): a line screen of 16 rows,
/// each a band as thick as the bust is dark there, lit from the right. Drawn once.
pub fn person_symbol() -> &'static str {
    static SYMBOL: LazyLock<String> = LazyLock::new(|| {
        let mut r = Rng(hash("owner"));
        let tilt = (r.next() - 0.5) * 1.2;
        let dark = |x: f64, y: f64| -> f64 {
            let hx = (x - 10.0 - tilt) / 4.4;
            let hy = (y - 8.2) / 5.0;
            let head = hx * hx + hy * hy;
            let sx = (x - 10.0) / 9.0;
            let sy = (y - 22.0) / 7.5;
            let shoulders = sx * sx + sy * sy;
            let mut d = if head < 1.0 {
                0.95 - head * 0.35
            } else if shoulders < 1.0 {
                0.9 - shoulders * 0.25
            } else {
                0.06
            };
            if head < 1.0 && y < 5.5 {
                d = 1.0;
            }
            if head < 1.0 && (y - 8.0).abs() < 0.8 && ((x - 10.0 - tilt).abs() - 1.8).abs() < 0.9 {
                d = 0.25;
            }
            d * (0.7 + 0.3 * (1.0 - x / 20.0))
        };
        let mut paths = String::new();
        let mut y = 1.0;
        while y < 20.0 {
            let xs: Vec<f64> = (0..=40).map(|i| f64::from(i) * 0.5).collect();
            let half: Vec<f64> = xs.iter().map(|&x| dark(x, y) * 1.15 / 2.0).collect();
            let mut d = format!("M{:.1} {:.2}", xs[0], y - half[0]);
            for (x, h) in xs.iter().zip(&half).skip(1) {
                d.push_str(&format!("L{x:.1} {:.2}", y - h));
            }
            for (x, h) in xs.iter().zip(&half).rev() {
                d.push_str(&format!("L{x:.1} {:.2}", y + h));
            }
            d.push('Z');
            paths.push_str(&format!("<path d=\"{d}\"/>"));
            y += 1.25;
        }
        format!(
            "<symbol id=\"av-person\" viewBox=\"0 0 20 20\"><g fill=\"currentColor\">{paths}</g></symbol>"
        )
    });
    &SYMBOL
}
