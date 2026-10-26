//! The look of the settings window and the layout editor: fonts (Inter +
//! Phosphor icons), a light and a dark palette in the style of macOS System
//! Settings / PowerToys, and the egui `Style` built from them. Both themes are
//! installed; egui picks the one matching the OS appearance.

use std::sync::Arc;

use eframe::egui::{
    self, epaint::Shadow, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    Margin, Stroke, TextStyle, Theme, Vec2,
};

/// Inter (variable weight), SIL Open Font License 1.1 (`assets/Inter-LICENSE.txt`).
static INTER: &[u8] = include_bytes!("../assets/Inter.ttf");
/// Phosphor icons (regular), MIT (`assets/Phosphor-LICENSE.txt`).
static PHOSPHOR: &[u8] = include_bytes!("../assets/Phosphor.ttf");

/// Font family for semibold text drawn with the painter.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

fn icon_family() -> FontFamily {
    FontFamily::Name("icons".into())
}

/// The font for [`icon`] glyphs.
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, icon_family())
}

/// An icon as text (set its size / color as usual).
pub fn icon_text(glyph: &str) -> egui::RichText {
    egui::RichText::new(glyph).family(icon_family())
}

/// `glyph` followed by `text` (for buttons and labels).
pub fn icon_job(glyph: &str, text: &str, size: f32, color: Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        glyph,
        0.0,
        egui::TextFormat::simple(icon_font(size + 1.0), color),
    );
    job.append(text, 6.0, egui::TextFormat::simple(font(size), color));
    job
}

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn font_semibold(size: f32) -> FontId {
    FontId::new(size, semibold())
}

/// Colors of one theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Content background.
    pub bg: Color32,
    pub sidebar: Color32,
    pub card: Color32,
    pub card_border: Color32,
    pub separator: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,
    pub control_border: Color32,
    pub input: Color32,
    pub error: Color32,
    pub warn: Color32,
    pub ok: Color32,
    pub off: Color32,
}

pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(0x00, 0x7A, 0xFF);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(0x0A, 0x84, 0xFF);

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: Color32::from_rgb(0xF5, 0xF5, 0xF7),
    sidebar: Color32::from_rgb(0xEC, 0xEC, 0xF0),
    card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    card_border: Color32::from_rgb(0xE2, 0xE2, 0xE7),
    separator: Color32::from_rgb(0xEC, 0xEC, 0xF0),
    text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
    text2: Color32::from_rgb(0x6E, 0x6E, 0x73),
    text3: Color32::from_rgb(0xA1, 0xA1, 0xA6),
    accent: ACCENT_LIGHT,
    accent_soft: Color32::from_rgb(0xE3, 0xEF, 0xFF),
    on_accent: Color32::WHITE,
    control: Color32::from_rgb(0xF2, 0xF2, 0xF5),
    control_hover: Color32::from_rgb(0xE8, 0xE8, 0xED),
    control_active: Color32::from_rgb(0xDC, 0xDC, 0xE2),
    control_border: Color32::from_rgb(0xD2, 0xD2, 0xD8),
    input: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    error: Color32::from_rgb(0xD7, 0x26, 0x2B),
    warn: Color32::from_rgb(0xB8, 0x5C, 0x00),
    ok: Color32::from_rgb(0x24, 0x8A, 0x3D),
    off: Color32::from_rgb(0xC7, 0xC7, 0xCC),
};

pub const DARK: Palette = Palette {
    dark: true,
    bg: Color32::from_rgb(0x1C, 0x1C, 0x1E),
    sidebar: Color32::from_rgb(0x24, 0x24, 0x27),
    card: Color32::from_rgb(0x2A, 0x2A, 0x2D),
    card_border: Color32::from_rgb(0x38, 0x38, 0x3C),
    separator: Color32::from_rgb(0x36, 0x36, 0x3A),
    text: Color32::from_rgb(0xF2, 0xF2, 0xF7),
    text2: Color32::from_rgb(0x9C, 0x9C, 0xA2),
    text3: Color32::from_rgb(0x63, 0x63, 0x68),
    accent: ACCENT_DARK,
    accent_soft: Color32::from_rgb(0x1B, 0x33, 0x52),
    on_accent: Color32::WHITE,
    control: Color32::from_rgb(0x3A, 0x3A, 0x3D),
    control_hover: Color32::from_rgb(0x45, 0x45, 0x49),
    control_active: Color32::from_rgb(0x50, 0x50, 0x55),
    control_border: Color32::from_rgb(0x4A, 0x4A, 0x4F),
    input: Color32::from_rgb(0x1F, 0x1F, 0x22),
    error: Color32::from_rgb(0xFF, 0x5F, 0x57),
    warn: Color32::from_rgb(0xFF, 0xB0, 0x40),
    ok: Color32::from_rgb(0x32, 0xD1, 0x5D),
    off: Color32::from_rgb(0x5A, 0x5A, 0x5F),
};

