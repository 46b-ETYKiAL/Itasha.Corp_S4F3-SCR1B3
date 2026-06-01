//! Theme model. Helix-style TOML with three orthogonal namespaces:
//! `[palette]` (named base colors), `[ui]` (chrome), `[syntax]` (token scopes).
//! UI values may reference a palette name or be an inline `#RRGGBB`/`#RRGGBBAA`.
//!
//! UI-toolkit-agnostic: colors are RGBA tuples the render/app layer maps onto
//! egui `Color32`. A fallback theme is compiled in so a broken user theme
//! never blanks the editor.

use crate::error::{CoreError, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parse `#RGB`, `#RRGGBB`, or `#RRGGBBAA`.
    pub fn parse_hex(s: &str) -> Option<Rgba> {
        let h = s.strip_prefix('#')?;
        let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
        match h.len() {
            3 => {
                let r = u8::from_str_radix(&h[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&h[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&h[2..3], 16).ok()? * 17;
                Some(Rgba::new(r, g, b, 255))
            }
            6 => Some(Rgba::new(v(0, 2)?, v(2, 2)?, v(4, 2)?, 255)),
            8 => Some(Rgba::new(v(0, 2)?, v(2, 2)?, v(4, 2)?, v(6, 2)?)),
            _ => None,
        }
    }

    pub fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub appearance: Appearance,
    pub palette: BTreeMap<String, Rgba>,
    pub ui: BTreeMap<String, Rgba>,
    pub syntax: BTreeMap<String, Rgba>,
}

impl Theme {
    /// Look up a UI color, falling back to a default if absent.
    pub fn ui(&self, key: &str, default: Rgba) -> Rgba {
        self.ui.get(key).copied().unwrap_or(default)
    }

    /// Resolve a syntax-scope color with longest-matching-scope-wins fallback
    /// (`function.builtin.static` -> `function.builtin` -> `function`).
    pub fn syntax_color(&self, scope: &str, default: Rgba) -> Rgba {
        let mut probe = scope;
        loop {
            if let Some(c) = self.syntax.get(probe) {
                return *c;
            }
            match probe.rfind('.') {
                Some(idx) => probe = &probe[..idx],
                None => return default,
            }
        }
    }

    /// The compiled-in fallback / default brand theme: `wired-noir`.
    /// Lore-council-approved (canon-decision-log DECISION-2026-005): a calm,
    /// legible surface over an implied machine substrate — cool near-black
    /// layers, off-white text, ONE teal accent (the system's voice),
    /// Akira-red reserved for alarms only, restrained amber for warnings,
    /// and a cool low-chroma syntax map. Supersedes the prior neon
    /// itasha-void hexes.
    pub fn wired_noir() -> Theme {
        let mut palette = BTreeMap::new();
        // Cool near-black background layers (#070A0C deepest → #1A242B raised).
        palette.insert("void".into(), Rgba::new(0x07, 0x0a, 0x0c, 255));
        palette.insert("panel".into(), Rgba::new(0x0e, 0x14, 0x17, 255));
        palette.insert("bezel".into(), Rgba::new(0x1a, 0x24, 0x2b, 255));
        palette.insert("text".into(), Rgba::new(0xc8, 0xd6, 0xdc, 255));
        palette.insert("muted".into(), Rgba::new(0x5a, 0x6b, 0x73, 255));
        palette.insert("dim".into(), Rgba::new(0x4f, 0x5e, 0x66, 255));
        palette.insert("teal".into(), Rgba::new(0x34, 0xe0, 0xd0, 255)); // the system voice
        palette.insert("red".into(), Rgba::new(0xff, 0x3b, 0x30, 255)); // alarms only
        palette.insert("amber".into(), Rgba::new(0xf2, 0xb3, 0x3d, 255)); // warnings
        palette.insert("green".into(), Rgba::new(0x6f, 0xb8, 0x9a, 255)); // muted ok
        palette.insert("slate".into(), Rgba::new(0x79, 0xa0, 0xb0, 255)); // keyword
        palette.insert("sage".into(), Rgba::new(0x8d, 0xa8, 0x8c, 255)); // string
        palette.insert("steel".into(), Rgba::new(0xa9, 0xc2, 0xcc, 255)); // type
        palette.insert("sand".into(), Rgba::new(0xc9, 0xa8, 0x6a, 255)); // constant/number

        let p = |k: &str| *palette.get(k).unwrap();
        let mut ui = BTreeMap::new();
        ui.insert("background".into(), p("void"));
        ui.insert("foreground".into(), p("text"));
        ui.insert("panel".into(), p("panel"));
        ui.insert("bezel".into(), p("bezel"));
        ui.insert("gutter".into(), p("void"));
        ui.insert("line_number".into(), p("muted"));
        ui.insert("line_number_active".into(), p("teal"));
        ui.insert("cursor".into(), p("teal"));
        ui.insert("selection".into(), Rgba::new(0x34, 0xe0, 0xd0, 0x33));
        ui.insert("accent".into(), p("teal"));
        ui.insert("ok".into(), p("green"));
        ui.insert("error".into(), p("red"));
        ui.insert("warning".into(), p("amber"));

        let mut syntax = BTreeMap::new();
        syntax.insert("keyword".into(), p("slate"));
        syntax.insert("function".into(), p("teal"));
        syntax.insert("string".into(), p("sage"));
        syntax.insert("comment".into(), p("dim"));
        syntax.insert("type".into(), p("steel"));
        syntax.insert("constant".into(), p("sand"));
        syntax.insert("number".into(), p("sand"));
        syntax.insert("variable".into(), p("text"));

        Theme {
            name: "wired-noir".into(),
            appearance: Appearance::Dark,
            palette,
            ui,
            syntax,
        }
    }

    /// The HOUSE BRAND DEFAULT, shared by every Itasha.Corp app (and identical
    /// in spirit to C0PL4ND's `itasha-corp` theme). Two brand primaries:
    /// electric purple #7700FF ("Itasha", structural/keyword voice) + spring
    /// green #00FF90 (".Corp", the live accent/cursor/function voice). Deep
    /// purple-black hull, off-white text, Akira-red alarm-only.
    pub fn itasha_corp() -> Theme {
        let mut palette = BTreeMap::new();
        palette.insert("void".into(), Rgba::new(0x12, 0x12, 0x12, 255)); // neutral dark grey-black
        palette.insert("panel".into(), Rgba::new(0x1c, 0x1c, 0x1f, 255));
        palette.insert("bezel".into(), Rgba::new(0x2c, 0x2c, 0x33, 255));
        palette.insert("text".into(), Rgba::new(0xe8, 0xe6, 0xf0, 255));
        palette.insert("muted".into(), Rgba::new(0x6a, 0x64, 0x88, 255));
        palette.insert("dim".into(), Rgba::new(0x4a, 0x43, 0x66, 255));
        palette.insert("green".into(), Rgba::new(0x00, 0xff, 0x90, 255)); // .Corp — the live voice
        palette.insert("purple".into(), Rgba::new(0x77, 0x00, 0xff, 255)); // Itasha — structural voice
        palette.insert("red".into(), Rgba::new(0xff, 0x3b, 0x5c, 255)); // alarms only
        palette.insert("amber".into(), Rgba::new(0xff, 0xc4, 0x4d, 255)); // warnings
        palette.insert("sage".into(), Rgba::new(0x8d, 0xa8, 0x8c, 255)); // string
        palette.insert("steel".into(), Rgba::new(0xa9, 0xc2, 0xcc, 255)); // type
        palette.insert("sand".into(), Rgba::new(0xc9, 0xa8, 0x6a, 255)); // constant/number

        let p = |k: &str| *palette.get(k).unwrap();
        let mut ui = BTreeMap::new();
        ui.insert("background".into(), p("void"));
        ui.insert("foreground".into(), p("text"));
        ui.insert("panel".into(), p("panel"));
        ui.insert("bezel".into(), p("bezel"));
        ui.insert("gutter".into(), p("void"));
        ui.insert("line_number".into(), p("muted"));
        ui.insert("line_number_active".into(), p("green"));
        ui.insert("cursor".into(), p("green"));
        ui.insert("selection".into(), Rgba::new(0x77, 0x00, 0xff, 0x40)); // purple wash
        ui.insert("accent".into(), p("green"));
        ui.insert("ok".into(), p("green"));
        ui.insert("error".into(), p("red"));
        ui.insert("warning".into(), p("amber"));

        let mut syntax = BTreeMap::new();
        syntax.insert("keyword".into(), p("purple")); // Itasha purple
        syntax.insert("function".into(), p("green")); // .Corp green
        syntax.insert("string".into(), p("sage"));
        syntax.insert("comment".into(), p("dim"));
        syntax.insert("type".into(), p("steel"));
        syntax.insert("constant".into(), p("sand"));
        syntax.insert("number".into(), p("sand"));
        syntax.insert("variable".into(), p("text"));

        Theme {
            name: "itasha-corp".into(),
            appearance: Appearance::Dark,
            palette,
            ui,
            syntax,
        }
    }

    /// Alt brand theme — phosphor-amber (BBS / hairline-terminal heritage).
    /// Approved sibling to wired-noir under DECISION-2026-005; the palette
    /// shifts to an amber-on-deep-brown phosphor read while keeping the wired
    /// vocabulary (one accent, alarm-only red, restrained warnings).
    pub fn phosphor_amber() -> Theme {
        let mut palette = BTreeMap::new();
        palette.insert("void".into(), Rgba::new(0x0c, 0x07, 0x05, 255));
        palette.insert("panel".into(), Rgba::new(0x14, 0x0c, 0x07, 255));
        palette.insert("bezel".into(), Rgba::new(0x24, 0x18, 0x0c, 255));
        palette.insert("text".into(), Rgba::new(0xf2, 0xc4, 0x6a, 255));
        palette.insert("muted".into(), Rgba::new(0x8a, 0x6a, 0x3a, 255));
        palette.insert("dim".into(), Rgba::new(0x6a, 0x4f, 0x2a, 255));
        palette.insert("amber".into(), Rgba::new(0xff, 0xc0, 0x4a, 255)); // system voice
        palette.insert("red".into(), Rgba::new(0xff, 0x4a, 0x30, 255)); // alarms only
        palette.insert("yellow".into(), Rgba::new(0xff, 0xe1, 0x7a, 255)); // warnings
        palette.insert("green".into(), Rgba::new(0x8a, 0xb0, 0x6a, 255));
        palette.insert("slate".into(), Rgba::new(0xa0, 0x80, 0x4a, 255));
        palette.insert("sage".into(), Rgba::new(0x9a, 0x8a, 0x5a, 255));
        palette.insert("steel".into(), Rgba::new(0xc4, 0xa8, 0x6a, 255));
        palette.insert("sand".into(), Rgba::new(0xe8, 0xc4, 0x8a, 255));

        let p = |k: &str| *palette.get(k).unwrap();
        let mut ui = BTreeMap::new();
        ui.insert("background".into(), p("void"));
        ui.insert("foreground".into(), p("text"));
        ui.insert("panel".into(), p("panel"));
        ui.insert("bezel".into(), p("bezel"));
        ui.insert("gutter".into(), p("void"));
        ui.insert("line_number".into(), p("muted"));
        ui.insert("line_number_active".into(), p("amber"));
        ui.insert("cursor".into(), p("amber"));
        ui.insert("selection".into(), Rgba::new(0xff, 0xc0, 0x4a, 0x33));
        ui.insert("accent".into(), p("amber"));
        ui.insert("ok".into(), p("green"));
        ui.insert("error".into(), p("red"));
        ui.insert("warning".into(), p("yellow"));

        let mut syntax = BTreeMap::new();
        syntax.insert("keyword".into(), p("slate"));
        syntax.insert("function".into(), p("amber"));
        syntax.insert("string".into(), p("sage"));
        syntax.insert("comment".into(), p("dim"));
        syntax.insert("type".into(), p("steel"));
        syntax.insert("constant".into(), p("sand"));
        syntax.insert("number".into(), p("sand"));
        syntax.insert("variable".into(), p("text"));
        Theme {
            name: "phosphor-amber".into(),
            appearance: Appearance::Dark,
            palette,
            ui,
            syntax,
        }
    }

    /// Alt brand theme — lain-mauve (Wired-era violet melancholy). Sibling to
    /// wired-noir, same shape; the accent shifts to a cool mauve and the
    /// background gains a faint violet bias.
    pub fn lain_mauve() -> Theme {
        let mut palette = BTreeMap::new();
        palette.insert("void".into(), Rgba::new(0x0a, 0x07, 0x0e, 255));
        palette.insert("panel".into(), Rgba::new(0x12, 0x0e, 0x18, 255));
        palette.insert("bezel".into(), Rgba::new(0x22, 0x1c, 0x2e, 255));
        palette.insert("text".into(), Rgba::new(0xd2, 0xc6, 0xdc, 255));
        palette.insert("muted".into(), Rgba::new(0x6a, 0x5a, 0x73, 255));
        palette.insert("dim".into(), Rgba::new(0x4f, 0x42, 0x5a, 255));
        palette.insert("mauve".into(), Rgba::new(0xc8, 0x9a, 0xe8, 255)); // system voice
        palette.insert("red".into(), Rgba::new(0xff, 0x3b, 0x30, 255)); // alarms only
        palette.insert("amber".into(), Rgba::new(0xf2, 0xb3, 0x3d, 255)); // warnings
        palette.insert("green".into(), Rgba::new(0x8a, 0xb8, 0x9a, 255));
        palette.insert("slate".into(), Rgba::new(0x8a, 0x9a, 0xb8, 255));
        palette.insert("sage".into(), Rgba::new(0x9a, 0xa8, 0x9a, 255));
        palette.insert("steel".into(), Rgba::new(0xb0, 0xa0, 0xc2, 255));
        palette.insert("sand".into(), Rgba::new(0xc9, 0xa8, 0xc4, 255));

        let p = |k: &str| *palette.get(k).unwrap();
        let mut ui = BTreeMap::new();
        ui.insert("background".into(), p("void"));
        ui.insert("foreground".into(), p("text"));
        ui.insert("panel".into(), p("panel"));
        ui.insert("bezel".into(), p("bezel"));
        ui.insert("gutter".into(), p("void"));
        ui.insert("line_number".into(), p("muted"));
        ui.insert("line_number_active".into(), p("mauve"));
        ui.insert("cursor".into(), p("mauve"));
        ui.insert("selection".into(), Rgba::new(0xc8, 0x9a, 0xe8, 0x33));
        ui.insert("accent".into(), p("mauve"));
        ui.insert("ok".into(), p("green"));
        ui.insert("error".into(), p("red"));
        ui.insert("warning".into(), p("amber"));

        let mut syntax = BTreeMap::new();
        syntax.insert("keyword".into(), p("slate"));
        syntax.insert("function".into(), p("mauve"));
        syntax.insert("string".into(), p("sage"));
        syntax.insert("comment".into(), p("dim"));
        syntax.insert("type".into(), p("steel"));
        syntax.insert("constant".into(), p("sand"));
        syntax.insert("number".into(), p("sand"));
        syntax.insert("variable".into(), p("text"));
        Theme {
            name: "lain-mauve".into(),
            appearance: Appearance::Dark,
            palette,
            ui,
            syntax,
        }
    }

    /// Light theme — `ghost-paper`. Warm-paper background, ink-grey text, the
    /// same teal accent as wired-noir for system-voice continuity. Meets WCAG AA
    /// for body fg/bg.
    pub fn ghost_paper() -> Theme {
        let mut palette = BTreeMap::new();
        palette.insert("paper".into(), Rgba::new(0xf5, 0xf2, 0xea, 255));
        palette.insert("panel".into(), Rgba::new(0xe8, 0xe4, 0xd8, 255));
        palette.insert("bezel".into(), Rgba::new(0xd4, 0xcf, 0xc0, 255));
        palette.insert("ink".into(), Rgba::new(0x18, 0x1c, 0x22, 255));
        palette.insert("muted".into(), Rgba::new(0x5a, 0x60, 0x6a, 255));
        palette.insert("dim".into(), Rgba::new(0x7a, 0x80, 0x88, 255));
        palette.insert("teal".into(), Rgba::new(0x0a, 0x80, 0x78, 255)); // system voice (darker for light bg)
        palette.insert("red".into(), Rgba::new(0xc4, 0x1e, 0x14, 255)); // alarms only
        palette.insert("amber".into(), Rgba::new(0xa6, 0x70, 0x12, 255)); // warnings
        palette.insert("green".into(), Rgba::new(0x2a, 0x70, 0x4a, 255));
        palette.insert("slate".into(), Rgba::new(0x2a, 0x4a, 0x70, 255));
        palette.insert("sage".into(), Rgba::new(0x40, 0x60, 0x40, 255));
        palette.insert("steel".into(), Rgba::new(0x30, 0x4a, 0x5a, 255));
        palette.insert("sand".into(), Rgba::new(0x6a, 0x4a, 0x12, 255));

        let p = |k: &str| *palette.get(k).unwrap();
        let mut ui = BTreeMap::new();
        ui.insert("background".into(), p("paper"));
        ui.insert("foreground".into(), p("ink"));
        ui.insert("panel".into(), p("panel"));
        ui.insert("bezel".into(), p("bezel"));
        ui.insert("gutter".into(), p("paper"));
        ui.insert("line_number".into(), p("muted"));
        ui.insert("line_number_active".into(), p("teal"));
        ui.insert("cursor".into(), p("teal"));
        ui.insert("selection".into(), Rgba::new(0x0a, 0x80, 0x78, 0x33));
        ui.insert("accent".into(), p("teal"));
        ui.insert("ok".into(), p("green"));
        ui.insert("error".into(), p("red"));
        ui.insert("warning".into(), p("amber"));

        let mut syntax = BTreeMap::new();
        syntax.insert("keyword".into(), p("slate"));
        syntax.insert("function".into(), p("teal"));
        syntax.insert("string".into(), p("sage"));
        syntax.insert("comment".into(), p("dim"));
        syntax.insert("type".into(), p("steel"));
        syntax.insert("constant".into(), p("sand"));
        syntax.insert("number".into(), p("sand"));
        syntax.insert("variable".into(), p("ink"));
        Theme {
            name: "ghost-paper".into(),
            appearance: Appearance::Light,
            palette,
            ui,
            syntax,
        }
    }

    /// Accessibility theme — `a11y-high-contrast`. Pure-white text on near-
    /// black, a single saturated teal accent at maximum chroma, and syntax
    /// colours pushed to high-contrast complements. Targets WCAG AAA body
    /// contrast (>= 7:1) for the low-vision audience the WCAG 2.2 AA brand
    /// commitment leaves room above. Same shape as the other built-ins so
    /// it picks every chrome + syntax slot without holes.
    pub fn a11y_high_contrast() -> Theme {
        let mut palette = BTreeMap::new();
        palette.insert("void".into(), Rgba::new(0x00, 0x00, 0x00, 255));
        palette.insert("panel".into(), Rgba::new(0x06, 0x07, 0x08, 255));
        palette.insert("bezel".into(), Rgba::new(0x12, 0x14, 0x18, 255));
        palette.insert("text".into(), Rgba::new(0xFF, 0xFF, 0xFF, 255));
        palette.insert("muted".into(), Rgba::new(0xC0, 0xC8, 0xCC, 255));
        palette.insert("dim".into(), Rgba::new(0x9A, 0xA6, 0xAC, 255));
        palette.insert("teal".into(), Rgba::new(0x00, 0xFF, 0xE0, 255));
        palette.insert("red".into(), Rgba::new(0xFF, 0x40, 0x30, 255));
        palette.insert("amber".into(), Rgba::new(0xFF, 0xCC, 0x00, 255));
        palette.insert("green".into(), Rgba::new(0x70, 0xFF, 0xA0, 255));
        palette.insert("slate".into(), Rgba::new(0x9A, 0xC4, 0xFF, 255));
        palette.insert("sage".into(), Rgba::new(0xB0, 0xFF, 0xB0, 255));
        palette.insert("steel".into(), Rgba::new(0xC0, 0xE0, 0xFF, 255));
        palette.insert("sand".into(), Rgba::new(0xFF, 0xD8, 0x80, 255));

        let p = |k: &str| *palette.get(k).unwrap();
        let mut ui = BTreeMap::new();
        ui.insert("background".into(), p("void"));
        ui.insert("foreground".into(), p("text"));
        ui.insert("panel".into(), p("panel"));
        ui.insert("bezel".into(), p("bezel"));
        ui.insert("gutter".into(), p("void"));
        ui.insert("line_number".into(), p("muted"));
        ui.insert("line_number_active".into(), p("teal"));
        ui.insert("cursor".into(), p("teal"));
        // Selection alpha pushed up vs wired-noir for visibility at the
        // higher base contrast.
        ui.insert("selection".into(), Rgba::new(0x00, 0xFF, 0xE0, 0x55));
        ui.insert("accent".into(), p("teal"));
        ui.insert("ok".into(), p("green"));
        ui.insert("error".into(), p("red"));
        ui.insert("warning".into(), p("amber"));

        let mut syntax = BTreeMap::new();
        syntax.insert("keyword".into(), p("slate"));
        syntax.insert("function".into(), p("teal"));
        syntax.insert("string".into(), p("sage"));
        syntax.insert("comment".into(), p("dim"));
        syntax.insert("type".into(), p("steel"));
        syntax.insert("constant".into(), p("sand"));
        syntax.insert("number".into(), p("sand"));
        syntax.insert("variable".into(), p("text"));
        Theme {
            name: "a11y-high-contrast".into(),
            appearance: Appearance::Dark,
            palette,
            ui,
            syntax,
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    //  DECISION-2026-009 brand-palette refresh (Phase 22 brand line)
    //
    // Six-theme `itasha-neon` family (the marketing brand-LINE) and eight
    // heritage-alt influence palettes. wired-noir REMAINS the editor's
    // default; the new family ADDS to canon. Each palette carries the same
    // 13-key shape so every chrome + syntax slot is covered with no holes.
    // Accent discipline: one accent = system voice; Akira-red alarm-only;
    // orange + yellow accent-only (with two documented exceptions cabined
    // to opt-in-only themes — akira-redshift and atompunk-sodium).
    // ─────────────────────────────────────────────────────────────────────

    /// Main neon palette `itasha-neon` — the user-seed 13 colours reconciled
    /// with the Itten complementary-pair discipline (cyan promoted to system
    /// voice; hot-pink/fuchsia/deep-purple demoted to syntax-token roles).
    pub fn itasha_neon() -> Theme {
        let mut palette = BTreeMap::new();
        palette.insert("void".into(), Rgba::new(0x00, 0x00, 0x00, 255));
        palette.insert("panel".into(), Rgba::new(0x0a, 0x0a, 0x0a, 255));
        palette.insert("bezel".into(), Rgba::new(0x16, 0x16, 0x16, 255));
        palette.insert("text".into(), Rgba::new(0xff, 0xff, 0xff, 255));
        palette.insert("muted".into(), Rgba::new(0x8c, 0x8c, 0x8c, 255));
        palette.insert("dim".into(), Rgba::new(0x46, 0x46, 0x46, 255));
        palette.insert("cyan".into(), Rgba::new(0x00, 0xff, 0xff, 255));
        palette.insert("red".into(), Rgba::new(0xff, 0x00, 0x00, 255));
        palette.insert("amber".into(), Rgba::new(0xf2, 0xb3, 0x3d, 255));
        palette.insert("green".into(), Rgba::new(0x00, 0xff, 0x80, 255));
        palette.insert("hotpink".into(), Rgba::new(0xff, 0x00, 0x50, 255));
        palette.insert("fuchsia".into(), Rgba::new(0xff, 0x00, 0xaa, 255));
        palette.insert("ultra".into(), Rgba::new(0x22, 0x00, 0xff, 255));
        palette.insert("violet".into(), Rgba::new(0x5d, 0x00, 0xff, 255));
        palette.insert("deep".into(), Rgba::new(0x39, 0x00, 0x9a, 255));
        palette.insert("lime".into(), Rgba::new(0x08, 0xff, 0x00, 255));

        let p = |k: &str| *palette.get(k).unwrap();
        let mut ui = BTreeMap::new();
        ui.insert("background".into(), p("void"));
        ui.insert("foreground".into(), p("text"));
        ui.insert("panel".into(), p("panel"));
        ui.insert("bezel".into(), p("bezel"));
        ui.insert("gutter".into(), p("void"));
        ui.insert("line_number".into(), p("muted"));
        ui.insert("line_number_active".into(), p("cyan"));
        ui.insert("cursor".into(), p("cyan"));
        ui.insert("selection".into(), Rgba::new(0x00, 0xff, 0xff, 0x33));
        ui.insert("accent".into(), p("cyan"));
        ui.insert("ok".into(), p("green"));
        ui.insert("error".into(), p("red"));
        ui.insert("warning".into(), p("amber"));

        let mut syntax = BTreeMap::new();
        syntax.insert("keyword".into(), p("ultra"));
        syntax.insert("function".into(), p("hotpink"));
        syntax.insert("string".into(), p("lime"));
        syntax.insert("comment".into(), p("dim"));
        syntax.insert("type".into(), p("violet"));
        syntax.insert("constant".into(), p("fuchsia"));
        syntax.insert("number".into(), p("fuchsia"));
        syntax.insert("variable".into(), p("text"));
        Theme {
            name: "itasha-neon".into(),
            appearance: Appearance::Dark,
            palette,
            ui,
            syntax,
        }
    }

    /// `itasha-neon-pastel` — ~30% chroma; 8-hour-session comfort variant.
    pub fn itasha_neon_pastel() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x0e, 0x0e, 0x10, 255));
        p.insert("panel".into(), Rgba::new(0x16, 0x17, 0x1a, 255));
        p.insert("bezel".into(), Rgba::new(0x1f, 0x21, 0x25, 255));
        p.insert("text".into(), Rgba::new(0xdf, 0xe3, 0xe6, 255));
        p.insert("muted".into(), Rgba::new(0x7a, 0x80, 0x88, 255));
        p.insert("dim".into(), Rgba::new(0x52, 0x57, 0x62, 255));
        p.insert("cyan".into(), Rgba::new(0x9e, 0xe5, 0xe5, 255));
        p.insert("red".into(), Rgba::new(0xff, 0x7a, 0x78, 255));
        p.insert("amber".into(), Rgba::new(0xf2, 0xc9, 0x8d, 255));
        p.insert("green".into(), Rgba::new(0x9e, 0xd4, 0xb8, 255));
        p.insert("hotpink".into(), Rgba::new(0xff, 0x97, 0xb3, 255));
        p.insert("fuchsia".into(), Rgba::new(0xd8, 0x96, 0xc8, 255));
        p.insert("ultra".into(), Rgba::new(0x8a, 0x92, 0xd6, 255));
        p.insert("violet".into(), Rgba::new(0xa6, 0x98, 0xc8, 255));
        p.insert("deep".into(), Rgba::new(0x5a, 0x4a, 0x7a, 255));
        p.insert("lime".into(), Rgba::new(0xa4, 0xd4, 0x93, 255));
        let ui = neon_ui_map(
            &p, "void", "text", "panel", "bezel", "muted", "cyan", "green", "red", "amber",
        );
        let syntax = neon_syntax_map(&p, "text");
        Theme {
            name: "itasha-neon-pastel".into(),
            appearance: Appearance::Dark,
            palette: p,
            ui,
            syntax,
        }
    }

    /// `itasha-neon-soft` — ~60% chroma; between full-neon and pastel.
    pub fn itasha_neon_soft() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x07, 0x07, 0x10, 255));
        p.insert("panel".into(), Rgba::new(0x10, 0x10, 0x1c, 255));
        p.insert("bezel".into(), Rgba::new(0x19, 0x19, 0x26, 255));
        p.insert("text".into(), Rgba::new(0xe8, 0xe8, 0xf0, 255));
        p.insert("muted".into(), Rgba::new(0x7a, 0x7a, 0x8a, 255));
        p.insert("dim".into(), Rgba::new(0x44, 0x44, 0x52, 255));
        p.insert("cyan".into(), Rgba::new(0x5c, 0xff, 0xe5, 255));
        p.insert("red".into(), Rgba::new(0xff, 0x5a, 0x55, 255));
        p.insert("amber".into(), Rgba::new(0xf2, 0xc0, 0x68, 255));
        p.insert("green".into(), Rgba::new(0x5c, 0xff, 0x9c, 255));
        p.insert("hotpink".into(), Rgba::new(0xff, 0x5a, 0x8c, 255));
        p.insert("fuchsia".into(), Rgba::new(0xe2, 0x5c, 0xc4, 255));
        p.insert("ultra".into(), Rgba::new(0x55, 0x60, 0xe8, 255));
        p.insert("violet".into(), Rgba::new(0x8b, 0x5c, 0xff, 255));
        p.insert("deep".into(), Rgba::new(0x3b, 0x2a, 0x78, 255));
        p.insert("lime".into(), Rgba::new(0x62, 0xe8, 0x5a, 255));
        Theme {
            name: "itasha-neon-soft".into(),
            appearance: Appearance::Dark,
            ui: neon_ui_map(
                &p, "void", "text", "panel", "bezel", "muted", "cyan", "green", "red", "amber",
            ),
            syntax: neon_syntax_map(&p, "text"),
            palette: p,
        }
    }

    /// `itasha-neon-night` — deepest-contrast variant; pure-black void.
    pub fn itasha_neon_night() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x00, 0x00, 0x00, 255));
        p.insert("panel".into(), Rgba::new(0x05, 0x05, 0x05, 255));
        p.insert("bezel".into(), Rgba::new(0x0c, 0x0c, 0x0c, 255));
        p.insert("text".into(), Rgba::new(0xff, 0xff, 0xff, 255));
        p.insert("muted".into(), Rgba::new(0xa0, 0xa0, 0xa0, 255));
        p.insert("dim".into(), Rgba::new(0x50, 0x50, 0x50, 255));
        p.insert("cyan".into(), Rgba::new(0x00, 0xff, 0xff, 255));
        p.insert("red".into(), Rgba::new(0xff, 0x3b, 0x30, 255));
        p.insert("amber".into(), Rgba::new(0xf2, 0xb3, 0x3d, 255));
        p.insert("green".into(), Rgba::new(0x00, 0xff, 0x80, 255));
        p.insert("hotpink".into(), Rgba::new(0xff, 0x00, 0x50, 255));
        p.insert("fuchsia".into(), Rgba::new(0xff, 0x00, 0xaa, 255));
        p.insert("ultra".into(), Rgba::new(0x55, 0x70, 0xff, 255));
        p.insert("violet".into(), Rgba::new(0x80, 0x60, 0xff, 255));
        p.insert("deep".into(), Rgba::new(0x39, 0x00, 0x9a, 255));
        p.insert("lime".into(), Rgba::new(0x08, 0xff, 0x00, 255));
        let mut ui = neon_ui_map(
            &p, "void", "text", "panel", "bezel", "muted", "cyan", "green", "red", "amber",
        );
        // Selection alpha bumped to 0x44 for visibility on pure black.
        ui.insert("selection".into(), Rgba::new(0x00, 0xff, 0xff, 0x44));
        let syntax = neon_syntax_map(&p, "text");
        Theme {
            name: "itasha-neon-night".into(),
            appearance: Appearance::Dark,
            palette: p,
            ui,
            syntax,
        }
    }

    /// `itasha-neon-dawn` — the neon line ported to light appearance.
    pub fn itasha_neon_dawn() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("paper".into(), Rgba::new(0xfa, 0xfb, 0xfd, 255));
        p.insert("panel".into(), Rgba::new(0xee, 0xf0, 0xf4, 255));
        p.insert("bezel".into(), Rgba::new(0xd8, 0xdb, 0xe2, 255));
        p.insert("ink".into(), Rgba::new(0x0e, 0x10, 0x14, 255));
        p.insert("muted".into(), Rgba::new(0x5a, 0x60, 0x68, 255));
        p.insert("dim".into(), Rgba::new(0x8a, 0x90, 0x9a, 255));
        p.insert("cyan".into(), Rgba::new(0x0a, 0x90, 0xa0, 255));
        p.insert("red".into(), Rgba::new(0xc4, 0x1e, 0x14, 255));
        p.insert("amber".into(), Rgba::new(0xa6, 0x70, 0x12, 255));
        p.insert("green".into(), Rgba::new(0x1f, 0x7a, 0x44, 255));
        p.insert("hotpink".into(), Rgba::new(0xc5, 0x17, 0x4f, 255));
        p.insert("fuchsia".into(), Rgba::new(0xa0, 0x10, 0x80, 255));
        p.insert("ultra".into(), Rgba::new(0x1f, 0x28, 0xb8, 255));
        p.insert("violet".into(), Rgba::new(0x48, 0x25, 0xc0, 255));
        p.insert("deep".into(), Rgba::new(0x2a, 0x10, 0x7a, 255));
        p.insert("lime".into(), Rgba::new(0x2a, 0x8a, 0x14, 255));
        let mut ui = neon_ui_map(
            &p, "paper", "ink", "panel", "bezel", "muted", "cyan", "green", "red", "amber",
        );
        ui.insert("selection".into(), Rgba::new(0x0a, 0x90, 0xa0, 0x33));
        let syntax = neon_syntax_map(&p, "ink");
        Theme {
            name: "itasha-neon-dawn".into(),
            appearance: Appearance::Light,
            palette: p,
            ui,
            syntax,
        }
    }

    /// `itasha-neon-aurora` — cyan-violet axis only; Wired-net aurora mood.
    pub fn itasha_neon_aurora() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x03, 0x07, 0x0f, 255));
        p.insert("panel".into(), Rgba::new(0x0a, 0x0e, 0x1c, 255));
        p.insert("bezel".into(), Rgba::new(0x14, 0x1a, 0x2e, 255));
        p.insert("text".into(), Rgba::new(0xcc, 0xe5, 0xff, 255));
        p.insert("muted".into(), Rgba::new(0x6a, 0x8a, 0xa8, 255));
        p.insert("dim".into(), Rgba::new(0x48, 0x60, 0x7a, 255));
        p.insert("cyan".into(), Rgba::new(0x5c, 0xff, 0xe5, 255));
        p.insert("violet".into(), Rgba::new(0x8a, 0x78, 0xff, 255));
        p.insert("green".into(), Rgba::new(0x5c, 0xe8, 0xc8, 255));
        p.insert("ultra".into(), Rgba::new(0x5c, 0x80, 0xff, 255));
        p.insert("deep".into(), Rgba::new(0x1a, 0x20, 0x50, 255));
        p.insert("red".into(), Rgba::new(0xff, 0x3b, 0x30, 255));
        p.insert("amber".into(), Rgba::new(0xf2, 0xb3, 0x3d, 255));
        p.insert("lime".into(), Rgba::new(0x5c, 0xff, 0xae, 255));
        p.insert("fuchsia".into(), Rgba::new(0xa0, 0x60, 0xff, 255));
        p.insert("hotpink".into(), Rgba::new(0xc0, 0x60, 0xd8, 255));
        let ui = neon_ui_map(
            &p, "void", "text", "panel", "bezel", "muted", "cyan", "green", "red", "amber",
        );
        let syntax = neon_syntax_map(&p, "text");
        Theme {
            name: "itasha-neon-aurora".into(),
            appearance: Appearance::Dark,
            palette: p,
            ui,
            syntax,
        }
    }

    // ───────── Heritage-alt palettes (DECISION-2026-009 §5) ─────────

    /// `geocities-bbs` — Web 1.0 16-colour cohort (1996–2001). Camp slot —
    /// construction-yellow IS body text by period-correct necessity. NOT
    /// recommended for long sessions.
    pub fn geocities_bbs() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x00, 0x00, 0x80, 255));
        p.insert("panel".into(), Rgba::new(0x0a, 0x0a, 0x8c, 255));
        p.insert("bezel".into(), Rgba::new(0x1a, 0x1a, 0x9a, 255));
        p.insert("text".into(), Rgba::new(0xff, 0xff, 0x00, 255));
        p.insert("muted".into(), Rgba::new(0xaa, 0xaa, 0xaa, 255));
        p.insert("dim".into(), Rgba::new(0x7a, 0x7a, 0x8a, 255));
        p.insert("cyan".into(), Rgba::new(0x00, 0xff, 0xff, 255));
        p.insert("red".into(), Rgba::new(0xff, 0x00, 0x00, 255));
        p.insert("amber".into(), Rgba::new(0xff, 0xaa, 0x00, 255));
        p.insert("green".into(), Rgba::new(0x00, 0xaa, 0x00, 255));
        p.insert("hyperlink".into(), Rgba::new(0x50, 0x50, 0xff, 255));
        p.insert("visited".into(), Rgba::new(0x90, 0x20, 0xa0, 255));
        p.insert("white".into(), Rgba::new(0xff, 0xff, 0xff, 255));
        let g = |k: &str| *p.get(k).unwrap();
        let mut ui = BTreeMap::new();
        ui.insert("background".into(), g("void"));
        ui.insert("foreground".into(), g("text"));
        ui.insert("panel".into(), g("panel"));
        ui.insert("bezel".into(), g("bezel"));
        ui.insert("gutter".into(), g("void"));
        ui.insert("line_number".into(), g("muted"));
        ui.insert("line_number_active".into(), g("cyan"));
        ui.insert("cursor".into(), g("cyan"));
        ui.insert("selection".into(), Rgba::new(0x00, 0xff, 0xff, 0x33));
        ui.insert("accent".into(), g("hyperlink"));
        ui.insert("ok".into(), g("green"));
        ui.insert("error".into(), g("red"));
        ui.insert("warning".into(), g("amber"));
        let mut s = BTreeMap::new();
        s.insert("keyword".into(), g("hyperlink"));
        s.insert("function".into(), g("cyan"));
        s.insert("string".into(), g("green"));
        s.insert("comment".into(), g("muted"));
        s.insert("type".into(), g("visited"));
        s.insert("constant".into(), g("amber"));
        s.insert("number".into(), g("amber"));
        s.insert("variable".into(), g("white"));
        Theme {
            name: "geocities-bbs".into(),
            appearance: Appearance::Dark,
            palette: p,
            ui,
            syntax: s,
        }
    }

    /// `lain-wired` — The Wired deep-violet + copper-circuit accent. Sister
    /// to (and distinct from) the existing `lain-mauve` (which leans pastel).
    pub fn lain_wired() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x08, 0x06, 0x14, 255));
        p.insert("panel".into(), Rgba::new(0x10, 0x0c, 0x1f, 255));
        p.insert("bezel".into(), Rgba::new(0x1c, 0x18, 0x30, 255));
        p.insert("text".into(), Rgba::new(0xbc, 0xb8, 0xe5, 255));
        p.insert("muted".into(), Rgba::new(0x5a, 0x52, 0x7a, 255));
        p.insert("dim".into(), Rgba::new(0x3a, 0x35, 0x50, 255));
        p.insert("mauve".into(), Rgba::new(0x8c, 0x6c, 0xd0, 255));
        p.insert("red".into(), Rgba::new(0xff, 0x3b, 0x30, 255));
        p.insert("copper".into(), Rgba::new(0xc8, 0x9a, 0x4e, 255));
        p.insert("green".into(), Rgba::new(0x7a, 0x9e, 0xc4, 255));
        p.insert("slate".into(), Rgba::new(0x6c, 0x80, 0xb4, 255));
        p.insert("sage".into(), Rgba::new(0x9c, 0x8c, 0xb8, 255));
        p.insert("steel".into(), Rgba::new(0xa0, 0xa8, 0xd4, 255));
        p.insert("sand".into(), Rgba::new(0xc4, 0xa8, 0x90, 255));
        Theme {
            name: "lain-wired".into(),
            appearance: Appearance::Dark,
            ui: heritage_ui_map(
                &p,
                "void",
                "text",
                "panel",
                "bezel",
                "muted",
                "mauve",
                "green",
                "red",
                "copper",
                Rgba::new(0x8c, 0x6c, 0xd0, 0x33),
            ),
            syntax: heritage_syntax_map(&p, "slate", "mauve", "sage", "dim", "steel", "sand"),
            palette: p,
        }
    }

    /// `kusanagi-dive` — Ghost in the Shell (1995) cyan-on-marine palette.
    pub fn kusanagi_dive() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x02, 0x0a, 0x14, 255));
        p.insert("panel".into(), Rgba::new(0x06, 0x12, 0x1f, 255));
        p.insert("bezel".into(), Rgba::new(0x0c, 0x1c, 0x30, 255));
        p.insert("text".into(), Rgba::new(0x9e, 0xc8, 0xe0, 255));
        p.insert("muted".into(), Rgba::new(0x4a, 0x64, 0x78, 255));
        p.insert("dim".into(), Rgba::new(0x32, 0x44, 0x52, 255));
        p.insert("cyan".into(), Rgba::new(0x34, 0xdc, 0xe0, 255));
        p.insert("red".into(), Rgba::new(0xe8, 0x48, 0x3a, 255));
        p.insert("amber".into(), Rgba::new(0xff, 0xa8, 0x48, 255));
        p.insert("green".into(), Rgba::new(0x34, 0xe0, 0xa0, 255));
        p.insert("slate".into(), Rgba::new(0x5e, 0x94, 0xc4, 255));
        p.insert("sage".into(), Rgba::new(0x7e, 0xb8, 0xd8, 255));
        p.insert("steel".into(), Rgba::new(0x9e, 0xc0, 0xd4, 255));
        p.insert("sand".into(), Rgba::new(0xd4, 0xae, 0x7c, 255));
        Theme {
            name: "kusanagi-dive".into(),
            appearance: Appearance::Dark,
            ui: heritage_ui_map(
                &p,
                "void",
                "text",
                "panel",
                "bezel",
                "muted",
                "cyan",
                "green",
                "red",
                "amber",
                Rgba::new(0x34, 0xdc, 0xe0, 0x33),
            ),
            syntax: heritage_syntax_map(&p, "slate", "cyan", "sage", "dim", "steel", "sand"),
            palette: p,
        }
    }

    /// `akira-redshift` — Neo-Tokyo. **OPT-IN ONLY**: red-as-system-voice
    /// is a documented exception to the alarm-only-red discipline (Akira
    /// IS red). UI should warn the user when selecting it.
    pub fn akira_redshift() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x08, 0x06, 0x0c, 255));
        p.insert("panel".into(), Rgba::new(0x1a, 0x0a, 0x14, 255));
        p.insert("bezel".into(), Rgba::new(0x2c, 0x16, 0x22, 255));
        p.insert("text".into(), Rgba::new(0xf0, 0xd8, 0xc0, 255));
        p.insert("muted".into(), Rgba::new(0x8a, 0x6a, 0x78, 255));
        p.insert("dim".into(), Rgba::new(0x5a, 0x3a, 0x4a, 255));
        p.insert("sphere_red".into(), Rgba::new(0xff, 0x20, 0x30, 255));
        p.insert("neon_pink".into(), Rgba::new(0xff, 0x64, 0x9c, 255));
        p.insert("neon_orange".into(), Rgba::new(0xff, 0x9c, 0x3c, 255));
        p.insert("electric_yellow".into(), Rgba::new(0xff, 0xe6, 0x48, 255));
        p.insert("biolume".into(), Rgba::new(0x3a, 0xaf, 0xc8, 255));
        Theme {
            name: "akira-redshift".into(),
            appearance: Appearance::Dark,
            ui: heritage_ui_map(
                &p,
                "void",
                "text",
                "panel",
                "bezel",
                "muted",
                "sphere_red",
                "biolume",
                "sphere_red",
                "neon_orange",
                Rgba::new(0xff, 0x20, 0x30, 0x33),
            ),
            syntax: heritage_syntax_map(
                &p,
                "neon_pink",
                "sphere_red",
                "biolume",
                "dim",
                "neon_orange",
                "electric_yellow",
            ),
            palette: p,
        }
    }

    /// `atompunk-sodium` — Eames-era retro-futurism. **OPT-IN ONLY**:
    /// sodium-orange-as-system-voice is a documented exception (Atompunk
    /// demands it).
    pub fn atompunk_sodium() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x0c, 0x0a, 0x08, 255));
        p.insert("panel".into(), Rgba::new(0x16, 0x13, 0x0e, 255));
        p.insert("bezel".into(), Rgba::new(0x24, 0x1f, 0x18, 255));
        p.insert("text".into(), Rgba::new(0xe0, 0xd8, 0xc8, 255));
        p.insert("muted".into(), Rgba::new(0x8c, 0x84, 0x78, 255));
        p.insert("dim".into(), Rgba::new(0x5a, 0x54, 0x4a, 255));
        p.insert("sodium".into(), Rgba::new(0xff, 0xa0, 0x30, 255));
        p.insert("atomic_teal".into(), Rgba::new(0x3a, 0xca, 0xb8, 255));
        p.insert("chrome".into(), Rgba::new(0xc0, 0xc4, 0xc8, 255));
        p.insert("red".into(), Rgba::new(0xd8, 0x30, 0x2a, 255));
        p.insert("slate".into(), Rgba::new(0x7a, 0x90, 0xb0, 255));
        p.insert("sage".into(), Rgba::new(0x8a, 0xb0, 0x98, 255));
        p.insert("steel".into(), Rgba::new(0xa8, 0xb8, 0xc4, 255));
        p.insert("sand".into(), Rgba::new(0xd0, 0xa8, 0x68, 255));
        Theme {
            name: "atompunk-sodium".into(),
            appearance: Appearance::Dark,
            ui: heritage_ui_map(
                &p,
                "void",
                "text",
                "panel",
                "bezel",
                "muted",
                "sodium",
                "atomic_teal",
                "red",
                "sodium",
                Rgba::new(0xff, 0xa0, 0x30, 0x33),
            ),
            syntax: heritage_syntax_map(&p, "slate", "sodium", "sage", "dim", "steel", "sand"),
            palette: p,
        }
    }

    /// `terminal-lock` — Tektronix 4014 / Hercules green-phosphor heritage.
    pub fn terminal_lock() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x00, 0x00, 0x00, 255));
        p.insert("panel".into(), Rgba::new(0x02, 0x06, 0x02, 255));
        p.insert("bezel".into(), Rgba::new(0x0a, 0x14, 0x0a, 255));
        p.insert("text".into(), Rgba::new(0x33, 0xff, 0x66, 255));
        p.insert("muted".into(), Rgba::new(0x1a, 0x7a, 0x35, 255));
        p.insert("dim".into(), Rgba::new(0x0a, 0x4a, 0x1f, 255));
        p.insert("green".into(), Rgba::new(0x33, 0xff, 0x66, 255));
        p.insert("amber".into(), Rgba::new(0xff, 0xaa, 0x00, 255));
        p.insert("cyan".into(), Rgba::new(0x00, 0xe0, 0xd0, 255));
        p.insert("red".into(), Rgba::new(0xff, 0x30, 0x30, 255));
        p.insert("bright".into(), Rgba::new(0x80, 0xff, 0x80, 255));
        p.insert("slate".into(), Rgba::new(0x33, 0xff, 0x66, 255));
        p.insert("sage".into(), Rgba::new(0x80, 0xff, 0x80, 255));
        p.insert("steel".into(), Rgba::new(0xbb, 0xff, 0xbb, 255));
        p.insert("sand".into(), Rgba::new(0xff, 0xaa, 0x00, 255));
        Theme {
            name: "terminal-lock".into(),
            appearance: Appearance::Dark,
            ui: heritage_ui_map(
                &p,
                "void",
                "text",
                "panel",
                "bezel",
                "muted",
                "green",
                "bright",
                "red",
                "amber",
                Rgba::new(0x33, 0xff, 0x66, 0x33),
            ),
            syntax: heritage_syntax_map(&p, "slate", "green", "sage", "dim", "steel", "sand"),
            palette: p,
        }
    }

    /// `mecha-armour` — RX-78-2 Federation white/blue/red/yellow on graphite.
    pub fn mecha_armour() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x0a, 0x0c, 0x10, 255));
        p.insert("panel".into(), Rgba::new(0x14, 0x18, 0x20, 255));
        p.insert("bezel".into(), Rgba::new(0x20, 0x27, 0x32, 255));
        p.insert("text".into(), Rgba::new(0xe0, 0xe4, 0xec, 255));
        p.insert("muted".into(), Rgba::new(0x6a, 0x74, 0x88, 255));
        p.insert("dim".into(), Rgba::new(0x3a, 0x42, 0x50, 255));
        p.insert("white".into(), Rgba::new(0xdc, 0xe4, 0xec, 255));
        p.insert("blue".into(), Rgba::new(0x00, 0x40, 0xa8, 255));
        p.insert("red".into(), Rgba::new(0xc4, 0x10, 0x28, 255));
        p.insert("yellow".into(), Rgba::new(0xf0, 0xc0, 0x20, 255));
        p.insert("zaku".into(), Rgba::new(0x58, 0x80, 0x48, 255));
        p.insert("chrome".into(), Rgba::new(0xa8, 0xb0, 0xc0, 255));
        p.insert("slate".into(), Rgba::new(0x4a, 0x6a, 0xb8, 255));
        p.insert("sage".into(), Rgba::new(0x7a, 0x88, 0x98, 255));
        p.insert("steel".into(), Rgba::new(0xa8, 0xb0, 0xc0, 255));
        p.insert("sand".into(), Rgba::new(0xf0, 0xc0, 0x20, 255));
        Theme {
            name: "mecha-armour".into(),
            appearance: Appearance::Dark,
            ui: heritage_ui_map(
                &p,
                "void",
                "text",
                "panel",
                "bezel",
                "muted",
                "chrome",
                "zaku",
                "red",
                "yellow",
                Rgba::new(0x00, 0x40, 0xa8, 0x33),
            ),
            syntax: heritage_syntax_map(&p, "slate", "chrome", "sage", "dim", "steel", "sand"),
            palette: p,
        }
    }

    /// `shutoko-night` — 80s-2000s JDM (Itasha brand root). Documented
    /// period paint codes: NH-547 Berlina Black (NSX), BT2 Bayside Blue
    /// (R34 GT-R), Mazda Soul-Red-Crystal precedent.
    pub fn shutoko_night() -> Theme {
        let mut p = BTreeMap::new();
        p.insert("void".into(), Rgba::new(0x05, 0x05, 0x05, 255)); // NH-547
        p.insert("panel".into(), Rgba::new(0x0a, 0x0c, 0x14, 255));
        p.insert("bezel".into(), Rgba::new(0x14, 0x3c, 0x5a, 255));
        p.insert("text".into(), Rgba::new(0xbc, 0xc0, 0xc4, 255)); // nismo_silver
        p.insert("muted".into(), Rgba::new(0x6a, 0x70, 0x80, 255));
        p.insert("dim".into(), Rgba::new(0x3a, 0x40, 0x4a, 255));
        p.insert("bayside".into(), Rgba::new(0x0c, 0x2d, 0x6a, 255)); // BT2
        p.insert("metallic".into(), Rgba::new(0x14, 0x3c, 0x8a, 255));
        p.insert("soul_red".into(), Rgba::new(0x9a, 0x1a, 0x1f, 255));
        p.insert("candy_red".into(), Rgba::new(0xc4, 0x20, 0x30, 255));
        p.insert("sodium".into(), Rgba::new(0xf0, 0xa0, 0x40, 255));
        p.insert("pink".into(), Rgba::new(0xff, 0x30, 0x88, 255)); // underglow
        p.insert("green".into(), Rgba::new(0x28, 0xff, 0x60, 255)); // underglow
        p.insert("slate".into(), Rgba::new(0x0c, 0x2d, 0x6a, 255));
        p.insert("sage".into(), Rgba::new(0x5a, 0x8a, 0x80, 255));
        p.insert("steel".into(), Rgba::new(0xbc, 0xc0, 0xc4, 255));
        p.insert("sand".into(), Rgba::new(0xf0, 0xa0, 0x40, 255));
        Theme {
            name: "shutoko-night".into(),
            appearance: Appearance::Dark,
            ui: heritage_ui_map(
                &p,
                "void",
                "text",
                "panel",
                "bezel",
                "muted",
                "bayside",
                "green",
                "candy_red",
                "sodium",
                Rgba::new(0x0c, 0x2d, 0x6a, 0x55),
            ),
            syntax: heritage_syntax_map(&p, "slate", "pink", "sage", "dim", "steel", "sand"),
            palette: p,
        }
    }

    /// The list of built-in theme names — drives the in-app theme picker.
    /// User themes (TOML files under `<config_dir>/themes/`) compose on top.
    pub fn builtin_names() -> &'static [&'static str] {
        &[
            // House brand default (shared across every Itasha.Corp app).
            "itasha-corp",
            // Existing five (DECISION-2026-005).
            "wired-noir",
            "phosphor-amber",
            "lain-mauve",
            "ghost-paper",
            "a11y-high-contrast",
            // itasha-neon family (DECISION-2026-009).
            "itasha-neon",
            "itasha-neon-pastel",
            "itasha-neon-soft",
            "itasha-neon-night",
            "itasha-neon-dawn",
            "itasha-neon-aurora",
            // Heritage-alt influence palettes (DECISION-2026-009 §5).
            "geocities-bbs",
            "lain-wired",
            "kusanagi-dive",
            "akira-redshift",
            "atompunk-sodium",
            "terminal-lock",
            "mecha-armour",
            "shutoko-night",
        ]
    }

    /// Look up a built-in theme by name. Returns `None` if no built-in matches.
    pub fn builtin(name: &str) -> Option<Theme> {
        match name {
            // House brand default (shared across every Itasha.Corp app).
            "itasha-corp" => Some(Theme::itasha_corp()),
            // Existing five.
            "wired-noir" => Some(Theme::wired_noir()),
            "phosphor-amber" => Some(Theme::phosphor_amber()),
            "lain-mauve" => Some(Theme::lain_mauve()),
            "ghost-paper" => Some(Theme::ghost_paper()),
            "a11y-high-contrast" => Some(Theme::a11y_high_contrast()),
            // itasha-neon family.
            "itasha-neon" => Some(Theme::itasha_neon()),
            "itasha-neon-pastel" => Some(Theme::itasha_neon_pastel()),
            "itasha-neon-soft" => Some(Theme::itasha_neon_soft()),
            "itasha-neon-night" => Some(Theme::itasha_neon_night()),
            "itasha-neon-dawn" => Some(Theme::itasha_neon_dawn()),
            "itasha-neon-aurora" => Some(Theme::itasha_neon_aurora()),
            // Heritage-alt.
            "geocities-bbs" => Some(Theme::geocities_bbs()),
            "lain-wired" => Some(Theme::lain_wired()),
            "kusanagi-dive" => Some(Theme::kusanagi_dive()),
            "akira-redshift" => Some(Theme::akira_redshift()),
            "atompunk-sodium" => Some(Theme::atompunk_sodium()),
            "terminal-lock" => Some(Theme::terminal_lock()),
            "mecha-armour" => Some(Theme::mecha_armour()),
            "shutoko-night" => Some(Theme::shutoko_night()),
            _ => None,
        }
    }

    /// Serialize this theme to a TOML string round-trippable through
    /// `from_toml_str`. Used by the in-app 'Export current theme as user TOML'
    /// flow (Phase 17 T17.6) — the user picks a starting built-in, the active
    /// theme is exported to `<config_dir>/themes/<name>.toml`, and the live-
    /// reload watcher picks up edits to that file.
    pub fn to_toml_string(&self) -> String {
        let appearance = match self.appearance {
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        };
        let mut out = String::new();
        out.push_str(&format!("name = \"{}\"\n", self.name));
        out.push_str(&format!("appearance = \"{}\"\n\n", appearance));
        let render_map = |buf: &mut String, header: &str, m: &BTreeMap<String, Rgba>| {
            buf.push_str(&format!("[{header}]\n"));
            for (k, c) in m {
                buf.push_str(&format!(
                    "{k} = \"#{:02X}{:02X}{:02X}{:02X}\"\n",
                    c.r, c.g, c.b, c.a
                ));
            }
            buf.push('\n');
        };
        render_map(&mut out, "palette", &self.palette);
        render_map(&mut out, "ui", &self.ui);
        render_map(&mut out, "syntax", &self.syntax);
        out
    }

    /// Parse a theme from TOML, resolving palette references in ui/syntax.
    pub fn from_toml_str(s: &str) -> Result<Theme> {
        let raw: RawTheme = toml::from_str(s).map_err(|e| CoreError::ThemeParse(e.to_string()))?;
        let mut palette = BTreeMap::new();
        for (k, v) in &raw.palette {
            let c = Rgba::parse_hex(v)
                .ok_or_else(|| CoreError::ThemeParse(format!("bad palette color {k} = {v}")))?;
            palette.insert(k.clone(), c);
        }
        let resolve = |val: &str| -> Result<Rgba> {
            if let Some(c) = palette.get(val) {
                Ok(*c)
            } else {
                Rgba::parse_hex(val)
                    .ok_or_else(|| CoreError::ThemeParse(format!("unknown color/ref: {val}")))
            }
        };
        let mut ui = BTreeMap::new();
        for (k, v) in &raw.ui {
            ui.insert(k.clone(), resolve(v)?);
        }
        let mut syntax = BTreeMap::new();
        for (k, v) in &raw.syntax {
            syntax.insert(k.clone(), resolve(v)?);
        }
        let appearance = match raw.appearance.as_deref() {
            Some("light") => Appearance::Light,
            _ => Appearance::Dark,
        };
        Ok(Theme {
            name: raw.name.unwrap_or_else(|| "custom".into()),
            appearance,
            palette,
            ui,
            syntax,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────
//  Brand-palette helper constructors (DECISION-2026-009)
//
// All six itasha-neon themes share the same chrome shape (background/
// foreground/panel/bezel/gutter/line-number-active/cursor/selection/accent/
// ok/error/warning) — `neon_ui_map` builds that map from named palette keys.
// `neon_syntax_map` builds the matching keyword/function/string/etc. assignment.
// The heritage-alt themes use a parallel pair of helpers that take an explicit
// `selection` Rgba so each can carry its period-correct selection alpha.
// ─────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn neon_ui_map(
    p: &BTreeMap<String, Rgba>,
    bg: &str,
    fg: &str,
    panel: &str,
    bezel: &str,
    muted: &str,
    voice: &str,
    ok: &str,
    error: &str,
    warning: &str,
) -> BTreeMap<String, Rgba> {
    let take = |k: &str| {
        *p.get(k)
            .unwrap_or_else(|| panic!("missing palette key {k}"))
    };
    let v = take(voice);
    let mut ui = BTreeMap::new();
    ui.insert("background".into(), take(bg));
    ui.insert("foreground".into(), take(fg));
    ui.insert("panel".into(), take(panel));
    ui.insert("bezel".into(), take(bezel));
    ui.insert("gutter".into(), take(bg));
    ui.insert("line_number".into(), take(muted));
    ui.insert("line_number_active".into(), v);
    ui.insert("cursor".into(), v);
    ui.insert("selection".into(), Rgba::new(v.r, v.g, v.b, 0x33));
    ui.insert("accent".into(), v);
    ui.insert("ok".into(), take(ok));
    ui.insert("error".into(), take(error));
    ui.insert("warning".into(), take(warning));
    ui
}

fn neon_syntax_map(p: &BTreeMap<String, Rgba>, text_key: &str) -> BTreeMap<String, Rgba> {
    let take = |k: &str| {
        *p.get(k)
            .unwrap_or_else(|| panic!("missing palette key {k}"))
    };
    let mut s = BTreeMap::new();
    s.insert("keyword".into(), take("ultra"));
    s.insert("function".into(), take("hotpink"));
    s.insert("string".into(), take("lime"));
    s.insert("comment".into(), take("dim"));
    s.insert("type".into(), take("violet"));
    s.insert("constant".into(), take("fuchsia"));
    s.insert("number".into(), take("fuchsia"));
    s.insert("variable".into(), take(text_key));
    s
}

#[allow(clippy::too_many_arguments)]
fn heritage_ui_map(
    p: &BTreeMap<String, Rgba>,
    bg: &str,
    fg: &str,
    panel: &str,
    bezel: &str,
    muted: &str,
    voice: &str,
    ok: &str,
    error: &str,
    warning: &str,
    selection: Rgba,
) -> BTreeMap<String, Rgba> {
    let take = |k: &str| {
        *p.get(k)
            .unwrap_or_else(|| panic!("missing palette key {k}"))
    };
    let v = take(voice);
    let mut ui = BTreeMap::new();
    ui.insert("background".into(), take(bg));
    ui.insert("foreground".into(), take(fg));
    ui.insert("panel".into(), take(panel));
    ui.insert("bezel".into(), take(bezel));
    ui.insert("gutter".into(), take(bg));
    ui.insert("line_number".into(), take(muted));
    ui.insert("line_number_active".into(), v);
    ui.insert("cursor".into(), v);
    ui.insert("selection".into(), selection);
    ui.insert("accent".into(), v);
    ui.insert("ok".into(), take(ok));
    ui.insert("error".into(), take(error));
    ui.insert("warning".into(), take(warning));
    ui
}

fn heritage_syntax_map(
    p: &BTreeMap<String, Rgba>,
    keyword: &str,
    function: &str,
    string: &str,
    comment: &str,
    ty: &str,
    constant: &str,
) -> BTreeMap<String, Rgba> {
    let take = |k: &str| {
        *p.get(k)
            .unwrap_or_else(|| panic!("missing palette key {k}"))
    };
    let mut s = BTreeMap::new();
    s.insert("keyword".into(), take(keyword));
    s.insert("function".into(), take(function));
    s.insert("string".into(), take(string));
    s.insert("comment".into(), take(comment));
    s.insert("type".into(), take(ty));
    s.insert("constant".into(), take(constant));
    s.insert("number".into(), take(constant));
    // Heritage palettes universally name the foreground "text"; the helper
    // assumes that for syntax variable colouring.
    s.insert("variable".into(), take("text"));
    s
}

#[derive(Debug, Deserialize, Default)]
struct RawTheme {
    name: Option<String>,
    appearance: Option<String>,
    #[serde(default)]
    palette: BTreeMap<String, String>,
    #[serde(default)]
    ui: BTreeMap<String, String>,
    #[serde(default)]
    syntax: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_forms() {
        assert_eq!(Rgba::parse_hex("#fff"), Some(Rgba::new(255, 255, 255, 255)));
        assert_eq!(Rgba::parse_hex("#08060d"), Some(Rgba::new(8, 6, 13, 255)));
        assert_eq!(
            Rgba::parse_hex("#00fffe80"),
            Some(Rgba::new(0, 255, 254, 128))
        );
        assert_eq!(Rgba::parse_hex("nope"), None);
    }

    #[test]
    fn builtin_brand_theme() {
        let t = Theme::wired_noir();
        assert_eq!(
            t.ui("background", Rgba::new(0, 0, 0, 0)),
            Rgba::new(7, 10, 12, 255)
        );
        assert_eq!(
            t.ui("accent", Rgba::new(0, 0, 0, 0)),
            Rgba::new(0x34, 0xe0, 0xd0, 255)
        );
    }

    #[test]
    fn all_builtins_construct_and_dispatch() {
        // Phase 17 T17.2: every name in `builtin_names()` resolves via `builtin()`
        // and the returned theme carries that exact name + matching appearance.
        for name in Theme::builtin_names() {
            let t = Theme::builtin(name).unwrap_or_else(|| panic!("missing builtin {name}"));
            assert_eq!(t.name, *name, "name round-trip");
            // Every built-in must define the chrome essentials.
            for key in ["background", "foreground", "panel", "accent"] {
                assert!(t.ui.contains_key(key), "{name} missing ui.{key}");
            }
        }
    }

    #[test]
    fn ghost_paper_is_light() {
        assert!(matches!(Theme::ghost_paper().appearance, Appearance::Light));
    }

    #[test]
    fn unknown_builtin_returns_none() {
        assert!(Theme::builtin("nonexistent-theme").is_none());
    }

    #[test]
    fn to_toml_string_roundtrips_through_from_toml_str() {
        // T17.6: the serializer + parser are inverses. Lets the in-app
        // 'Export current theme' flow produce a TOML the live-reload watcher
        // can read back without loss.
        for name in Theme::builtin_names() {
            let original = Theme::builtin(name).unwrap();
            let toml_str = original.to_toml_string();
            let parsed = Theme::from_toml_str(&toml_str)
                .unwrap_or_else(|e| panic!("round-trip failed for {name}: {e}"));
            assert_eq!(parsed.name, original.name);
            assert_eq!(parsed.palette.len(), original.palette.len());
            assert_eq!(parsed.ui.len(), original.ui.len());
            assert_eq!(parsed.syntax.len(), original.syntax.len());
            // Spot-check a few critical chrome keys.
            for key in ["background", "foreground", "accent", "panel"] {
                let zero = Rgba::new(0, 0, 0, 0);
                assert_eq!(
                    parsed.ui(key, zero),
                    original.ui(key, zero),
                    "{name}.ui.{key} drifted"
                );
            }
        }
    }

    #[test]
    fn longest_scope_wins() {
        let t = Theme::wired_noir();
        let kw = t.syntax_color("keyword", Rgba::new(0, 0, 0, 0));
        // unknown sub-scope falls back to "keyword"
        assert_eq!(
            t.syntax_color("keyword.control.return", Rgba::new(0, 0, 0, 0)),
            kw
        );
    }

    #[test]
    fn parse_theme_with_palette_refs() {
        let toml = r##"
name = "t"
appearance = "dark"
[palette]
bg = "#101010"
fg = "#fafafa"
[ui]
background = "bg"
foreground = "fg"
[syntax]
keyword = "#ff00ff"
"##;
        let t = Theme::from_toml_str(toml).unwrap();
        assert_eq!(
            t.ui("background", Rgba::new(0, 0, 0, 0)),
            Rgba::new(16, 16, 16, 255)
        );
        assert_eq!(
            t.syntax_color("keyword", Rgba::new(0, 0, 0, 0)),
            Rgba::new(255, 0, 255, 255)
        );
    }
}
