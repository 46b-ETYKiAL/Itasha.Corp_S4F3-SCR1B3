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

    /// The compiled-in fallback / default brand theme. VOID black, SIGNAL CYAN,
    /// STATUS GREEN — the Itasha.Corp CRT palette.
    pub fn itasha_void() -> Theme {
        let mut palette = BTreeMap::new();
        palette.insert("void".into(), Rgba::new(0x08, 0x06, 0x0d, 255));
        palette.insert("bezel".into(), Rgba::new(0x11, 0x11, 0x18, 255));
        palette.insert("panel".into(), Rgba::new(0x0d, 0x0b, 0x14, 255));
        palette.insert("text".into(), Rgba::new(0xd6, 0xe2, 0xf0, 255));
        palette.insert("muted".into(), Rgba::new(0x5a, 0x58, 0x69, 255));
        palette.insert("cyan".into(), Rgba::new(0x00, 0xff, 0xfe, 255));
        palette.insert("green".into(), Rgba::new(0x01, 0xfe, 0x36, 255));
        palette.insert("magenta".into(), Rgba::new(0xd9, 0x46, 0xef, 255));
        palette.insert("yellow".into(), Rgba::new(0xfb, 0xbf, 0x24, 255));
        palette.insert("orange".into(), Rgba::new(0xfb, 0x92, 0x3c, 255));
        palette.insert("red".into(), Rgba::new(0xff, 0x00, 0x40, 255));

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
        ui.insert("selection".into(), Rgba::new(0x00, 0xff, 0xfe, 0x33));
        ui.insert("accent".into(), p("cyan"));
        ui.insert("ok".into(), p("green"));
        ui.insert("error".into(), p("red"));
        ui.insert("warning".into(), p("yellow"));

        let mut syntax = BTreeMap::new();
        syntax.insert("keyword".into(), p("magenta"));
        syntax.insert("function".into(), p("cyan"));
        syntax.insert("string".into(), p("green"));
        syntax.insert("comment".into(), p("muted"));
        syntax.insert("type".into(), p("yellow"));
        syntax.insert("constant".into(), p("orange"));
        syntax.insert("number".into(), p("orange"));
        syntax.insert("variable".into(), p("text"));

        Theme {
            name: "itasha-void".into(),
            appearance: Appearance::Dark,
            palette,
            ui,
            syntax,
        }
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
        let t = Theme::itasha_void();
        assert_eq!(
            t.ui("background", Rgba::new(0, 0, 0, 0)),
            Rgba::new(8, 6, 13, 255)
        );
        assert_eq!(
            t.ui("accent", Rgba::new(0, 0, 0, 0)),
            Rgba::new(0, 255, 254, 255)
        );
    }

    #[test]
    fn longest_scope_wins() {
        let t = Theme::itasha_void();
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
