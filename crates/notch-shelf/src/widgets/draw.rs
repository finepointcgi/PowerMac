//! The drawing model of the Home view: each widget card becomes a list of
//! primitives (in card-local, top-left coordinates) plus the regions that
//! react to clicks. The platform layer only paints primitives and reports
//! clicks, so all of the visual design and interaction logic lives here.
//!
//! The built-in cards are in this file; the "build your own" and newer
//! cards are in [`super::cards`].

use std::sync::Arc;

use super::calendar;
use super::catalog::KindSettings;
use super::instance::WidgetSize;
use super::layout::{self, CARD_RADIUS, PAD};
use super::media::{self, Command};
use super::stats::{self, Pressure};
use super::timer::{self, Timer};
use super::weather;
use super::{Access, MediaSource, Rgba, WeatherStatus, WidgetData, WidgetKind};
use crate::geometry::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
    Bold,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
    pub size: f64,
    pub weight: Weight,
    /// Monospaced digits (timers, rates).
    pub mono: bool,
    /// SF Pro Rounded (big numbers).
    pub rounded: bool,
}

pub const fn font(size: f64, weight: Weight) -> Font {
    Font { size, weight, mono: false, rounded: false }
}

pub const fn mono(size: f64, weight: Weight) -> Font {
    Font { size, weight, mono: true, rounded: false }
}

pub const fn rounded(size: f64, weight: Weight) -> Font {
    Font { size, weight, mono: true, rounded: true }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Images a card can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKey {
    Artwork,
    /// Decoded from bytes (plugin images), by content hash.
    Data(u64),
    /// A file / app icon, by path hash.
    Icon(u64),
}

/// Where the platform layer gets an image it doesn't have yet.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageSource {
    Data { bytes: Arc<Vec<u8>>, template: bool },
    /// Icon of a file / folder / app at a path, or of an app named /
    /// identified by bundle id.
    Icon(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    /// Rounded rectangle (radius ≥ h/2 makes a capsule / circle).
    Rect { r: Rect, radius: f64, fill: Rgba, stroke: Option<(Rgba, f64)> },
    /// One line of text, vertically centered in `r` (or wrapped inside `r`
    /// when `wrap`), truncated with an ellipsis.
    Text { r: Rect, text: String, font: Font, color: Rgba, align: Align, wrap: bool },
    /// SF Symbol centered in `r`.
    Symbol { r: Rect, name: String, size: f64, weight: Weight, color: Rgba },
    Image { r: Rect, key: ImageKey, radius: f64 },
    /// Polyline; `fill` closes it down to `baseline` and fills the area.
    Line { pts: Vec<Point>, color: Rgba, width: f64, fill: Option<(f64, Rgba)> },
    /// Ring: a full `track` circle and a `frac` (0..1) arc clockwise from
    /// 12 o'clock.
    Ring { c: Point, radius: f64, width: f64, frac: f64, color: Rgba, track: Rgba },
    /// Animated "music bars" equalizer (static heights when not playing).
    Bars { r: Rect, color: Rgba, playing: bool },
    /// The live camera preview.
    Camera { r: Rect, radius: f64 },
}

/// What a click does (card-local; the card's instance is known to the
/// caller).
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Media(Command),
    OpenEvent(String),
    OpenCalendar,
    CalendarAllow,
    /// Open a System Settings privacy pane ("Calendars", "Camera").
    OpenPrivacy(&'static str),
    TimerPreset(u32),
    TimerStopwatch,
    TimerPause,
    TimerReset,
    TimerAdd(u32),
    /// Index into the controls list.
    Control(usize),
    EditNote,
    Mirror,
    WeatherRetry,
    /// Re-run a Script / xbar / Web widget.
    Refresh,
    /// The card's own click action (Script / Web / xbar title).
    CardAction,
    /// Item `i` of the content list.
    Item(usize),
    /// Run shortcut `i` of the widget's list.
    Shortcut(usize),
    /// Launcher item `i`.
    Launch(usize),
    /// Output device `i` of the sound state.
    SoundDevice(usize),
    /// The volume slider: the track's card-local x range.
    SoundSlider(f64, f64),
    SoundMute,
    /// Tick a reminder (by id).
    ReminderDone(String),
    OpenReminders,
    RemindersAllow,
    OpenUrl(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct HitRegion {
    pub r: Rect,
    pub act: Act,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardDraw {
    pub prims: Vec<Prim>,
    pub hits: Vec<HitRegion>,
    /// Images the card refers to, for loading.
    pub images: Vec<(ImageKey, ImageSource)>,
    /// A vertically scrolling list: (visible area, max scroll).
    pub scroll: Option<(Rect, f64)>,
}

impl CardDraw {
    pub fn p(&mut self, p: Prim) {
        self.prims.push(p);
    }
    pub fn hit(&mut self, r: Rect, act: Act) {
        self.hits.push(HitRegion { r, act });
    }
    pub fn rect(&mut self, r: Rect, radius: f64, fill: Rgba) {
        self.p(Prim::Rect { r, radius, fill, stroke: None });
    }
    pub fn text(&mut self, r: Rect, text: impl Into<String>, font: Font, color: Rgba, align: Align) {
        self.p(Prim::Text { r, text: text.into(), font, color, align, wrap: false });
    }
    pub fn wrap(&mut self, r: Rect, text: impl Into<String>, font: Font, color: Rgba, align: Align) {
        self.p(Prim::Text { r, text: text.into(), font, color, align, wrap: true });
    }
    pub fn sym(&mut self, r: Rect, name: &str, size: f64, weight: Weight, color: Rgba) {
        self.p(Prim::Symbol { r, name: name.to_string(), size, weight, color });
    }
    pub fn image(&mut self, r: Rect, key: ImageKey, src: ImageSource, radius: f64) {
        if !self.images.iter().any(|(k, _)| *k == key) {
            self.images.push((key, src));
        }
        self.p(Prim::Image { r, key, radius });
    }

    /// Topmost hit at a card-local point.
    pub fn hit_at(&self, p: Point) -> Option<&Act> {
        self.hits.iter().rev().find(|h| h.r.contains(p)).map(|h| &h.act)
    }
}

/// A quick control as the card sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    pub id: String,
    pub title: String,
    pub symbol: String,
    /// `Some(on)` for toggles, `None` for actions.
    pub on: Option<bool>,
}

/// UI state of one card, kept by the platform layer across frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardUi {
    pub timer: Timer,
    pub note: String,
    pub editing: bool,
    /// Scroll offset of a list card (points).
    pub scroll: f64,
    /// Reminders being ticked off: (id, mono time of the click).
    pub done: Vec<(String, f64)>,
    /// Launcher / shortcut item that was just clicked (index, mono time).
    pub pressed: Option<(usize, f64)>,
}

/// One card to draw.
pub struct Card<'a> {
    pub id: &'a str,
    pub kind: WidgetKind,
    pub size: WidgetSize,
    pub settings: &'a KindSettings,
    pub ui: &'a CardUi,
}

/// Everything a card may need besides its own input.
#[derive(Clone)]
pub struct Ctx<'a> {
    pub data: &'a WidgetData,
    /// Monotonic seconds (timer, animations).
    pub mono_now: f64,
    /// Unix seconds (events, media progress).
    pub unix_now: f64,
    pub utc_offset: f64,
    pub accent: Rgba,
    pub controls: &'a [Control],
    /// The hovered region of this card.
    pub hover: Option<&'a Act>,
    pub camera_on: bool,
    /// The system uses a 24-hour clock.
    pub hour24: bool,
}

