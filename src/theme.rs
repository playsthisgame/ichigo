//! The colors the TUI draws with, and the file that overrides them.
//!
//! A top-level module beside `media.rs` for the same reason that one is: it is
//! pure — a palette, a parser, and one transform — with no IO and no knowledge
//! of what is being drawn. `config.rs` reads [`ThemeFile`] out of
//! `config.toml`; `tui::render` reads the resolved [`Theme`].
//!
//! **Roles, not colors.** Nothing in `render.rs` names a color any more; it
//! names what the color is *for*. That indirection is the whole feature, and
//! it is also what makes the palette reviewable: `Color::Green` used to mean
//! both "a 2xx response" and "a JSON string", so recoloring one recolored the
//! other, and no amount of config could separate them.
//!
//! **The dark theme is mostly named colors on purpose.** `Color::Yellow` is
//! whatever the reader's terminal says yellow is, so ichigo looks like it
//! belongs in gruvbox, nord, or solarized without shipping a palette for each.
//! The light theme cannot do that — see [`Theme::LIGHT`] — and neither can the
//! two bands, which are the one place a named color caused a real bug.

use anyhow::{Result, bail};
use ratatui::style::Color;
use serde::Deserialize;

/// The palette, resolved and validated.
///
/// `Copy` because it lives in `UserConfig`, which is, and because the renderer
/// reads it through a `&'static` and never mutates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    // ─── UI ───────────────────────────────────────────────────────────────
    /// Primary text: field values, list rows, anything being read rather than
    /// labelled.
    pub text: Color,
    /// Everything secondary — labels, hints, disabled rows, borders at rest.
    /// The most-used role by a wide margin, so it is the one to get right.
    pub dim: Color,
    /// The focused thing: a focused field's label, a key in the hint line.
    pub accent: Color,
    /// Pane borders at rest.
    pub border: Color,
    /// The border of the pane that has focus.
    pub border_focus: Color,
    /// Text drawn *on* a colored badge, so it has to contrast with every
    /// status color rather than with the page.
    pub badge_text: Color,
    /// The band under the cursor line — a background, not ink.
    pub cursor_line: Color,
    /// The band under a visual selection, a step louder than `cursor_line`.
    pub selection: Color,
    /// `{{VAR}}` placeholders, and the profiles that fill them in.
    pub variable: Color,
    /// Folders in the request tree, borders and headings that belong to them —
    /// always *ink*, never a background. See `row_selected` for the other half.
    pub folder: Color,
    /// The background of the selected row in any list: the request list, the
    /// profile picker, the profile list, and the `VISUAL` badge.
    ///
    /// Its own role rather than `folder` reused, which is what it was until a
    /// light theme proved they are different jobs. On a dark theme one blue can
    /// serve as ink on the page *and* as a field behind text, because the same
    /// value is dark against a dark page either way. On a light theme it cannot:
    /// ink there has to be dark to read on the page, and a dark field then
    /// needs light ink on it — which is the pair below rather than `text`.
    pub row_selected: Color,
    /// Ink on `row_selected`. Contrasts with that color, not with the page —
    /// the distinction `text` cannot make.
    pub row_selected_text: Color,

    // ─── Status ───────────────────────────────────────────────────────────
    /// 2xx, and `POST`.
    pub success: Color,
    /// 3xx, and `GET`.
    pub info: Color,
    /// 4xx, and `PUT`/`PATCH`.
    pub warning: Color,
    /// 5xx, `DELETE`, and anything that failed.
    pub error: Color,

    // ─── JSON ─────────────────────────────────────────────────────────────
    // Separate from the status roles even where they start out the same
    // color. Sharing them is what made a 2xx and a JSON string impossible to
    // tell apart in config, which is the thing this module exists to fix.
    pub json_key: Color,
    pub json_string: Color,
    pub json_number: Color,
    pub json_bool: Color,
    pub json_null: Color,
    /// Braces, colons, commas. Its own role because it is the one that has to
    /// stay visible on a highlighted line — see `render::emphasize`.
    pub json_punct: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::DARK
    }
}

