//! The Apteronotus visual language.
//!
//! One place decides colour, type and geometry; `lib.rs` only composes
//! widgets. See `DESIGN.md` for the reasoning behind each token.

use eframe::egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Margin, Response,
    RichText, Sense, Shadow, Stroke, Style, TextStyle, Ui, Vec2, Visuals, Widget as _,
};
use std::sync::Arc;

/// Heavy rectangular caps, used only for the wordmark and section headers.
pub const DISPLAY: fn() -> FontFamily = || FontFamily::Name("display".into());

/// Roboto Mono, which is every other glyph in the application.
pub const MONO: FontFamily = FontFamily::Monospace;

// ---------------------------------------------------------------------------
// Palette
//
// Deep water, one electric discharge. Everything that is not the accent is a
// desaturated blue-grey, so the accent never competes with anything.
// ---------------------------------------------------------------------------

/// The editor well — the deepest surface, and the only true black-ish one.
pub const VOID: Color32 = Color32::from_rgb(0x04, 0x07, 0x0a);
/// The window body behind panels.
pub const DEEP: Color32 = Color32::from_rgb(0x08, 0x0c, 0x11);
/// Chrome: toolbar, side panels, diagnostics.
pub const PANEL: Color32 = Color32::from_rgb(0x0d, 0x13, 0x1a);
/// A widget at rest.
pub const RAISED: Color32 = Color32::from_rgb(0x14, 0x1d, 0x26);
/// A widget under the pointer.
pub const RAISED_HOT: Color32 = Color32::from_rgb(0x1b, 0x27, 0x33);
/// Hairlines, separators, widget outlines.
pub const LINE: Color32 = Color32::from_rgb(0x22, 0x30, 0x3c);

/// Labels that are structure rather than content.
pub const MUTED: Color32 = Color32::from_rgb(0x59, 0x6c, 0x7a);
/// Line numbers: present, countable, and never competing with the code.
pub const GUTTER: Color32 = Color32::from_rgb(0x35, 0x45, 0x51);
/// Ordinary text.
pub const TEXT: Color32 = Color32::from_rgb(0xbe, 0xcf, 0xda);
/// Emphasised text.
pub const BRIGHT: Color32 = Color32::from_rgb(0xe8, 0xf4, 0xfb);

/// The discharge: the single accent, used for anything live or actionable.
pub const DISCHARGE: Color32 = Color32::from_rgb(0x00, 0xc8, 0xff);
/// The accent under pressure.
pub const DISCHARGE_HOT: Color32 = Color32::from_rgb(0x7d, 0xe8, 0xff);
/// The accent at rest, for rails and idle indicators.
pub const DISCHARGE_DIM: Color32 = Color32::from_rgb(0x00, 0x6d, 0x94);
/// The accent as a wash behind selected text.
pub const DISCHARGE_WASH: Color32 = Color32::from_rgba_premultiplied(0x00, 0x38, 0x4c, 0x60);

/// Work in flight.
pub const CAUTION: Color32 = Color32::from_rgb(0xff, 0xb0, 0x3a);
/// A refused edit.
pub const ALERT: Color32 = Color32::from_rgb(0xff, 0x51, 0x48);

// Code colours. The document is mostly `TEXT`; the two things that carry
// musical meaning — the sandbox vocabulary and mini-notation strings — are the
// two things that are allowed to be loud.

/// Lua's reserved words.
pub const CODE_KEYWORD: Color32 = Color32::from_rgb(0x3f, 0x9e, 0xc4);
/// The Apteronotus vocabulary.
pub const CODE_BUILTIN: Color32 = DISCHARGE;
/// Mini-notation and every other string: the one warm colour in the palette.
pub const CODE_STRING: Color32 = Color32::from_rgb(0xe6, 0xa5, 0x54);
/// Literal numbers.
pub const CODE_NUMBER: Color32 = Color32::from_rgb(0x8f, 0xdc, 0xf0);
/// Comments.
pub const CODE_COMMENT: Color32 = Color32::from_rgb(0x46, 0x59, 0x66);
/// Operators and delimiters.
pub const CODE_PUNCT: Color32 = Color32::from_rgb(0x7b, 0x8e, 0x9b);

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Nothing in this application has a rounded corner.
const SQUARE: CornerRadius = CornerRadius::ZERO;
/// The vertical rhythm; every margin is a multiple of it.
pub const UNIT: f32 = 6.0;

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