pub const ORANGE: Rgba = Rgba(1.0, 0.62, 0.04, 1.0);
pub const RED: Rgba = Rgba(1.0, 0.27, 0.23, 1.0);
pub const GREEN: Rgba = Rgba(0.20, 0.84, 0.29, 1.0);
pub const YELLOW: Rgba = Rgba(1.0, 0.80, 0.0, 1.0);
pub const CYAN: Rgba = Rgba(0.39, 0.82, 1.0, 1.0);
pub const BLUE: Rgba = Rgba(0.04, 0.52, 1.0, 1.0);
pub const INDIGO: Rgba = Rgba(0.37, 0.36, 0.90, 1.0);
pub const PINK: Rgba = Rgba(1.0, 0.22, 0.37, 1.0);
pub const PURPLE: Rgba = Rgba(0.75, 0.35, 0.95, 1.0);
pub const TEAL: Rgba = Rgba(0.25, 0.78, 0.88, 1.0);
pub const W: fn(f64) -> Rgba = Rgba::white;

/// Card fill / edge.
pub const CARD_FILL: Rgba = Rgba(1.0, 1.0, 1.0, 0.075);
pub const CARD_EDGE: Rgba = Rgba(1.0, 1.0, 1.0, 0.07);

/// Should a card of `kind` be shown (cards without anything to show — no
/// battery, no controls — are dropped)?
pub fn kind_visible(kind: WidgetKind, has_battery: bool, n_controls: usize) -> bool {
    match kind {
        WidgetKind::Battery => has_battery,
        WidgetKind::QuickToggles => n_controls > 0,
        WidgetKind::Unknown => false,
        _ => true,
    }
}

/// Which cards to show for the enabled kinds (kept for callers of the
/// kind-based API).
pub fn visible_kinds(enabled: &[WidgetKind], data: &WidgetData, n_controls: usize) -> Vec<WidgetKind> {
    enabled.iter().copied().filter(|k| kind_visible(*k, data.battery.is_some(), n_controls)).collect()
}

pub fn hovered(ctx: &Ctx, a: &Act) -> bool {
    ctx.hover == Some(a)
}

/// Round icon button with a hover highlight.
pub fn icon_button(d: &mut CardDraw, ctx: &Ctx, r: Rect, name: &str, size: f64, color: Rgba, act: Act) {
    if hovered(ctx, &act) {
        d.rect(r, r.h / 2.0, W(0.12));
    }
    d.sym(r, name, size, Weight::Semibold, color);
    d.hit(r, act);
}

/// Draw one card at size `w`×`h` (card-local coordinates).
pub fn card(c: &Card, w: f64, h: f64, ctx: &Ctx) -> CardDraw {
    let mut d = CardDraw::default();
    let edge = if c.kind == WidgetKind::Note && c.ui.editing { (ctx.accent.with_alpha(0.9), 1.5) } else { (CARD_EDGE, 0.5) };
    d.p(Prim::Rect { r: Rect::new(0.0, 0.0, w, h), radius: CARD_RADIUS, fill: CARD_FILL, stroke: Some(edge) });
    match c.kind {
        WidgetKind::NowPlaying => now_playing(&mut d, c, w, h, ctx),
        WidgetKind::Calendar => calendar_card(&mut d, c, w, h, ctx),
        WidgetKind::Weather => weather_card(&mut d, c, w, h, ctx),
        WidgetKind::Battery => battery(&mut d, w, h, ctx),
        WidgetKind::SystemStats => system_stats(&mut d, w, h, ctx),
        WidgetKind::Timer => timer_card(&mut d, c, w, h, ctx),
        WidgetKind::QuickToggles => controls(&mut d, c, w, h, ctx),
        WidgetKind::Note => note(&mut d, c, w, h, ctx),
        WidgetKind::Mirror => mirror(&mut d, w, h, ctx),
        WidgetKind::Script | WidgetKind::Xbar | WidgetKind::Web => super::cards::custom(&mut d, c, w, h, ctx),
        WidgetKind::Shortcuts => super::cards::shortcuts(&mut d, c, w, h, ctx),
        WidgetKind::Launcher => super::cards::launcher(&mut d, c, w, h, ctx),
        WidgetKind::Bluetooth => super::cards::bluetooth(&mut d, c, w, h, ctx),
        WidgetKind::Sound => super::cards::sound(&mut d, c, w, h, ctx),
        WidgetKind::Reminders => super::cards::reminders(&mut d, c, w, h, ctx),
        WidgetKind::WorldClock => super::cards::world_clock(&mut d, c, w, h, ctx),
        WidgetKind::Unknown => {}
    }
    d
}

/// Header line: symbol + title.
pub fn header(d: &mut CardDraw, symbol: &str, color: Rgba, title: &str, w: f64) {
    d.sym(Rect::new(PAD - 1.0, PAD, 18.0, 16.0), symbol, 12.5, Weight::Semibold, color);
    d.text(Rect::new(PAD + 21.0, PAD, w - 2.0 * PAD - 21.0, 16.0), title, font(12.0, Weight::Semibold), W(0.92), Align::Left);
}

/// Centered status message with an optional button.
pub fn message(d: &mut CardDraw, ctx: &Ctx, w: f64, symbol: &str, text: &str, button: Option<(&str, Act)>) {
    message_at(d, ctx, w, 34.0, symbol, text, button)
}

pub fn message_at(d: &mut CardDraw, ctx: &Ctx, w: f64, top: f64, symbol: &str, text: &str, button: Option<(&str, Act)>) {
    d.sym(Rect::new(0.0, top, w, 22.0), symbol, 17.0, Weight::Regular, W(0.45));
    let th = if button.is_some() { 28.0 } else { (layout::CARD_H - top - 25.0 - 6.0).max(14.0) };
    d.wrap(Rect::new(PAD, top + 25.0, w - 2.0 * PAD, th), text, font(11.0, Weight::Medium), W(0.55), Align::Center);
    if let Some((label, act)) = button {
        let bw = (label.chars().count() as f64 * 6.6 + 22.0).min(w - 2.0 * PAD);
        let r = Rect::new(w / 2.0 - bw / 2.0, top + 54.0, bw, 20.0);
        let fill = if hovered(ctx, &act) { ctx.accent.with_alpha(1.0) } else { ctx.accent.with_alpha(0.85) };
        d.rect(r, 10.0, fill);
        d.text(r, label, font(11.0, Weight::Semibold), W(1.0), Align::Center);
        d.hit(r, act);
    }
}

// ----- Now Playing -------------------------------------------------------