/// The palette of the theme `ui` is drawn with.
pub fn pal(ui: &egui::Ui) -> Palette {
    if ui.visuals().dark_mode {
        DARK
    } else {
        LIGHT
    }
}

pub fn pal_ctx(ctx: &egui::Context) -> Palette {
    if ctx.global_style().visuals.dark_mode {
        DARK
    } else {
        LIGHT
    }
}

/// Install fonts and both themes once per context.
pub fn ensure(ctx: &egui::Context) {
    let id = egui::Id::unique("pm-theme-installed");
    if ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(id, true));
    install(ctx);
    // Fonts apply from the next pass: redo this one with them.
    ctx.request_discard("theme installed");
}

pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_style_of(Theme::Light, style(&LIGHT));
    ctx.set_style_of(Theme::Dark, style(&DARK));
}

fn fonts() -> FontDefinitions {
    let mut f = FontDefinitions::default();
    let tweak = |w: f32| egui::FontTweak {
        coords: egui::epaint::text::VariationCoords::new([(b"wght", w)]),
        ..Default::default()
    };
    f.font_data.insert(
        "Inter".into(),
        Arc::new(FontData::from_static(INTER).tweak(tweak(420.0))),
    );
    f.font_data.insert(
        "Inter-SemiBold".into(),
        Arc::new(FontData::from_static(INTER).tweak(tweak(620.0))),
    );
    f.font_data.insert(
        "Phosphor".into(),
        Arc::new(FontData::from_static(PHOSPHOR).tweak(egui::FontTweak {
            y_offset_factor: 0.08,
            ..Default::default()
        })),
    );
    // Icons have a family of their own: Inter has glyphs of its own in the
    // private use area Phosphor uses.
    f.families.insert(icon_family(), vec!["Phosphor".into()]);
    let prop = f.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "Inter".into());
    let fallback: Vec<String> = prop.clone();
    let mut semi = vec!["Inter-SemiBold".to_string()];
    semi.extend(fallback.iter().skip(1).cloned());
    f.families.insert(semibold(), semi);
    let mono = f.families.entry(FontFamily::Monospace).or_default();
    mono.push("Inter".into());
    f
}

pub fn style(p: &Palette) -> egui::Style {
    let mut s = egui::Style {
        text_styles: [
            (TextStyle::Small, font(11.5)),
            (TextStyle::Body, font(13.5)),
            (TextStyle::Button, font(13.5)),
            (TextStyle::Heading, font_semibold(22.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into(),
        ..Default::default()
    };
    let sp = &mut s.spacing;
    sp.item_spacing = Vec2::new(8.0, 8.0);
    sp.button_padding = Vec2::new(12.0, 5.0);
    sp.interact_size = Vec2::new(40.0, 28.0);
    sp.slider_width = 200.0;
    sp.slider_rail_height = 4.0;
    sp.combo_width = 200.0;
    sp.text_edit_width = 220.0;
    sp.window_margin = Margin::same(18);
    sp.menu_margin = Margin::same(6);
    sp.icon_width = 16.0;
    sp.icon_width_inner = 9.0;
    sp.scroll = egui::style::ScrollStyle::floating();
    sp.scroll.bar_width = 8.0;

    let mut v = if p.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.card_border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 34,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 120 } else { 46 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if p.dark { 100 } else { 32 }),
    };
    v.faint_bg_color = p.control;
    v.extreme_bg_color = p.input;
    v.text_edit_bg_color = Some(p.input);
    v.code_bg_color = p.control;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.error;
    v.override_text_color = None;
    v.weak_text_color = Some(p.text2);
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.5, p.on_accent);
    v.text_cursor.stroke = Stroke::new(2.0, p.accent);
    v.striped = false;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;
    let cr = CornerRadius::same(7);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = cr;
    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::new(1.0, p.control_border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = cr;
    w.inactive.expansion = 0.0;
    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.control_border);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = cr;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = p.control_active;
    w.active.weak_bg_fill = p.control_active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.active.corner_radius = cr;
    w.active.expansion = 0.0;
    w.open = w.hovered;
    s.visuals = v;
    s.animation_time = 0.14;
    s
}