/// Install fonts and style. Call once, from the creation context.
pub fn install(ctx: &eframe::egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(eframe::egui::ThemePreference::Dark);
    ctx.all_styles_mut(apply);
}

fn fonts() -> FontDefinitions {
    // Start from the defaults so the emoji faces stay as a fallback for the
    // glyphs a text face does not carry.
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "roboto-mono".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/RobotoMono-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "roboto-mono-medium".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/RobotoMono-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        "display".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/BigShouldersDisplay-Black.ttf"
        ))),
    );

    for family in [FontFamily::Monospace, FontFamily::Proportional] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "roboto-mono".into());
    }
    fonts.families.insert(
        DISPLAY(),
        vec![
            "display".into(),
            "roboto-mono-medium".into(),
            "roboto-mono".into(),
        ],
    );

    fonts
}

fn apply(style: &mut Style) {
    style.text_styles = [
        // The display face is very condensed, so it is set larger than the
        // text it sits beside rather than merely bolder.
        (TextStyle::Heading, FontId::new(22.0, DISPLAY())),
        (TextStyle::Body, FontId::new(13.0, MONO)),
        (TextStyle::Monospace, FontId::new(14.0, MONO)),
        (TextStyle::Button, FontId::new(13.0, MONO)),
        (TextStyle::Small, FontId::new(11.0, MONO)),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(UNIT, UNIT);
    spacing.button_padding = Vec2::new(UNIT * 1.5, UNIT * 0.75);
    spacing.window_margin = Margin::same(UNIT as i8);
    spacing.menu_margin = Margin::same(UNIT as i8);
    spacing.interact_size.y = 22.0;
    spacing.slider_width = 140.0;
    spacing.slider_rail_height = 4.0;
    spacing.scroll.bar_width = 8.0;
    spacing.scroll.floating = false;
    // A scrollbar is furniture, not text: take its colour from the widget fill
    // so it stays inside the palette instead of borrowing the text colour.
    spacing.scroll.foreground_color = false;
    spacing.scroll.dormant_background_opacity = 0.0;
    spacing.scroll.active_background_opacity = 0.0;
    spacing.scroll.interact_background_opacity = 0.0;

    style.visuals = visuals();
}

fn visuals() -> Visuals {
    let mut visuals = Visuals::dark();

    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = VOID;
    visuals.faint_bg_color = Color32::from_rgb(0x10, 0x17, 0x1e);
    visuals.code_bg_color = VOID;
    visuals.window_stroke = Stroke::new(1.0, LINE);
    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow::NONE;
    visuals.window_corner_radius = SQUARE;
    visuals.menu_corner_radius = SQUARE;

    visuals.hyperlink_color = DISCHARGE;
    visuals.warn_fg_color = CAUTION;
    visuals.error_fg_color = ALERT;
    visuals.weak_text_color = Some(MUTED);

    visuals.selection.bg_fill = DISCHARGE_WASH;
    visuals.selection.stroke = Stroke::new(1.0, BRIGHT);
    visuals.text_cursor.stroke = Stroke::new(2.0, DISCHARGE);
    visuals.text_cursor.blink = true;

    // Square handles on a thin rail: the slider reads as a fader, not a knob.
    visuals.handle_shape = eframe::egui::style::HandleShape::Rect { aspect_ratio: 0.3 };
    visuals.slider_trailing_fill = true;
    visuals.striped = false;
    visuals.button_frame = true;
    visuals.collapsing_header_frame = true;
    visuals.indent_has_left_vline = false;

    let widgets = &mut visuals.widgets;
    for state in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.corner_radius = SQUARE;
        state.expansion = 0.0;
    }

    widgets.noninteractive.bg_fill = PANEL;
    widgets.noninteractive.weak_bg_fill = PANEL;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);

    widgets.inactive.bg_fill = RAISED;
    widgets.inactive.weak_bg_fill = RAISED;
    widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);

    // Interaction is where the accent appears: a hovered control lights up at
    // its edge, a held one fills.
    widgets.hovered.bg_fill = RAISED_HOT;
    widgets.hovered.weak_bg_fill = RAISED_HOT;
    widgets.hovered.bg_stroke = Stroke::new(1.0, DISCHARGE);
    widgets.hovered.fg_stroke = Stroke::new(1.0, BRIGHT);

    widgets.active.bg_fill = DISCHARGE_DIM;
    widgets.active.weak_bg_fill = DISCHARGE_DIM;
    widgets.active.bg_stroke = Stroke::new(1.0, DISCHARGE_HOT);
    widgets.active.fg_stroke = Stroke::new(1.0, BRIGHT);

    widgets.open.bg_fill = RAISED_HOT;
    widgets.open.weak_bg_fill = RAISED_HOT;
    widgets.open.bg_stroke = Stroke::new(1.0, DISCHARGE_DIM);
    widgets.open.fg_stroke = Stroke::new(1.0, TEXT);

    visuals
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// Chrome that sits above or beside the document, closed by a hairline on the
/// edge facing the document.
pub fn chrome(edge: Edge) -> Frame {
    let (top, bottom, left, right) = match edge {
        Edge::Bottom => (0, 1, 0, 0),
        Edge::Top => (1, 0, 0, 0),
        Edge::Left => (0, 0, 1, 0),
    };
    Frame::NONE
        .fill(PANEL)
        .inner_margin(Margin::symmetric((UNIT * 1.5) as i8, UNIT as i8))
        .outer_margin(Margin {
            top,
            bottom,
            left,
            right,
        })
        .corner_radius(SQUARE)
}