fn now_playing(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let use_applescript = matches!(c.settings, KindSettings::NowPlaying(s) if s.use_applescript);
    let np = ctx.data.now_playing.as_ref().filter(|n| n.has_track());
    let art = Rect::new(PAD, PAD, h - 2.0 * PAD, h - 2.0 * PAD);
    match np {
        Some(n) if n.artwork.is_some() => d.p(Prim::Image { r: art, key: ImageKey::Artwork, radius: 13.0 }),
        _ => {
            d.rect(art, 13.0, W(0.08));
            d.sym(art, "music.note", 30.0, Weight::Medium, W(0.32));
        }
    }
    let x0 = art.right() + 13.0;
    let x1 = w - PAD;
    let cw = x1 - x0;
    let playing = np.is_some_and(|n| n.playing);
    let (title, sub) = match np {
        Some(n) => {
            let sub = if n.artist.is_empty() { n.album.clone() } else { n.artist.clone() };
            (n.title.clone(), if sub.is_empty() { n.app.clone() } else { sub })
        }
        None => match ctx.data.now_playing.as_ref().map(|n| n.source) {
            Some(MediaSource::MediaKeys) | None if !use_applescript => ("Media Controls".to_string(), "Controls any player".to_string()),
            _ => ("Not Playing".to_string(), "Music or Spotify".to_string()),
        },
    };
    let bars_w = if playing { 20.0 } else { 0.0 };
    d.text(Rect::new(x0, PAD + 1.0, cw - bars_w, 18.0), title, font(14.0, Weight::Semibold), W(0.96), Align::Left);
    d.text(Rect::new(x0, PAD + 20.0, cw, 16.0), sub, font(12.5, Weight::Regular), W(0.55), Align::Left);
    if playing {
        d.p(Prim::Bars { r: Rect::new(x1 - 15.0, PAD + 4.0, 15.0, 12.0), color: ctx.accent.with_alpha(1.0), playing: true });
    }
    // Progress.
    let prog_y = PAD + 50.0;
    if let Some(n) = np {
        if let (Some(p), Some(dur)) = (media::progress(n, ctx.unix_now), n.duration) {
            let el = media::elapsed_at(n, ctx.unix_now).unwrap_or(0.0);
            let tw = if dur >= 3600.0 { 40.0 } else { 30.0 };
            d.text(Rect::new(x0, prog_y - 7.0, tw, 14.0), super::format_clock(el), mono(10.0, Weight::Medium), W(0.45), Align::Left);
            d.text(Rect::new(x1 - tw - 4.0, prog_y - 7.0, tw + 4.0, 14.0), format!("-{}", super::format_clock(dur - el)), mono(10.0, Weight::Medium), W(0.45), Align::Right);
            let bar = Rect::new(x0 + tw + 4.0, prog_y - 2.0, (cw - 2.0 * tw - 12.0).max(10.0), 4.0);
            d.rect(bar, 2.0, W(0.16));
            d.rect(Rect::new(bar.x, bar.y, (bar.w * p).max(4.0), bar.h), 2.0, W(0.88));
        }
    }
    // Controls.
    let cy = h - PAD - 13.0;
    let cx = x0 + cw / 2.0;
    let play_icon = if playing { "pause.fill" } else { "play.fill" };
    icon_button(d, ctx, Rect::new(cx - 52.0 - 15.0, cy - 15.0, 30.0, 30.0), "backward.fill", 15.0, W(0.9), Act::Media(Command::Previous));
    icon_button(d, ctx, Rect::new(cx - 17.0, cy - 17.0, 34.0, 34.0), play_icon, 21.0, W(1.0), Act::Media(Command::PlayPause));
    icon_button(d, ctx, Rect::new(cx + 52.0 - 15.0, cy - 15.0, 30.0, 30.0), "forward.fill", 15.0, W(0.9), Act::Media(Command::Next));
}

// ----- Calendar ----------------------------------------------------------

fn calendar_card(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let (days, filter) = match c.settings {
        KindSettings::Calendar(s) => (s.days.clamp(1.0, 7.0) as u32, s.calendars.as_slice()),
        _ => (2, &[][..]),
    };
    let title = calendar::date_header(ctx.unix_now, ctx.utc_offset);
    header(d, "calendar", RED, &title, w);
    d.hit(Rect::new(0.0, 0.0, w, PAD + 20.0), Act::OpenCalendar);
    match ctx.data.calendar_access {
        Access::Granted => {}
        Access::Requesting => return message(d, ctx, w, "lock.fill", "Waiting for permission…", None),
        Access::Denied => return message(d, ctx, w, "lock.fill", "No access to your calendars", Some(("Open Settings", Act::OpenPrivacy("Calendars")))),
        Access::MissingUsageKey | Access::Unavailable => return message(d, ctx, w, "calendar.badge.exclamationmark", "Calendar isn't available in this build", None),
        Access::NotDetermined | Access::Unknown => return message(d, ctx, w, "calendar.badge.plus", "Show your next events here", Some(("Allow Access", Act::CalendarAllow))),
    }
    let evs = calendar::upcoming(&ctx.data.events, ctx.unix_now, ctx.utc_offset, days, filter);
    if evs.is_empty() {
        let txt = if days > 1 { "Nothing on today or tomorrow" } else { "No more events today" };
        d.sym(Rect::new(0.0, 42.0, w, 24.0), "checkmark.circle", 19.0, Weight::Regular, W(0.4));
        d.text(Rect::new(PAD, 70.0, w - 2.0 * PAD, 16.0), txt, font(11.5, Weight::Medium), W(0.5), Align::Center);
        return;
    }
    let row_h = 21.5;
    let rows = (((h - PAD - 24.0 - 4.0) / row_h).floor() as usize).max(1);
    for (i, e) in evs.iter().take(rows).enumerate() {
        let y = PAD + 24.0 + i as f64 * row_h;
        let row = Rect::new(PAD - 5.0, y, w - 2.0 * PAD + 10.0, row_h - 1.5);
        let act = Act::OpenEvent(e.id.clone());
        let now = calendar::is_now(e, ctx.unix_now);
        if hovered(ctx, &act) {
            d.rect(row, 7.0, W(0.08));
        } else if now {
            d.rect(row, 7.0, e.color.with_alpha(0.16));
        }
        d.rect(Rect::new(PAD, y + 3.5, 3.5, row_h - 8.5), 1.75, e.color.with_alpha(1.0));
        let when = calendar::when_label(e, ctx.unix_now, ctx.utc_offset);
        let ww = (when.chars().count() as f64 * 6.2 + 4.0).clamp(28.0, 86.0);
        let soon = when == "Now" || when.starts_with("in ");
        d.text(Rect::new(w - PAD - ww, y, ww, row_h - 1.5), when, font(11.0, Weight::Medium), if soon { e.color.with_alpha(1.0) } else { W(0.5) }, Align::Right);
        d.text(Rect::new(PAD + 10.0, y, w - 2.0 * PAD - ww - 14.0, row_h - 1.5), e.title.clone(), font(12.5, Weight::Medium), W(0.92), Align::Left);
        d.hit(row, act);
    }
}

// ----- Weather -----------------------------------------------------------

