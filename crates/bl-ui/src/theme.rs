//! Colours and metrics of the user interface.

use bl_render::Color;

fn hsl(h: f32, s: f32, l: f32, a: f32) -> Color {
    let h = (h % 360.0) / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    Color(f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0), a)
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    /// Translucent panel background drawn over the visualizer.
    pub panel: Color,
    pub panel_strong: Color,
    pub surface: Color,
    pub surface_hover: Color,
    pub surface_active: Color,
    pub border: Color,
    pub primary: Color,
    pub primary_hover: Color,
    pub primary_text: Color,
    pub danger: Color,
    pub warning: Color,
    pub success: Color,
    pub shadow: Color,
    pub backdrop: Color,
    pub radius: f32,
    pub radius_small: f32,
    pub font_small: f32,
    pub font: f32,
    pub font_large: f32,
    pub font_title: f32,
    pub row_height: f32,
    pub control_height: f32,
    pub spacing: f32,
}

impl Theme {
    /// Dark theme with a pink/red accent (hue 347).
    pub fn dark(accent_hue: f32) -> Theme {
        Theme {
            text: Color(0.97, 0.97, 0.98, 1.0),
            text_dim: Color(0.97, 0.97, 0.98, 0.68),
            text_faint: Color(0.97, 0.97, 0.98, 0.42),
            panel: hsl(261.0, 0.07, 0.08, 0.72),
            panel_strong: hsl(261.0, 0.07, 0.09, 0.92),
            surface: Color(1.0, 1.0, 1.0, 0.06),
            surface_hover: Color(1.0, 1.0, 1.0, 0.11),
            surface_active: Color(1.0, 1.0, 1.0, 0.17),
            border: Color(1.0, 1.0, 1.0, 0.10),
            primary: hsl(accent_hue, 0.80, 0.56, 1.0),
            primary_hover: hsl(accent_hue, 0.85, 0.63, 1.0),
            primary_text: Color(1.0, 1.0, 1.0, 1.0),
            danger: hsl(0.0, 0.70, 0.56, 1.0),
            warning: hsl(42.0, 0.95, 0.55, 1.0),
            success: hsl(150.0, 0.60, 0.48, 1.0),
            shadow: Color(0.0, 0.0, 0.0, 0.45),
            backdrop: Color(0.0, 0.0, 0.0, 0.45),
            radius: 10.0,
            radius_small: 6.0,
            font_small: 12.0,
            font: 14.0,
            font_large: 17.0,
            font_title: 24.0,
            row_height: 44.0,
            control_height: 34.0,
            spacing: 10.0,
        }
    }

    pub fn accent_presets() -> &'static [(&'static str, f32)] {
        &[("Rose", 347.0), ("Violet", 268.0), ("Blue", 214.0), ("Teal", 172.0), ("Green", 140.0), ("Amber", 36.0)]
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark(347.0)
    }
}