impl Theme {
    /// What ichigo has always looked like.
    ///
    /// Named colors nearly throughout, so the palette is the reader's: their
    /// terminal decides what `Yellow` is, and ichigo sits inside gruvbox or
    /// nord without knowing either exists.
    ///
    /// The two bands are the exception, and the exception is load-bearing. A
    /// named background can collide with a named foreground — the cursor line
    /// was once `DarkGray`, which is what `json_punct` is, so every brace and
    /// comma on the highlighted line vanished into it. They come from the
    /// grayscale ramp (232–255), the half of the 256 palette nothing remaps:
    /// `base16-shell` and friends rewrite 16–21 for their extra shades, and a
    /// band living there could become an arbitrary color. The ramp also needs
    /// no truecolor, so it survives a tmux without `Tc`.
    pub const DARK: Self = Self {
        text: Color::White,
        dim: Color::DarkGray,
        accent: Color::Yellow,
        border: Color::DarkGray,
        border_focus: Color::Cyan,
        badge_text: Color::Black,
        cursor_line: Color::Indexed(237), // #3a3a3a
        selection: Color::Indexed(240),   // #585858
        variable: Color::Magenta,
        folder: Color::Blue,
        row_selected: Color::Blue,
        row_selected_text: Color::White,

        success: Color::Green,
        info: Color::Cyan,
        warning: Color::Yellow,
        error: Color::Red,

        json_key: Color::Yellow,
        json_string: Color::Green,
        json_number: Color::Cyan,
        json_bool: Color::Magenta,
        json_null: Color::Red,
        json_punct: Color::DarkGray,
    };

    /// For a light terminal background.
    ///
    /// This one **cannot** use named colors, and that is the reason it exists
    /// as a built-in rather than as advice. On a light scheme the ANSI "bright"
    /// variants are usually *darker* than their base, so the emphasis transform
    /// that makes a highlighted line readable on `DARK` pushes the wrong way —
    /// it is not a bad color choice but a wrong assumption, and only real
    /// values fix it. Every color here is `Rgb`, which `emphasize` can reason
    /// about: it moves ink away from the band's own lightness rather than
    /// guessing which direction is brighter.
    pub const LIGHT: Self = Self {
        text: Color::Rgb(0x1c, 0x1c, 0x1c),
        dim: Color::Rgb(0x6c, 0x6c, 0x6c),
        accent: Color::Rgb(0xaf, 0x5f, 0x00),
        border: Color::Rgb(0x9e, 0x9e, 0x9e),
        border_focus: Color::Rgb(0x00, 0x5f, 0x87),
        badge_text: Color::Rgb(0xff, 0xff, 0xff),
        cursor_line: Color::Rgb(0xda, 0xda, 0xda),
        selection: Color::Rgb(0xbc, 0xbc, 0xbc),
        variable: Color::Rgb(0x87, 0x00, 0xaf),
        folder: Color::Rgb(0x00, 0x5f, 0xaf),
        row_selected: Color::Rgb(0x00, 0x5f, 0xaf),
        row_selected_text: Color::Rgb(0xff, 0xff, 0xff),

        success: Color::Rgb(0x00, 0x5f, 0x00),
        info: Color::Rgb(0x00, 0x5f, 0x87),
        warning: Color::Rgb(0xaf, 0x5f, 0x00),
        error: Color::Rgb(0xaf, 0x00, 0x00),

        json_key: Color::Rgb(0xaf, 0x5f, 0x00),
        json_string: Color::Rgb(0x00, 0x5f, 0x00),
        json_number: Color::Rgb(0x00, 0x5f, 0x87),
        json_bool: Color::Rgb(0x87, 0x00, 0xaf),
        json_null: Color::Rgb(0xaf, 0x00, 0x00),
        json_punct: Color::Rgb(0x76, 0x76, 0x76),
    };

    /// The built-ins, by the name `theme.name` takes.
    pub const BUILTIN: &'static [(&'static str, Self)] =
        &[("dark", Self::DARK), ("light", Self::LIGHT)];

    fn by_name(name: &str) -> Option<Self> {
        Self::BUILTIN
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, theme)| *theme)
    }

    /// Resolves `[theme]` into a palette: a built-in, then the overrides on top.
    ///
    /// Overrides are applied to a *named* base rather than to the default, so
    /// `name = "light"` plus one changed accent is one changed accent and not
    /// nineteen surprises.
    pub fn from_file(file: ThemeFile) -> Result<Self> {
        let mut theme = match file.name.as_deref() {
            None => Self::DARK,
            Some(name) => Self::by_name(name).ok_or_else(|| {
                let known: Vec<&str> = Self::BUILTIN.iter().map(|(n, _)| *n).collect();
                anyhow::anyhow!(
                    "theme.name must be one of {} (got {name:?}); \
                     any role can still be overridden under [theme.colors]",
                    known.join(", ")
                )
            })?,
        };
        file.colors.apply(&mut theme)?;
        Ok(theme)
    }
}