fn weather_card(d: &mut CardDraw, c: &Card, w: f64, _h: f64, ctx: &Ctx) {
    let has_location = match c.settings {
        KindSettings::Weather(s) => !s.location.trim().is_empty(),
        _ => true,
    };
    let status = ctx.data.weather_for(c.id);
    let wn = match status {
        WeatherStatus::Ready(wn) => wn.clone(),
        WeatherStatus::Error(_, Some(wn)) => wn.clone(),
        WeatherStatus::Error(msg, None) => return message(d, ctx, w, "exclamationmark.icloud", msg, Some(("Retry", Act::WeatherRetry))),
        WeatherStatus::Loading => return message(d, ctx, w, "cloud.sun", "Loading weather…", None),
        WeatherStatus::Off | WeatherStatus::NoLocation => {
            let txt = if has_location { "Loading weather…" } else { "Set a location in Settings" };
            return message(d, ctx, w, "location.slash", txt, None);
        }
    };
    let stale = matches!(status, WeatherStatus::Error(..));
    d.text(Rect::new(PAD, PAD, w - 2.0 * PAD - 30.0, 16.0), wn.place.clone(), font(12.0, Weight::Semibold), W(0.92), Align::Left);
    if stale {
        d.sym(Rect::new(w - PAD - 44.0, PAD, 14.0, 16.0), "exclamationmark.triangle.fill", 9.0, Weight::Bold, W(0.4));
    }
    d.sym(Rect::new(w - PAD - 28.0, PAD - 3.0, 30.0, 26.0), weather::symbol(wn.code, wn.is_day), 21.0, Weight::Regular, weather::tint(wn.code, wn.is_day));
    d.text(Rect::new(PAD - 1.0, 32.0, w - 2.0 * PAD, 40.0), weather::temp(wn.temp), rounded(36.0, Weight::Medium), W(0.97), Align::Left);
    d.text(Rect::new(PAD, 72.0, w - 2.0 * PAD, 15.0), weather::describe(wn.code), font(11.5, Weight::Semibold), W(0.78), Align::Left);
    d.text(Rect::new(PAD, 87.0, w - 2.0 * PAD, 14.0), format!("H:{}  L:{}", weather::temp(wn.high), weather::temp(wn.low)), font(11.0, Weight::Medium), W(0.5), Align::Left);
}

// ----- Battery -----------------------------------------------------------

/// "1:05 to full", "3:20 left", ...
pub fn battery_caption(b: &super::BatteryInfo) -> String {
    let hm = |m: u32| format!("{}:{:02}", m / 60, m % 60);
    if b.charging {
        match b.minutes_to_full {
            Some(m) if m > 0 => format!("{} to full", hm(m)),
            _ => "Charging".into(),
        }
    } else if b.on_ac {
        if b.percent >= 95 {
            "Charged".into()
        } else {
            "On power".into()
        }
    } else {
        match b.minutes_to_empty {
            Some(m) if m > 0 => format!("{} left", hm(m)),
            _ => "On battery".into(),
        }
    }
}

pub fn battery_color(b: &super::BatteryInfo) -> Rgba {
    if b.charging || (b.on_ac && b.percent >= 95) {
        GREEN
    } else if !b.on_ac && b.percent <= 20 {
        RED
    } else {
        W(0.92)
    }
}

fn battery(d: &mut CardDraw, w: f64, h: f64, ctx: &Ctx) {
    let Some(b) = ctx.data.battery else { return };
    let color = battery_color(&b);
    let wide = w > layout::SMALL_W + 1.0;
    let c = if wide { Point::new(PAD + 38.0, h / 2.0) } else { Point::new(w / 2.0, PAD + 33.0) };
    d.p(Prim::Ring { c, radius: 29.0, width: 5.0, frac: b.percent as f64 / 100.0, color, track: W(0.12) });
    d.text(Rect::new(c.x - 28.0, c.y - (if b.charging { 11.0 } else { 9.0 }), 56.0, 18.0), format!("{}%", b.percent), rounded(16.0, Weight::Semibold), W(0.97), Align::Center);
    if b.charging {
        d.sym(Rect::new(c.x - 8.0, c.y + 7.0, 16.0, 12.0), "bolt.fill", 9.5, Weight::Bold, GREEN);
    }
    if wide {
        let x = c.x + 29.0 + 18.0;
        let tw = w - x - PAD;
        d.text(Rect::new(x, h / 2.0 - 24.0, tw, 16.0), "This Mac", font(12.0, Weight::Semibold), W(0.6), Align::Left);
        d.text(Rect::new(x, h / 2.0 - 7.0, tw, 22.0), battery_caption(&b), font(15.0, Weight::Semibold), W(0.95), Align::Left);
        let src = if b.on_ac { "Power Adapter" } else { "Battery" };
        d.text(Rect::new(x, h / 2.0 + 15.0, tw, 15.0), src, font(11.0, Weight::Medium), W(0.45), Align::Left);
    } else {
        d.text(Rect::new(4.0, h - PAD - 13.0, w - 8.0, 14.0), battery_caption(&b), font(11.0, Weight::Medium), W(0.55), Align::Center);
    }
}

// ----- System stats ------------------------------------------------------

fn spark(d: &mut CardDraw, values: &[f64], r: Rect, max: f64, color: Rgba) {
    let pts = stats::sparkline(values, r, max);
    if pts.len() >= 2 {
        d.p(Prim::Line { pts, color, width: 1.4, fill: Some((r.bottom(), color.with_alpha(0.16))) });
    } else {
        d.rect(Rect::new(r.x, r.bottom() - 1.0, r.w, 1.0), 0.5, W(0.12));
    }
}

fn system_stats(d: &mut CardDraw, w: f64, h: f64, ctx: &Ctx) {
    let s = &ctx.data.stats;
    let row_h = (h - 2.0 * PAD) / 3.0;
    let sw = (w - 2.0 * PAD - 96.0).clamp(56.0, 300.0);
    let pct = |v: Option<f64>| v.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "—".into());
    let mem_color = match s.pressure {
        Pressure::Normal => GREEN,
        Pressure::Warning => YELLOW,
        Pressure::Critical => RED,
    };
    let rows: [(&str, &str, Rgba); 3] = [("cpu", "CPU", CYAN), ("memorychip", "Memory", mem_color), ("network", "Network", ORANGE)];
    for (i, (sym, label, color)) in rows.iter().enumerate() {
        let y = PAD + i as f64 * row_h;
        let mid = y + row_h / 2.0;
        d.sym(Rect::new(PAD - 2.0, mid - 9.0, 20.0, 18.0), sym, 13.0, Weight::Medium, color.with_alpha(0.95));
        let tx = PAD + 24.0;
        let sr = Rect::new(w - PAD - sw, mid - 9.0, sw, 18.0);
        match i {
            0 | 1 => {
                d.text(Rect::new(tx, mid - 14.0, 70.0, 12.0), *label, font(9.5, Weight::Semibold), W(0.45), Align::Left);
                let v = if i == 0 { s.cpu.last() } else { s.mem.last() };
                d.text(Rect::new(tx, mid - 2.0, 70.0, 16.0), pct(v), mono(13.0, Weight::Semibold), W(0.95), Align::Left);
                spark(d, if i == 0 { &s.cpu.values } else { &s.mem.values }, sr, 1.0, *color);
            }
            _ => {
                let rx = s.rx.last().map(stats::format_rate).unwrap_or_else(|| "—".into());
                let tx_rate = s.tx.last().map(stats::format_rate).unwrap_or_else(|| "—".into());
                let tw = w - 2.0 * PAD - 24.0 - sw - 4.0;
                d.text(Rect::new(tx, mid - 14.0, tw, 13.0), format!("↓ {rx}"), mono(11.0, Weight::Semibold), W(0.95), Align::Left);
                d.text(Rect::new(tx, mid + 0.5, tw, 13.0), format!("↑ {tx_rate}"), mono(11.0, Weight::Medium), W(0.55), Align::Left);
                let max = stats::nice_max(s.rx.max().max(s.tx.max()));
                spark(d, &s.tx.values, sr, max, W(0.5));
                spark(d, &s.rx.values, sr, max, *color);
            }
        }
    }
}