/// Which side of a panel faces the document it frames.
pub enum Edge {
    Top,
    Bottom,
    Left,
}

/// The document well: the recessed surface the editor sits in.
pub fn well() -> Frame {
    Frame::NONE
        .fill(VOID)
        .stroke(Stroke::new(1.0, LINE))
        .inner_margin(Margin::same(UNIT as i8))
        .corner_radius(SQUARE)
}

// ---------------------------------------------------------------------------
// Small shared widgets
// ---------------------------------------------------------------------------

/// A field label: small, muted, upper-case, above the thing it names.
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .text_style(TextStyle::Small)
            .color(MUTED),
    );
}

/// The application wordmark.
pub fn wordmark(ui: &mut Ui) {
    ui.label(
        RichText::new("APTERONOTUS")
            .text_style(TextStyle::Heading)
            .color(DISCHARGE),
    );
}

/// A panel title, in the display face.
pub fn section_heading(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        tick(ui, DISCHARGE);
        ui.label(
            RichText::new(text)
                .text_style(TextStyle::Heading)
                .size(17.0)
                .color(BRIGHT),
        );
    });
}

/// Outlined, never filled: the look every control that is not Run wears.
fn outlined(ui: &mut Ui, enabled: bool) -> Color32 {
    let label = if enabled { TEXT } else { MUTED };
    let widgets = &mut ui.visuals_mut().widgets;
    widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    widgets.inactive.fg_stroke = Stroke::new(1.0, label);
    widgets.hovered.weak_bg_fill = RAISED_HOT;
    widgets.hovered.bg_stroke = Stroke::new(1.0, DISCHARGE);
    widgets.hovered.fg_stroke = Stroke::new(1.0, BRIGHT);
    widgets.active.weak_bg_fill = RAISED;
    widgets.active.bg_stroke = Stroke::new(1.0, DISCHARGE_HOT);
    widgets.active.fg_stroke = Stroke::new(1.0, BRIGHT);
    widgets.open.weak_bg_fill = RAISED_HOT;
    widgets.open.bg_stroke = Stroke::new(1.0, DISCHARGE);
    widgets.open.fg_stroke = Stroke::new(1.0, BRIGHT);
    label
}