/// The `[theme]` table exactly as written.
///
/// Every field optional so adding a role never invalidates an existing file,
/// and `deny_unknown_fields` so a misspelled role is refused rather than
/// silently ignored — a color that does nothing forever is the exact failure
/// this crate's config already refuses everywhere else.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeFile {
    pub name: Option<String>,
    #[serde(default)]
    pub colors: ColorsFile,
}

/// `[theme.colors]`, as written: role name to color spec.
///
/// A struct and not a map, so a typo is a load error naming the field rather
/// than a key nobody reads.
macro_rules! colors_file {
    ($($role:ident),* $(,)?) => {
        #[derive(Debug, Default, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct ColorsFile {
            $(pub $role: Option<String>,)*
        }

        impl ColorsFile {
            /// Parses each override and writes it over the base palette.
            ///
            /// Failures name the role, because "invalid color" in a file with
            /// twenty of them is a hunt rather than a fix.
            fn apply(self, theme: &mut Theme) -> Result<()> {
                $(
                    if let Some(spec) = self.$role {
                        theme.$role = parse_color(&spec).map_err(|e| {
                            anyhow::anyhow!("theme.colors.{}: {e}", stringify!($role))
                        })?;
                    }
                )*
                Ok(())
            }
        }
    };
}

colors_file!(
    text,
    dim,
    accent,
    border,
    border_focus,
    badge_text,
    cursor_line,
    selection,
    variable,
    folder,
    row_selected,
    row_selected_text,
    success,
    info,
    warning,
    error,
    json_key,
    json_string,
    json_number,
    json_bool,
    json_null,
    json_punct,
);

/// Reads one color spec.
///
/// Three forms, in the order someone reaches for them:
/// - `"#rrggbb"` — an exact color, which no terminal theme can remap.
/// - `"yellow"`, `"bright yellow"` — an ANSI name, which *follows* the reader's
///   terminal theme. This is the one to use if you want ichigo to keep matching
///   your scheme when you change it.
/// - `"0"`–`"255"` — a palette index, for the grayscale ramp and the color cube.
///
/// Anything else is refused by name rather than defaulted, which is the same
/// bet `deny_unknown_fields` makes: a config that quietly means something other
/// than what it says costs more than an error does.
pub fn parse_color(spec: &str) -> Result<Color> {
    let s = spec.trim();

    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("{spec:?} is not a #rrggbb color");
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
        return Ok(Color::Rgb(byte(0), byte(2), byte(4)));
    }

    if let Ok(index) = s.parse::<u8>() {
        return Ok(Color::Indexed(index));
    }
    // A number that does not fit a `u8` is someone meaning an index, not a
    // color name, so it gets the index error rather than "unknown color".
    if s.chars().all(|c| c.is_ascii_digit()) {
        bail!("{spec:?} is out of range; palette indices are 0-255");
    }

    let normalized = s.to_ascii_lowercase().replace([' ', '-', '_'], "");
    let named = match normalized.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" | "brightblack" => Color::DarkGray,
        "brightred" | "lightred" => Color::LightRed,
        "brightgreen" | "lightgreen" => Color::LightGreen,
        "brightyellow" | "lightyellow" => Color::LightYellow,
        "brightblue" | "lightblue" => Color::LightBlue,
        "brightmagenta" | "lightmagenta" => Color::LightMagenta,
        "brightcyan" | "lightcyan" => Color::LightCyan,
        "white" | "brightwhite" => Color::White,
        _ => bail!(
            "{spec:?} is not a color; use #rrggbb, an ANSI name like \"yellow\" \
             or \"bright yellow\", or a palette index 0-255"
        ),
    };
    Ok(named)
}