/// Mix `a` towards `b` by `t` (0..=1), in sRGB.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

/// Phosphor icon code points (regular weight).
#[allow(dead_code)]
pub mod icon {
    pub const APP_WINDOW: &str = "\u{E5DA}";
    pub const ARROW_COUNTER_CLOCKWISE: &str = "\u{E038}";
    pub const ARROW_SQUARE_OUT: &str = "\u{E5DE}";
    pub const ARROWS_CLOCKWISE: &str = "\u{E094}";
    pub const ARROWS_DOWN_UP: &str = "\u{E098}";
    pub const ARROWS_LEFT_RIGHT: &str = "\u{E0A0}";
    pub const CARET_DOWN: &str = "\u{E136}";
    pub const CARET_LEFT: &str = "\u{E138}";
    pub const CARET_RIGHT: &str = "\u{E13A}";
    pub const CHECK: &str = "\u{E182}";
    pub const CHECK_CIRCLE: &str = "\u{E184}";
    pub const CLIPBOARD_TEXT: &str = "\u{E198}";
    pub const CLOCK: &str = "\u{E19A}";
    pub const COFFEE: &str = "\u{E1C2}";
    pub const COMMAND: &str = "\u{E1C4}";
    pub const COPY: &str = "\u{E1CA}";
    pub const CROSSHAIR: &str = "\u{E1D6}";
    pub const CURSOR: &str = "\u{E1DC}";
    pub const CURSOR_CLICK: &str = "\u{E7C8}";
    pub const DOTS_SIX_VERTICAL: &str = "\u{EAE2}";
    pub const EYE: &str = "\u{E220}";
    pub const EYE_SLASH: &str = "\u{E224}";
    pub const FOLDER_OPEN: &str = "\u{E256}";
    pub const GEAR_SIX: &str = "\u{E272}";
    pub const GRID_FOUR: &str = "\u{E296}";
    pub const HAND_GRABBING: &str = "\u{E57C}";
    pub const INFO: &str = "\u{E2CE}";
    pub const KEY: &str = "\u{E2D6}";
    pub const KEYBOARD: &str = "\u{E2D8}";
    pub const LAYOUT: &str = "\u{E6D6}";
    pub const LIGHTNING: &str = "\u{E2DE}";
    pub const LINK: &str = "\u{E2E2}";
    pub const LOCK_KEY: &str = "\u{E2FE}";
    pub const MAGIC_WAND: &str = "\u{E6B6}";
    pub const MAGNIFYING_GLASS: &str = "\u{E30C}";
    pub const MONITOR: &str = "\u{E32E}";
    pub const MOUSE: &str = "\u{E33A}";
    pub const MOUSE_SIMPLE: &str = "\u{E644}";
    pub const NETWORK: &str = "\u{EDDE}";
    pub const PALETTE: &str = "\u{E6C8}";
    pub const PENCIL_SIMPLE: &str = "\u{E3B4}";
    pub const PLAY: &str = "\u{E3D0}";
    pub const PLUGS_CONNECTED: &str = "\u{EB5A}";
    pub const PLUS: &str = "\u{E3D4}";
    pub const POWER: &str = "\u{E3DA}";
    pub const RECORD: &str = "\u{E3EE}";
    pub const ROCKET_LAUNCH: &str = "\u{E3FE}";
    pub const SHIELD_CHECK: &str = "\u{E40C}";
    pub const SLIDERS_HORIZONTAL: &str = "\u{E434}";
    pub const SPARKLE: &str = "\u{E6A2}";
    pub const SQUARES_FOUR: &str = "\u{E464}";
    pub const STACK: &str = "\u{E466}";
    pub const SWAP: &str = "\u{E83C}";
    pub const TEXT_T: &str = "\u{E48A}";
    pub const TIMER: &str = "\u{E492}";
    pub const TRASH: &str = "\u{E4A6}";
    pub const WARNING: &str = "\u{E4E0}";
    pub const WARNING_CIRCLE: &str = "\u{E4E2}";
    pub const X: &str = "\u{E4F6}";
}