/// A secondary action: outlined, never filled, so the accent stays on the one
/// control that starts sound.
pub fn secondary_button(ui: &mut Ui, enabled: bool, text: &str) -> Response {
    ui.scope(|ui| {
        let label = outlined(ui, enabled);
        eframe::egui::Button::new(RichText::new(text).color(label)).ui(ui)
    })
    .inner
}

/// A secondary action that opens a menu. Returns whatever the menu body
/// returned, or `None` while it is closed.
pub fn menu_button<R>(
    ui: &mut Ui,
    text: &str,
    width: f32,
    content: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    ui.scope(|ui| {
        let label = outlined(ui, true);
        let button = eframe::egui::Button::new(RichText::new(text).color(label));
        eframe::egui::menu::MenuButton::from_button(button)
            .ui(ui, |ui| {
                // Both bounds: a popup has no natural width, so rows that ask
                // for `available_width` would otherwise grow without limit.
                ui.set_min_width(width);
                ui.set_max_width(width);
                // Menu rows are full-bleed and flat; the frame belongs to the
                // popup, not to every row inside it.
                let widgets = &mut ui.visuals_mut().widgets;
                widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                widgets.inactive.bg_stroke = Stroke::NONE;
                widgets.hovered.weak_bg_fill = RAISED_HOT;
                widgets.hovered.bg_stroke = Stroke::NONE;
                widgets.active.weak_bg_fill = DISCHARGE_DIM;
                widgets.active.bg_stroke = Stroke::NONE;
                content(ui)
            })
            .1
            .map(|inner| inner.inner)
    })
    .inner
}

/// A program control, drawn as a fader: a thin rail, a filled trail and a
/// square handle in the accent.
pub fn fader(ui: &mut Ui, value: &mut f64, range: std::ops::RangeInclusive<f64>) -> Response {
    ui.scope(|ui| {
        // `Slider` sizes its rail from the style rather than from the space it
        // is given, so the style is what has to be widened.
        ui.spacing_mut().slider_width = ui.available_width();
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.fg_stroke = Stroke::new(1.0, DISCHARGE);
        widgets.hovered.fg_stroke = Stroke::new(1.0, DISCHARGE_HOT);
        widgets.active.fg_stroke = Stroke::new(1.0, BRIGHT);
        eframe::egui::Slider::new(value, range)
            .show_value(false)
            .ui(ui)
    })
    .inner
}

/// Give a scroll area a handle that belongs to the palette: quiet at rest,
/// accented once the pointer is on it.
pub fn scrollbars(ui: &mut Ui) {
    let widgets = &mut ui.visuals_mut().widgets;
    widgets.inactive.bg_fill = LINE;
    widgets.hovered.bg_fill = DISCHARGE_DIM;
    widgets.active.bg_fill = DISCHARGE;
}

/// A labelled value: the label small and muted, the value in ordinary text.
pub fn readout(ui: &mut Ui, label: &str, value: impl Into<String>) {
    // Right-to-left layouts add items in reverse, so the value goes first.
    ui.spacing_mut().item_spacing.x = UNIT * 0.6;
    if ui.layout().horizontal_placement() == eframe::egui::Align::Max {
        ui.label(RichText::new(value.into()).color(BRIGHT));
        field_label(ui, label);
    } else {
        field_label(ui, label);
        ui.label(RichText::new(value.into()).color(BRIGHT));
    }
}

/// A keyboard-shortcut cap.
pub fn key_hint(ui: &mut Ui, text: &str) {
    Frame::NONE
        .stroke(Stroke::new(1.0, LINE))
        .inner_margin(Margin::symmetric((UNIT * 0.6) as i8, 1))
        .corner_radius(SQUARE)
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .text_style(TextStyle::Small)
                    .color(MUTED),
            );
        });
}