/// The emphasized twin of a color, for ink about to be drawn on `band`.
///
/// A highlighted line keeps its meaning — a string still reads as a string —
/// but every color has to step away from the band or the line is worse to read
/// than the ones around it. The direction of that step is the whole problem:
/// "brighter" is only right on a dark theme.
///
/// So it is answered per representation, by what can actually be known:
/// - `Rgb` is computed. The band's own lightness says which way is *away*, so
///   this is correct on light and dark themes alike, and is why [`Theme::LIGHT`]
///   is written in `Rgb` throughout.
/// - A named color becomes its ANSI bright twin. Nothing here knows what the
///   terminal will draw for either, so this is a convention rather than a
///   calculation — it is right on the dark themes named colors are for.
/// - `Indexed(0..=7)` is the same convention: the bright half is `+8`.
/// - Anything already bright, or a cube/ramp index chosen deliberately, is left
///   alone. Guessing at a value someone picked exactly is not an improvement.
pub fn emphasize(color: Color, band: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            let toward = if is_light(band) { 0.0 } else { 255.0 };
            let mix = |c: u8| (c as f32 + (toward - c as f32) * EMPHASIS) as u8;
            Color::Rgb(mix(r), mix(g), mix(b))
        }
        Color::Indexed(i @ 0..=7) => Color::Indexed(i + 8),

        Color::Black => Color::DarkGray,
        Color::Red => Color::LightRed,
        Color::Green => Color::LightGreen,
        Color::Yellow => Color::LightYellow,
        Color::Blue => Color::LightBlue,
        Color::Magenta => Color::LightMagenta,
        Color::Cyan => Color::LightCyan,
        // Not `Gray`, which is dim enough to lose against a band. `DarkGray` is
        // what `json_punct` is on the dark theme, and punctuation is the part
        // of a JSON line most needed to read its structure.
        Color::DarkGray | Color::Gray => Color::White,

        other => other,
    }
}

/// How far `emphasize` moves an `Rgb` color toward the far end from its band.
/// Enough to separate ink from page, short of washing every color into one.
const EMPHASIS: f32 = 0.45;