// ----- Timer -------------------------------------------------------------

fn timer_card(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let t = &c.ui.timer;
    let now = ctx.mono_now;
    if t.is_idle() {
        header(d, "timer", ORANGE, "Timer", w);
        let sw_r = Rect::new(w - PAD - 30.0, PAD - 4.0, 30.0, 24.0);
        let act = Act::TimerStopwatch;
        d.rect(sw_r, 12.0, if hovered(ctx, &act) { W(0.18) } else { W(0.09) });
        d.sym(sw_r, "stopwatch", 12.0, Weight::Semibold, W(0.85));
        d.hit(sw_r, act);
        let presets = match c.settings {
            KindSettings::Timer(s) => s.preset_secs(),
            _ => timer::PRESETS.to_vec(),
        };
        // As many presets as fit at a comfortable width.
        let gap = 7.0;
        let fit = (((w - 2.0 * PAD + gap) / (48.0 + gap)).floor() as usize).max(1);
        let presets: Vec<u32> = presets.into_iter().take(fit).collect();
        let n = presets.len() as f64;
        let pw = (w - 2.0 * PAD - gap * (n - 1.0)) / n;
        for (i, secs) in presets.iter().enumerate() {
            let r = Rect::new(PAD + i as f64 * (pw + gap), 40.0, pw, h - PAD - 40.0);
            let act = Act::TimerPreset(*secs);
            let hot = hovered(ctx, &act);
            d.rect(r, 13.0, if hot { ORANGE.with_alpha(0.28) } else { W(0.08) });
            let (num, unit) = if secs % 3600 == 0 {
                ((secs / 3600).to_string(), "hr")
            } else if secs % 60 == 0 {
                ((secs / 60).to_string(), "min")
            } else {
                (secs.to_string(), "sec")
            };
            d.text(Rect::new(r.x, r.y + 9.0, r.w, 24.0), num, rounded(21.0, Weight::Semibold), if hot { ORANGE } else { W(0.95) }, Align::Center);
            d.text(Rect::new(r.x, r.y + 33.0, r.w, 13.0), unit, font(10.0, Weight::Semibold), W(0.45), Align::Center);
            d.hit(r, act);
        }
        return;
    }
    let finished = t.is_finished();
    let stopwatch = t.is_stopwatch();
    let color = if stopwatch { CYAN } else { ORANGE };
    let cc = Point::new(PAD + 31.0, h / 2.0);
    let frac = if stopwatch { t.elapsed(now).map(|e| (e % 60.0) / 60.0).unwrap_or(0.0) } else { t.fraction_left(now).unwrap_or(0.0) };
    let pulse = if finished { 0.55 + 0.45 * (now * std::f64::consts::PI * 2.0).sin().abs() } else { 1.0 };
    let ring_color = if finished { RED.with_alpha(pulse) } else { color };
    d.p(Prim::Ring { c: cc, radius: 29.0, width: 5.0, frac: if finished { 1.0 } else { frac }, color: ring_color, track: color.with_alpha(0.18) });
    let icon = if finished {
        "bell.fill"
    } else if t.is_paused() {
        "pause.fill"
    } else if stopwatch {
        "stopwatch.fill"
    } else {
        "timer"
    };
    d.sym(Rect::new(cc.x - 15.0, cc.y - 11.0, 30.0, 22.0), icon, 17.0, Weight::Semibold, if finished { RED.with_alpha(pulse) } else { color });
    let x0 = cc.x + 29.0 + 14.0;
    let tw = w - PAD - x0;
    if finished {
        d.text(Rect::new(x0, 18.0, tw + 4.0, 22.0), "Time's up", font(16.0, Weight::Semibold), RED, Align::Left);
    } else {
        let readout = t.readout(now);
        let size = if readout.len() > 6 { 20.0 } else { 25.0 };
        d.text(Rect::new(x0 - 1.0, 16.0, tw + 6.0, 30.0), readout, rounded(size, Weight::Semibold), if t.is_paused() { W(0.6) } else { W(0.97) }, Align::Left);
    }
    let by = h - PAD - 34.0;
    let b1 = Rect::new(x0, by, 34.0, 34.0);
    let b2 = Rect::new(x0 + 42.0, by, 34.0, 34.0);
    let (a1, s1) = if finished { (Act::TimerAdd(60), "goforward.60") } else { (Act::TimerPause, if t.is_paused() { "play.fill" } else { "pause.fill" }) };
    let hot1 = hovered(ctx, &a1);
    d.rect(b1, 17.0, color.with_alpha(if hot1 { 0.36 } else { 0.22 }));
    d.sym(b1, s1, 14.0, Weight::Bold, color);
    d.hit(b1, a1);
    let a2 = Act::TimerReset;
    d.rect(b2, 17.0, W(if hovered(ctx, &a2) { 0.2 } else { 0.1 }));
    d.sym(b2, "xmark", 12.5, Weight::Bold, W(0.85));
    d.hit(b2, a2);
}

// ----- Quick controls ----------------------------------------------------

/// Indexes (into `ctx.controls`) of the controls a card shows: the ids in
/// its settings (in that order), or all.
pub fn control_indexes(c: &Card, ctx: &Ctx) -> Vec<usize> {
    let only = match c.settings {
        KindSettings::QuickToggles(s) => s.controls.as_slice(),
        _ => &[],
    };
    if only.is_empty() {
        return (0..ctx.controls.len()).collect();
    }
    only.iter().filter_map(|id| ctx.controls.iter().position(|c| c.id.eq_ignore_ascii_case(id.trim()))).collect()
}

fn controls(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let idx = control_indexes(c, ctx);
    let fit = layout::toggle_capacity(w);
    let idx: Vec<usize> = idx.into_iter().take(fit).collect();
    let cells = layout::toggle_cells(Rect::new(0.0, 0.0, w, h), idx.len());
    let hot = ctx.hover.and_then(|a| match a {
        Act::Control(i) => ctx.controls.get(*i),
        _ => None,
    });
    let caption = hot.map(|c| c.title.clone()).unwrap_or_else(|| "Controls".into());
    d.text(Rect::new(6.0, PAD - 1.0, w - 12.0, 15.0), caption, font(11.0, Weight::Semibold), W(if hot.is_some() { 0.92 } else { 0.5 }), Align::Center);
    for (&i, r) in idx.iter().zip(cells) {
        let Some(c) = ctx.controls.get(i) else { continue };
        let act = Act::Control(i);
        let hover = hovered(ctx, &act);
        let on = c.on == Some(true);
        // A gray (graphite) accent wouldn't stand out: "on" is white then.
        let vivid = is_vivid(ctx.accent);
        let fill = match (on, vivid) {
            (true, true) => ctx.accent.with_alpha(if hover { 1.0 } else { 0.92 }),
            (true, false) => W(if hover { 1.0 } else { 0.9 }),
            _ => W(if hover { 0.2 } else { 0.11 }),
        };
        let fg = if on && !vivid { Rgba(0.0, 0.0, 0.0, 0.85) } else { W(if on { 1.0 } else { 0.88 }) };
        d.rect(r, r.h / 2.0, fill);
        d.sym(r, &c.symbol, 14.0, Weight::Semibold, fg);
        d.hit(r, act);
    }
}