/// A short coloured bar, used to mark a section or a state.
pub fn tick(ui: &mut Ui, accent: Color32) {
    let height = ui.text_style_height(&TextStyle::Body);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(3.0, height), Sense::hover());
    ui.painter().rect_filled(rect, SQUARE, accent);
}

/// A vertical hairline, for separating toolbar groups without a bevel.
pub fn divider(ui: &mut Ui) {
    let height = ui.available_height().min(20.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, height), Sense::hover());
    ui.painter().rect_filled(rect, SQUARE, LINE);
}

/// The transport indicator: a row of bars derived from wall-clock time, so it
/// costs no state and settles flat when nothing is sounding.
pub fn discharge_meter(ui: &mut Ui, accent: Color32, active: bool) {
    const BARS: usize = 9;
    let height = ui.text_style_height(&TextStyle::Body);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(BARS as f32 * 3.0, height), Sense::hover());
    let time = ui.input(|input| input.time);
    let painter = ui.painter();
    for bar in 0..BARS {
        let phase = time * 6.0 - bar as f64 * 0.55;
        let amplitude = if active {
            (0.5 + 0.5 * phase.sin()) as f32
        } else {
            0.12
        };
        let bar_height = (height * (0.2 + 0.8 * amplitude)).round().max(2.0);
        let left = rect.left() + bar as f32 * 3.0;
        let bar_rect = eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(left, rect.bottom() - bar_height),
            Vec2::new(2.0, bar_height),
        );
        let color = if active {
            accent.gamma_multiply(0.45 + 0.55 * amplitude)
        } else {
            accent.gamma_multiply(0.5)
        };
        painter.rect_filled(bar_rect, SQUARE, color);
    }
}

/// The editor gutter: right-aligned line numbers on the same baseline grid as
/// the text, closed by a hairline.
pub fn gutter(ui: &mut Ui, lines: usize, current: usize, row_height: f32, font: &FontId) {
    let digits = lines.to_string().len().max(2) as f32;
    let glyph = ui.fonts_mut(|fonts| fonts.glyph_width(font, '0'));
    let width = digits * glyph + UNIT * 1.5;
    // This is painted decoration, not an interactive widget. Registering an
    // implicit hover ID here made egui's sizing and paint passes assign
    // different IDs to the same gutter rectangle.
    let (_, rect) = ui.allocate_space(Vec2::new(width, lines as f32 * row_height));
    let painter = ui.painter();
    for line in 0..lines {
        let top = rect.top() + line as f32 * row_height;
        let color = if line == current { DISCHARGE } else { GUTTER };
        painter.text(
            eframe::egui::pos2(rect.right() - UNIT, top),
            eframe::egui::Align2::RIGHT_TOP,
            line + 1,
            font.clone(),
            color,
        );
    }
    painter.rect_filled(
        eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(rect.right() - 1.0, rect.top()),
            Vec2::new(1.0, rect.height().max(row_height)),
        ),
        SQUARE,
        LINE,
    );
}

/// The primary action: filled with the accent, not merely outlined by it.
pub fn primary_button(ui: &mut Ui, enabled: bool, text: &str) -> Response {
    let fill = if enabled { DISCHARGE_DIM } else { RAISED };
    let stroke = if enabled { DISCHARGE } else { LINE };
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.weak_bg_fill = fill;
        widgets.inactive.bg_stroke = Stroke::new(1.0, stroke);
        widgets.inactive.fg_stroke = Stroke::new(1.0, BRIGHT);
        widgets.hovered.weak_bg_fill = DISCHARGE;
        widgets.hovered.bg_stroke = Stroke::new(1.0, DISCHARGE_HOT);
        widgets.hovered.fg_stroke = Stroke::new(1.0, VOID);
        widgets.active.weak_bg_fill = DISCHARGE_HOT;
        widgets.active.fg_stroke = Stroke::new(1.0, VOID);
        eframe::egui::Button::new(RichText::new(text).color(BRIGHT)).ui(ui)
    })
    .inner
}