/// Whether a color is light enough to want dark ink on it.
///
/// Relative luminance, thresholded where the two contrast ratios against black
/// and white cross. Only `Rgb` can be answered: a named color or a palette
/// index is whatever the terminal draws, and this module does not get to know.
/// Those are assumed dark, which is what a terminal running a TUI usually is
/// and what the named-color path above already assumes.
pub fn is_light(color: Color) -> bool {
    let Color::Rgb(r, g, b) = color else {
        return matches!(color, Color::White | Color::Gray);
    };
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b) > 0.179
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: Option<&str>) -> ThemeFile {
        ThemeFile { name: name.map(str::to_string), colors: ColorsFile::default() }
    }

    #[test]
    fn no_theme_table_is_the_dark_theme() {
        assert_eq!(Theme::from_file(ThemeFile::default()).unwrap(), Theme::DARK);
    }

    #[test]
    fn a_built_in_can_be_chosen_by_name_case_insensitively() {
        assert_eq!(Theme::from_file(file(Some("light"))).unwrap(), Theme::LIGHT);
        assert_eq!(Theme::from_file(file(Some("DARK"))).unwrap(), Theme::DARK);
    }

    /// Refused by name, not defaulted — the same bet `deny_unknown_fields`
    /// makes. A theme that silently is not the one you asked for is worse than
    /// a startup error.
    #[test]
    fn an_unknown_theme_name_is_refused_and_lists_the_real_ones() {
        let e = Theme::from_file(file(Some("gruvbox"))).unwrap_err().to_string();
        assert!(e.contains("gruvbox"), "{e}");
        assert!(e.contains("dark") && e.contains("light"), "{e}");
    }

    /// Overrides land on the *named* base, so picking a theme and changing one
    /// role changes one role.
    #[test]
    fn overrides_apply_on_top_of_the_named_theme() {
        let mut f = file(Some("light"));
        f.colors.accent = Some("#ff0000".to_string());
        let theme = Theme::from_file(f).unwrap();
        assert_eq!(theme.accent, Color::Rgb(0xff, 0, 0));
        // Everything else is still the light theme.
        assert_eq!(theme.text, Theme::LIGHT.text);
        assert_eq!(theme.json_string, Theme::LIGHT.json_string);
    }

    #[test]
    fn a_bad_override_names_the_role_it_came_from() {
        let mut f = file(None);
        f.colors.json_null = Some("puce".to_string());
        let e = Theme::from_file(f).unwrap_err().to_string();
        assert!(e.contains("theme.colors.json_null"), "{e}");
    }

    #[test]
    fn colors_parse_in_all_three_forms() {
        assert_eq!(parse_color("#fabd2f").unwrap(), Color::Rgb(0xfa, 0xbd, 0x2f));
        assert_eq!(parse_color("yellow").unwrap(), Color::Yellow);
        assert_eq!(parse_color("bright yellow").unwrap(), Color::LightYellow);
        assert_eq!(parse_color("BrightYellow").unwrap(), Color::LightYellow);
        assert_eq!(parse_color("237").unwrap(), Color::Indexed(237));
    }

    #[test]
    fn bad_color_specs_are_refused() {
        for spec in ["#fff", "#gggggg", "puce", "256", "1000", ""] {
            assert!(parse_color(spec).is_err(), "{spec:?} should be refused");
        }
    }

    /// The bug the whole highlight change exists for: a foreground must never
    /// end up equal to the band behind it.
    #[test]
    fn no_emphasized_role_collides_with_its_band() {
        for (name, theme) in Theme::BUILTIN {
            for band in [theme.cursor_line, theme.selection] {
                for role in [
                    theme.text, theme.dim, theme.accent, theme.json_key,
                    theme.json_string, theme.json_number, theme.json_bool,
                    theme.json_null, theme.json_punct,
                ] {
                    assert_ne!(emphasize(role, band), band, "{name} theme");
                }
            }
        }
    }

    /// The light theme's whole reason for existing: emphasis has to move ink
    /// *away* from the band, which is darker on a light theme and lighter on a
    /// dark one. Getting this backwards is what made a light scheme unreadable.
    #[test]
    fn emphasis_moves_away_from_the_band_in_both_directions() {
        let ink = Color::Rgb(0x80, 0x80, 0x80);

        let on_light = emphasize(ink, Theme::LIGHT.selection);
        let on_dark = emphasize(ink, Color::Rgb(0x20, 0x20, 0x20));
        let channel = |c: Color| match c {
            Color::Rgb(r, _, _) => r,
            _ => unreachable!(),
        };
        assert!(channel(on_light) < 0x80, "light band should darken its ink");
        assert!(channel(on_dark) > 0x80, "dark band should lighten its ink");
    }

    #[test]
    fn named_colors_emphasize_to_their_bright_twin() {
        assert_eq!(emphasize(Color::Green, Theme::DARK.selection), Color::LightGreen);
        assert_eq!(emphasize(Color::Indexed(2), Theme::DARK.selection), Color::Indexed(10));
        // Punctuation is rescued to White rather than the dim Gray.
        assert_eq!(emphasize(Color::DarkGray, Theme::DARK.selection), Color::White);
    }

    /// A value someone picked exactly is not improved by guessing at it.
    #[test]
    fn deliberate_palette_indices_are_left_alone() {
        assert_eq!(emphasize(Color::Indexed(237), Theme::DARK.selection), Color::Indexed(237));
        assert_eq!(emphasize(Color::LightCyan, Theme::DARK.selection), Color::LightCyan);
    }

    /// The two bands are a step apart in weight, not two arbitrary values: the
    /// selection is the louder, which is what makes entering visual mode read
    /// as a change rather than as a color swap. True in both directions —
    /// louder means lighter on a dark theme and darker on a light one.
    #[test]
    fn every_theme_makes_its_selection_louder_than_its_cursor_line() {
        for (name, theme) in Theme::BUILTIN {
            assert_ne!(theme.cursor_line, theme.selection, "{name} theme");
            let step = |c: Color| match c {
                Color::Indexed(i) => i as i32,
                Color::Rgb(r, _, _) => r as i32,
                _ => panic!("{name}: a band must be an exact color, not a named one"),
            };
            let away_from_page = step(theme.selection) - step(theme.cursor_line);
            if is_light(theme.selection) {
                assert!(away_from_page < 0, "{name}: light theme selection should be darker");
            } else {
                assert!(away_from_page > 0, "{name}: dark theme selection should be lighter");
            }
        }
    }

    #[test]
    fn lightness_is_only_claimed_where_it_can_be_known() {
        assert!(is_light(Color::Rgb(0xff, 0xff, 0xff)));
        assert!(!is_light(Color::Rgb(0x1c, 0x1c, 0x1c)));
        assert!(is_light(Theme::LIGHT.selection));
        assert!(!is_light(Theme::DARK.selection));
        // Unknowable: a named color is whatever the terminal draws.
        assert!(!is_light(Color::Yellow));
    }
}