/// Saturated enough to be used as a highlight.
pub fn is_vivid(c: Rgba) -> bool {
    let mx = c.0.max(c.1).max(c.2);
    let mn = c.0.min(c.1).min(c.2);
    mx - mn > 0.18
}

// ----- Note --------------------------------------------------------------

/// Where the note's text goes (card-local); the platform puts its text
/// editor here while editing.
pub fn note_text_rect(w: f64, h: f64) -> Rect {
    Rect::new(PAD, PAD + 22.0, w - 2.0 * PAD, h - 2.0 * PAD - 22.0)
}

fn note(d: &mut CardDraw, c: &Card, w: f64, h: f64, _ctx: &Ctx) {
    header(d, "note.text", YELLOW, "Note", w);
    let r = note_text_rect(w, h);
    if !c.ui.editing {
        let (txt, color) = if c.ui.note.trim().is_empty() { ("Click to jot something down…", W(0.35)) } else { (c.ui.note.as_str(), W(0.85)) };
        d.wrap(r, txt, font(12.0, Weight::Regular), color, Align::Left);
    } else {
        d.text(Rect::new(w - PAD - 60.0, PAD, 60.0, 16.0), "esc", font(10.0, Weight::Semibold), W(0.35), Align::Right);
    }
    d.hit(Rect::new(0.0, 0.0, w, h), Act::EditNote);
}

// ----- Mirror ------------------------------------------------------------

fn mirror(d: &mut CardDraw, w: f64, h: f64, ctx: &Ctx) {
    let r = Rect::new(6.0, 6.0, w - 12.0, h - 12.0);
    let act = Act::Mirror;
    if ctx.camera_on {
        d.p(Prim::Camera { r, radius: CARD_RADIUS - 6.0 });
        d.hit(r, act);
        return;
    }
    let (sym, txt, a) = match ctx.data.camera_access {
        Access::Denied => ("video.slash.fill", "No camera access", Act::OpenPrivacy("Camera")),
        Access::MissingUsageKey | Access::Unavailable => ("video.slash.fill", "Unavailable", act),
        _ => ("person.crop.square", "Mirror", act),
    };
    if hovered(ctx, &a) {
        d.rect(r, CARD_RADIUS - 6.0, W(0.06));
    }
    d.sym(Rect::new(0.0, h / 2.0 - 22.0, w, 30.0), sym, 25.0, Weight::Regular, W(0.5));
    d.text(Rect::new(6.0, h / 2.0 + 13.0, w - 12.0, 15.0), txt, font(11.0, Weight::Semibold), W(0.55), Align::Center);
    d.hit(r, a);
}

// ----- live activities (collapsed notch) ---------------------------------

/// What the collapsed notch shows on its "ears".
#[derive(Clone, Debug, PartialEq)]
pub enum Live {
    Music { artwork: bool, playing: bool },
    Timer { text: String, frac: f64, stopwatch: bool, finished: bool, paused: bool },
    /// A Script / Web / xbar widget with "Live activity" on.
    Custom { symbol: String, color: Rgba, text: String, progress: Option<f64> },
}

/// Pick the live activity: a running timer wins over music, music over
/// custom widgets. `cards` are all enabled cards (any page).
pub fn live_activity(ctx: &Ctx, cards: &[Card]) -> Option<Live> {
    if let Some(t) = cards.iter().filter(|c| c.kind == WidgetKind::Timer).map(|c| &c.ui.timer).find(|t| t.is_active()) {
        return Some(Live::Timer {
            text: if t.is_finished() { "0:00".into() } else { t.short_readout(ctx.mono_now) },
            frac: if t.is_stopwatch() || t.is_finished() { 1.0 } else { t.fraction_left(ctx.mono_now).unwrap_or(0.0) },
            stopwatch: t.is_stopwatch(),
            finished: t.is_finished(),
            paused: t.is_paused(),
        });
    }
    if cards.iter().any(|c| c.kind == WidgetKind::NowPlaying) {
        if let Some(np) = ctx.data.now_playing.as_ref() {
            if np.playing && np.has_track() {
                return Some(Live::Music { artwork: np.artwork.is_some(), playing: true });
            }
        }
    }
    cards.iter().find_map(|c| super::cards::live_custom(c, ctx))
}

