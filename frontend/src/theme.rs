//! Theme and accent colour, applied at runtime.
//!
//! Every accent utility reads `--ac-200` … `--ac-900` (see `style/app.css`),
//! so a custom accent is one ramp computed from the chosen colour and set
//! on `:root`. The animated backdrop takes its palette as inline CSS
//! variables on its container; a custom theme overrides those from a
//! stylesheet with `!important`, which outranks inline declarations.

use infinity_ipc::dto::{Preferences, Theme};
use leptos::prelude::*;

const STYLE_ID: &str = "infinity-theme";
/// Class on the backdrop container the custom palette targets.
pub(crate) const BACKDROP_CLASS: &str = "app-backdrop";

/// Ramp steps and the lightness each sits at, close to Tailwind's emerald.
const RAMP: [(u16, f64); 7] = [
    (200, 0.82),
    (300, 0.69),
    (400, 0.52),
    (500, 0.40),
    (600, 0.33),
    (700, 0.27),
    (900, 0.17),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rgb(pub(crate) u8, pub(crate) u8, pub(crate) u8);

impl Rgb {
    /// `#rrggbb` or `rrggbb`.
    pub(crate) fn parse(hex: &str) -> Option<Self> {
        let hex = hex.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self(byte(0)?, byte(2)?, byte(4)?))
    }

    pub(crate) fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    fn channels(self) -> String {
        format!("{} {} {}", self.0, self.1, self.2)
    }

    fn commas(self) -> String {
        format!("{}, {}, {}", self.0, self.1, self.2)
    }

    /// Hue in degrees, saturation and lightness in `0..=1`.
    fn hsl(self) -> (f64, f64, f64) {
        let [r, g, b] = [self.0, self.1, self.2].map(|c| f64::from(c) / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let d = max - min;
        if d == 0.0 {
            return (0.0, 0.0, l);
        }
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            60.0 * ((g - b) / d).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        (h, s, l)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from_hsl(h: f64, s: f64, l: f64) -> Self {
        let s = s.clamp(0.0, 1.0);
        let l = l.clamp(0.0, 1.0);
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let h = h.rem_euclid(360.0) / 60.0;
        let x = c * (1.0 - (h % 2.0 - 1.0).abs());
        let (r, g, b) = match h as u8 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = l - c / 2.0;
        let to = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        Self(to(r), to(g), to(b))
    }
}

/// `--ac-*` declarations for an accent colour. Greys stay grey; a colour
/// keeps its hue and saturation across the ramp.
pub(crate) fn accent_ramp(accent: Rgb) -> String {
    let (h, s, _) = accent.hsl();
    RAMP.iter()
        .map(|(step, l)| format!("--ac-{step}: {};", Rgb::from_hsl(h, s, *l).channels()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Backdrop variables for the custom theme, drawn from one colour the way
/// the brand palette is drawn from emerald.
pub(crate) fn custom_backdrop(base: Rgb) -> String {
    let (h, s, _) = base.hsl();
    let at = |dh: f64, ds: f64, l: f64| Rgb::from_hsl(h + dh, s * ds, l);
    let start = at(0.0, 0.8, 0.07);
    format!(
        "--gradient-background-start: rgb({}) !important; \
         --gradient-background-end: rgb(0, 0, 0) !important; \
         --first-color: {} !important; --second-color: {} !important; \
         --third-color: {} !important; --fourth-color: {} !important; \
         --fifth-color: {} !important;",
        start.commas(),
        at(0.0, 1.0, 0.52).commas(),
        at(0.0, 0.3, 0.92).commas(),
        at(0.0, 1.0, 0.40).commas(),
        at(14.0, 1.0, 0.50).commas(),
        at(-12.0, 0.8, 0.68).commas(),
    )
}

/// The colour the backdrop is drawn from: the custom theme's own colour,
/// else the accent. `None` keeps the brand gradient (and AMOLED has none).
fn backdrop_base(prefs: &Preferences) -> Option<Rgb> {
    let hex = match prefs.theme {
        Theme::Amoled => return None,
        Theme::Custom => prefs
            .theme_accent
            .as_deref()
            .or(prefs.accent_color.as_deref()),
        Theme::Dark => prefs.accent_color.as_deref(),
    };
    hex.and_then(Rgb::parse)
}

/// The reduced-motion backdrop; `None` uses the brand's.
pub(crate) fn still_backdrop(prefs: &Preferences) -> Option<String> {
    let (h, s, _) = backdrop_base(prefs)?.hsl();
    let start = Rgb::from_hsl(h, s * 0.8, 0.07);
    Some(format!(
        "background: linear-gradient(40deg, rgb({}), rgb(0, 0, 0));",
        start.commas()
    ))
}

/// The stylesheet for a set of preferences.
pub(crate) fn stylesheet(prefs: &Preferences) -> String {
    let mut css = String::new();
    if let Some(accent) = prefs.accent_color.as_deref().and_then(Rgb::parse) {
        css.push_str(&format!(":root {{ {} }}\n", accent_ramp(accent)));
    }
    if let Some(base) = backdrop_base(prefs) {
        css.push_str(&format!(
            ".{BACKDROP_CLASS} {{ {} }}\n",
            custom_backdrop(base)
        ));
    }
    css
}

/// Keeps the theme stylesheet in step with the preferences.
pub(crate) fn provide(prefs: RwSignal<Preferences>) {
    Effect::new(move |_| {
        let css = prefs.with(stylesheet);
        let theme = prefs.with(|p| p.theme);
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let style = document.get_element_by_id(STYLE_ID).or_else(|| {
            let el = document.create_element("style").ok()?;
            el.set_id(STYLE_ID);
            document.body()?.append_child(&el).ok()?;
            Some(el)
        });
        if let Some(style) = style {
            style.set_text_content(Some(&css));
        }
        if let Some(root) = document.document_element() {
            let name = match theme {
                Theme::Dark => "dark",
                Theme::Amoled => "amoled",
                Theme::Custom => "custom",
            };
            let _ = root.set_attribute("data-theme", name);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex() {
        assert_eq!(Rgb::parse("#34d399"), Some(Rgb(0x34, 0xd3, 0x99)));
        assert_eq!(Rgb::parse("34D399"), Some(Rgb(0x34, 0xd3, 0x99)));
        assert_eq!(Rgb::parse("#34d39"), None);
        assert_eq!(Rgb::parse("#zzzzzz"), None);
        assert_eq!(Rgb::parse("#é4d399"), None);
        assert_eq!(Rgb(0x34, 0xd3, 0x99).hex(), "#34d399");
    }

    #[test]
    fn hsl_round_trips() {
        for c in [
            Rgb(52, 211, 153),
            Rgb(124, 58, 237),
            Rgb(250, 10, 10),
            Rgb(128, 128, 128),
        ] {
            let (h, s, l) = c.hsl();
            let back = Rgb::from_hsl(h, s, l);
            let near = |a: u8, b: u8| a.abs_diff(b) <= 1;
            assert!(
                near(c.0, back.0) && near(c.1, back.1) && near(c.2, back.2),
                "{c:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn ramp_keeps_hue_and_darkens_down_the_steps() {
        let ramp = accent_ramp(Rgb(124, 58, 237));
        assert!(ramp.starts_with("--ac-200:"));
        assert_eq!(ramp.matches("--ac-").count(), 7);
        let violet_400 = Rgb::from_hsl(Rgb(124, 58, 237).hsl().0, Rgb(124, 58, 237).hsl().1, 0.52);
        assert!(ramp.contains(&format!("--ac-400: {};", violet_400.channels())));
        assert!(violet_400.2 > violet_400.1, "still violet");
    }

    #[test]
    fn stylesheet_follows_the_preferences() {
        let mut prefs = Preferences::default();
        assert!(
            stylesheet(&prefs).is_empty(),
            "brand defaults need no overrides"
        );

        prefs.accent_color = Some("#7c3aed".into());
        let css = stylesheet(&prefs);
        assert!(css.contains(":root"));
        assert!(
            css.contains(BACKDROP_CLASS),
            "the dark gradient follows the accent"
        );

        prefs.theme = Theme::Custom;
        prefs.theme_accent = Some("#ff0000".into());
        let red = custom_backdrop(Rgb(255, 0, 0));
        assert!(
            stylesheet(&prefs).contains(&red),
            "custom uses its own colour"
        );

        prefs.theme = Theme::Amoled;
        assert!(
            !stylesheet(&prefs).contains(BACKDROP_CLASS),
            "AMOLED stays black"
        );
        assert_eq!(still_backdrop(&prefs), None);

        prefs.theme = Theme::Dark;
        prefs.accent_color = Some("not a colour".into());
        assert!(stylesheet(&prefs).is_empty());
    }
}