/// Primitives for the two ears (rects in the caller's coordinates).
pub fn live_prims(live: &Live, left: Rect, right: Rect, accent: Rgba, mono_now: f64) -> Vec<Prim> {
    let mut v = vec![];
    let lc = Point::new(left.center_x(), left.center_y());
    match live {
        Live::Music { artwork, playing } => {
            let a = Rect::new(lc.x - 11.0, lc.y - 11.0, 22.0, 22.0);
            if *artwork {
                v.push(Prim::Image { r: a, key: ImageKey::Artwork, radius: 6.0 });
            } else {
                v.push(Prim::Rect { r: a, radius: 6.0, fill: W(0.14), stroke: None });
                v.push(Prim::Symbol { r: a, name: "music.note".into(), size: 12.0, weight: Weight::Semibold, color: W(0.85) });
            }
            let b = Rect::new(right.center_x() - 9.0, right.center_y() - 7.0, 18.0, 14.0);
            v.push(Prim::Bars { r: b, color: accent.with_alpha(1.0), playing: *playing });
        }
        Live::Timer { text, frac, stopwatch, finished, paused } => {
            let color = if *finished {
                RED.with_alpha(0.55 + 0.45 * (mono_now * std::f64::consts::PI * 2.0).sin().abs())
            } else if *stopwatch {
                CYAN
            } else {
                ORANGE
            };
            v.push(Prim::Ring { c: lc, radius: 8.5, width: 2.6, frac: *frac, color, track: color.with_alpha(0.25) });
            let txt_color = if *paused { W(0.55) } else { color.with_alpha(1.0) };
            v.push(Prim::Text { r: Rect::new(right.x - 2.0, right.center_y() - 9.0, right.w + 4.0, 18.0), text: text.clone(), font: mono(13.0, Weight::Semibold), color: txt_color, align: Align::Center, wrap: false });
        }
        Live::Custom { symbol, color, text, progress } => {
            match progress {
                Some(p) => v.push(Prim::Ring { c: lc, radius: 8.5, width: 2.6, frac: *p, color: *color, track: color.with_alpha(0.25) }),
                None => v.push(Prim::Symbol { r: Rect::new(lc.x - 11.0, lc.y - 9.0, 22.0, 18.0), name: symbol.clone(), size: 13.0, weight: Weight::Semibold, color: *color }),
            }
            v.push(Prim::Text { r: Rect::new(right.x - 4.0, right.center_y() - 9.0, right.w + 8.0, 18.0), text: text.clone(), font: mono(12.5, Weight::Semibold), color: W(0.95), align: Align::Center, wrap: false });
        }
    }
    v
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::widgets::catalog::{self, CalendarSettings};
    use crate::widgets::{BatteryInfo, CalEvent, NowPlaying, WeatherNow};

    pub struct Fixture {
        pub data: WidgetData,
        pub ui: CardUi,
        pub controls: Vec<Control>,
    }

    pub const NOW: f64 = 1_792_972_800.0 + 10.0 * 3600.0;

    impl Fixture {
        pub fn new() -> Self {
            Fixture { data: WidgetData::default(), ui: CardUi::default(), controls: vec![] }
        }
        pub fn ctx<'a>(&'a self, hover: Option<&'a Act>) -> Ctx<'a> {
            Ctx { data: &self.data, mono_now: 100.0, unix_now: NOW, utc_offset: 0.0, accent: Rgba(0.0, 0.5, 1.0, 1.0), controls: &self.controls, hover, camera_on: false, hour24: true }
        }
    }

    pub fn settings(kind: WidgetKind) -> KindSettings {
        KindSettings::parse(kind, &catalog::default_settings(kind))
    }

    pub fn draw(kind: WidgetKind, settings: &KindSettings, ui: &CardUi, w: f64, ctx: &Ctx) -> CardDraw {
        let size = if w <= layout::SMALL_W + 1.0 {
            WidgetSize::Small
        } else if w <= layout::MEDIUM_W + 1.0 {
            WidgetSize::Medium
        } else {
            WidgetSize::Large
        };
        let c = Card { id: "x", kind, size, settings, ui };
        card(&c, w, layout::CARD_H, ctx)
    }

    pub fn texts(d: &CardDraw) -> Vec<String> {
        d.prims.iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.clone()) } else { None }).collect()
    }

    pub fn inside(d: &CardDraw, w: f64, h: f64) {
        let card = Rect::new(-0.5, -0.5, w + 1.0, h + 1.0);
        for p in &d.prims {
            let r = match p {
                Prim::Rect { r, .. } | Prim::Text { r, .. } | Prim::Symbol { r, .. } | Prim::Image { r, .. } | Prim::Bars { r, .. } | Prim::Camera { r, .. } => *r,
                Prim::Ring { c, radius, width, .. } => Rect::new(c.x - radius - width / 2.0, c.y - radius - width / 2.0, 2.0 * radius + width, 2.0 * radius + width),
                Prim::Line { pts, .. } => {
                    for q in pts {
                        assert!(card.contains(*q), "{q:?} outside {w}x{h}");
                    }
                    continue;
                }
            };
            assert!(r.x >= card.x && r.y >= card.y && r.right() <= card.right() && r.bottom() <= card.bottom(), "{p:?} outside {w}x{h}");
        }
        for h in &d.hits {
            assert!(h.r.x >= card.x && h.r.right() <= card.right(), "{h:?}");
        }
    }

    #[test]
    fn builtin_cards_stay_inside_at_every_allowed_size() {
        let mut f = Fixture::new();
        f.data.battery = Some(BatteryInfo { percent: 80, on_ac: true, charging: true, minutes_to_full: Some(65), minutes_to_empty: None });
        f.data.now_playing = Some(NowPlaying { title: "Song".into(), artist: "Artist".into(), playing: true, duration: Some(200.0), elapsed: Some(20.0), sampled_at: NOW, artwork: Some(std::sync::Arc::new(vec![1])), ..Default::default() });
        f.data.calendar_access = Access::Granted;
        f.data.weather = WeatherStatus::Ready(WeatherNow { place: "Berlin".into(), temp: 11.6, high: 14.0, low: 6.0, code: 3, is_day: true, ..Default::default() });
        for i in 0..40 {
            f.data.stats.cpu.push(i as f64 / 40.0);
            f.data.stats.rx.push(1000.0 * i as f64);
        }
        f.controls = (0..9).map(|i| Control { id: format!("c{i}"), title: format!("C{i}"), symbol: "bolt".into(), on: Some(i % 2 == 0) }).collect();
        for kind in WidgetKind::ALL {
            let s = settings(kind);
            for size in &catalog::kind_info(kind).sizes {
                let d = draw(kind, &s, &f.ui, size.width(), &f.ctx(None));
                inside(&d, size.width(), layout::CARD_H);
            }
        }
        f.ui.timer.start(300.0, 50.0);
        let w = WidgetSize::Medium.width();
        inside(&draw(WidgetKind::Timer, &settings(WidgetKind::Timer), &f.ui, w, &f.ctx(None)), w, layout::CARD_H);
    }

    #[test]
    fn now_playing_controls_and_hover() {
        let mut f = Fixture::new();
        let s = settings(WidgetKind::NowPlaying);
        let d = draw(WidgetKind::NowPlaying, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(texts(&d).contains(&"Media Controls".to_string()));
        let acts: Vec<&Act> = d.hits.iter().map(|h| &h.act).collect();
        assert_eq!(acts, [&Act::Media(Command::Previous), &Act::Media(Command::PlayPause), &Act::Media(Command::Next)]);
        let play = d.hits[1].r;
        assert_eq!(d.hit_at(Point::new(play.center_x(), play.center_y())), Some(&Act::Media(Command::PlayPause)));
        let hov = Act::Media(Command::PlayPause);
        let dh = draw(WidgetKind::NowPlaying, &s, &f.ui, 280.0, &f.ctx(Some(&hov)));
        assert_eq!(dh.prims.len(), d.prims.len() + 1);
        f.data.now_playing = Some(NowPlaying { title: "T".into(), artist: "A".into(), playing: true, duration: Some(100.0), elapsed: Some(50.0), sampled_at: NOW, ..Default::default() });
        let d = draw(WidgetKind::NowPlaying, &s, &f.ui, 280.0, &f.ctx(None));
        let t = texts(&d);
        assert!(t.contains(&"T".to_string()) && t.contains(&"0:50".to_string()) && t.contains(&"-0:50".to_string()));
        assert!(d.prims.iter().any(|p| matches!(p, Prim::Bars { playing: true, .. })));
    }

    #[test]
    fn calendar_states_and_settings() {
        let mut f = Fixture::new();
        let s = settings(WidgetKind::Calendar);
        let d = draw(WidgetKind::Calendar, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(d.hits.iter().any(|h| h.act == Act::CalendarAllow));
        f.data.calendar_access = Access::Denied;
        let d = draw(WidgetKind::Calendar, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(d.hits.iter().any(|h| h.act == Act::OpenPrivacy("Calendars")));
        f.data.calendar_access = Access::Granted;
        let d = draw(WidgetKind::Calendar, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(texts(&d).contains(&"Nothing on today or tomorrow".to_string()));
        assert_eq!(texts(&d)[0], "Mon, Oct 26");
        f.data.events = (0..5)
            .map(|i| CalEvent {
                id: format!("e{i}"),
                title: format!("Event {i}"),
                start: NOW + 1800.0 * (i as f64 + 1.0),
                end: NOW + 1800.0 * (i as f64 + 2.0),
                time_label: "11:00".into(),
                color: RED,
                calendar: if i == 1 { "Home".into() } else { "Work".into() },
                ..Default::default()
            })
            .collect();
        let d = draw(WidgetKind::Calendar, &s, &f.ui, 280.0, &f.ctx(None));
        let rows: Vec<&Act> = d.hits.iter().filter(|h| matches!(h.act, Act::OpenEvent(_))).map(|h| &h.act).collect();
        assert_eq!(rows, [&Act::OpenEvent("e0".into()), &Act::OpenEvent("e1".into()), &Act::OpenEvent("e2".into())]);
        // A per-instance calendar filter.
        let work = KindSettings::Calendar(CalendarSettings { days: 1.0, calendars: vec!["work".into()] });
        let d = draw(WidgetKind::Calendar, &work, &f.ui, 280.0, &f.ctx(None));
        assert!(!texts(&d).contains(&"Event 1".to_string()));
    }

    #[test]
    fn timer_card_flows_and_presets() {
        let mut f = Fixture::new();
        let s = settings(WidgetKind::Timer);
        let d = draw(WidgetKind::Timer, &s, &f.ui, 280.0, &f.ctx(None));
        let presets: Vec<&Act> = d.hits.iter().filter(|h| matches!(h.act, Act::TimerPreset(_))).map(|h| &h.act).collect();
        assert_eq!(presets, [&Act::TimerPreset(300), &Act::TimerPreset(600), &Act::TimerPreset(1500)]);
        let custom = KindSettings::Timer(catalog::TimerSettings { presets: vec![1.0, 2.0, 3.0, 60.0, 90.0] });
        let d = draw(WidgetKind::Timer, &custom, &f.ui, WidgetSize::Medium.width(), &f.ctx(None));
        assert_eq!(d.hits.iter().filter(|h| matches!(h.act, Act::TimerPreset(_))).count(), 4, "only what fits");
        let d = draw(WidgetKind::Timer, &custom, &f.ui, WidgetSize::Large.width(), &f.ctx(None));
        assert!(texts(&d).contains(&"hr".to_string()));
        f.ui.timer.start(300.0, 40.0);
        let d = draw(WidgetKind::Timer, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(texts(&d).contains(&"4:00".to_string()));
        f.ui.timer.tick(1000.0);
        let d = draw(WidgetKind::Timer, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(texts(&d).contains(&"Time's up".to_string()));
        let ui = f.ui.clone();
        let cards = [Card { id: "t", kind: WidgetKind::Timer, size: WidgetSize::Medium, settings: &s, ui: &ui }];
        assert!(matches!(live_activity(&f.ctx(None), &cards), Some(Live::Timer { finished: true, .. })));
    }

    #[test]
    fn controls_card_filters_and_fits() {
        let mut f = Fixture::new();
        f.controls = vec![
            Control { id: "awake".into(), title: "Awake".into(), symbol: "cup.and.saucer.fill".into(), on: Some(true) },
            Control { id: "pick".into(), title: "Pick Color".into(), symbol: "eyedropper".into(), on: None },
            Control { id: "ocr".into(), title: "Extract Text".into(), symbol: "text.viewfinder".into(), on: None },
        ];
        let s = settings(WidgetKind::QuickToggles);
        let w = WidgetSize::Small.width();
        let d = draw(WidgetKind::QuickToggles, &s, &f.ui, w, &f.ctx(None));
        assert_eq!(texts(&d), ["Controls"]);
        assert_eq!(d.hits.len(), 3);
        let h = Act::Control(1);
        let d = draw(WidgetKind::QuickToggles, &s, &f.ui, w, &f.ctx(Some(&h)));
        assert_eq!(texts(&d), ["Pick Color"]);
        let only = KindSettings::QuickToggles(catalog::QuickControlsSettings { controls: vec!["ocr".into(), "nope".into(), "AWAKE".into()] });
        let d = draw(WidgetKind::QuickToggles, &only, &f.ui, w, &f.ctx(None));
        assert_eq!(d.hits.iter().map(|h| h.act.clone()).collect::<Vec<_>>(), vec![Act::Control(2), Act::Control(0)]);
    }

    #[test]
    fn kind_visibility() {
        assert!(!kind_visible(WidgetKind::Battery, false, 3));
        assert!(!kind_visible(WidgetKind::QuickToggles, true, 0));
        assert!(kind_visible(WidgetKind::Script, false, 0));
        assert!(!kind_visible(WidgetKind::Unknown, true, 3));
        let data = WidgetData { battery: Some(BatteryInfo::default()), ..Default::default() };
        assert_eq!(visible_kinds(&WidgetKind::ALL, &data, 3).len(), WidgetKind::ALL.len());
    }

    #[test]
    fn battery_texts() {
        let b = BatteryInfo { percent: 15, on_ac: false, charging: false, minutes_to_empty: Some(95), minutes_to_full: None };
        assert_eq!(battery_caption(&b), "1:35 left");
        assert_eq!(battery_color(&b), RED);
        let c = BatteryInfo { percent: 100, on_ac: true, charging: false, ..b };
        assert_eq!(battery_caption(&c), "Charged");
        assert_eq!(battery_color(&c), GREEN);
        let ch = BatteryInfo { charging: true, on_ac: true, minutes_to_full: None, ..b };
        assert_eq!(battery_caption(&ch), "Charging");
    }

    #[test]
    fn live_activity_priority() {
        let mut f = Fixture::new();
        let np = settings(WidgetKind::NowPlaying);
        let ts = settings(WidgetKind::Timer);
        let idle = CardUi::default();
        let mut running = CardUi::default();
        running.timer.start(60.0, 100.0);
        let pick = |f: &Fixture, list: &[(WidgetKind, &KindSettings, &CardUi)]| -> Option<Live> {
            let c: Vec<Card> = list.iter().map(|(k, s, u)| Card { id: "i", kind: *k, size: WidgetSize::Medium, settings: s, ui: u }).collect();
            live_activity(&f.ctx(None), &c)
        };
        let both = [(WidgetKind::NowPlaying, &np, &idle), (WidgetKind::Timer, &ts, &idle)];
        assert_eq!(pick(&f, &both), None);
        f.data.now_playing = Some(NowPlaying { title: "x".into(), playing: true, ..Default::default() });
        assert_eq!(pick(&f, &both), Some(Live::Music { artwork: false, playing: true }));
        // Without a Now Playing card there's no music live activity.
        assert_eq!(pick(&f, &both[1..]), None);
        let with_timer = [(WidgetKind::NowPlaying, &np, &idle), (WidgetKind::Timer, &ts, &running)];
        let live = pick(&f, &with_timer).unwrap();
        assert!(matches!(&live, Live::Timer { text, .. } if text == "1:00"));
        let prims = live_prims(&live, Rect::new(0.0, 0.0, 40.0, 32.0), Rect::new(200.0, 0.0, 40.0, 32.0), Rgba(0.0, 0.5, 1.0, 1.0), 0.0);
        assert!(prims.iter().any(|p| matches!(p, Prim::Ring { .. })));
    }
}
