//! The drawing model of the Home view: each widget card becomes a list of
//! primitives (in card-local, top-left coordinates) plus the regions that
//! react to clicks. The platform layer only paints primitives and reports
//! clicks, so all of the visual design and interaction logic lives here.

use super::calendar;
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

const fn font(size: f64, weight: Weight) -> Font {
    Font { size, weight, mono: false, rounded: false }
}

const fn mono(size: f64, weight: Weight) -> Font {
    Font { size, weight, mono: true, rounded: false }
}

const fn rounded(size: f64, weight: Weight) -> Font {
    Font { size, weight, mono: true, rounded: true }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKey {
    Artwork,
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

/// What a click does.
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
}

impl CardDraw {
    fn p(&mut self, p: Prim) {
        self.prims.push(p);
    }
    fn hit(&mut self, r: Rect, act: Act) {
        self.hits.push(HitRegion { r, act });
    }
    fn rect(&mut self, r: Rect, radius: f64, fill: Rgba) {
        self.p(Prim::Rect { r, radius, fill, stroke: None });
    }
    fn text(&mut self, r: Rect, text: impl Into<String>, font: Font, color: Rgba, align: Align) {
        self.p(Prim::Text { r, text: text.into(), font, color, align, wrap: false });
    }
    fn sym(&mut self, r: Rect, name: &str, size: f64, weight: Weight, color: Rgba) {
        self.p(Prim::Symbol { r, name: name.to_string(), size, weight, color });
    }

    /// Topmost hit at a card-local point.
    pub fn hit_at(&self, p: Point) -> Option<&Act> {
        self.hits.iter().rev().find(|h| h.r.contains(p)).map(|h| &h.act)
    }
}

/// A quick control as the card sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    pub title: String,
    pub symbol: String,
    /// `Some(on)` for toggles, `None` for actions.
    pub on: Option<bool>,
}

/// Everything a card may need.
pub struct Ctx<'a> {
    pub data: &'a WidgetData,
    pub timer: &'a Timer,
    /// Monotonic seconds (timer, animations).
    pub mono_now: f64,
    /// Unix seconds (events, media progress).
    pub unix_now: f64,
    pub utc_offset: f64,
    pub accent: Rgba,
    pub controls: &'a [Control],
    pub hover: Option<&'a Act>,
    pub note: &'a str,
    pub editing_note: bool,
    pub camera_on: bool,
    pub calendar_days: u32,
    pub calendar_filter: &'a [String],
    pub weather_enabled: bool,
    pub use_applescript: bool,
}

pub const ORANGE: Rgba = Rgba(1.0, 0.62, 0.04, 1.0);
pub const RED: Rgba = Rgba(1.0, 0.27, 0.23, 1.0);
pub const GREEN: Rgba = Rgba(0.20, 0.84, 0.29, 1.0);
pub const YELLOW: Rgba = Rgba(1.0, 0.80, 0.0, 1.0);
pub const CYAN: Rgba = Rgba(0.39, 0.82, 1.0, 1.0);
const W: fn(f64) -> Rgba = Rgba::white;

/// Card fill / edge.
pub const CARD_FILL: Rgba = Rgba(1.0, 1.0, 1.0, 0.075);
pub const CARD_EDGE: Rgba = Rgba(1.0, 1.0, 1.0, 0.07);

/// Which cards to show for the enabled kinds (cards without anything to
/// show — no battery, no controls — are dropped).
pub fn visible_kinds(enabled: &[WidgetKind], data: &WidgetData, n_controls: usize) -> Vec<WidgetKind> {
    enabled
        .iter()
        .copied()
        .filter(|k| match k {
            WidgetKind::Battery => data.battery.is_some(),
            WidgetKind::QuickToggles => n_controls > 0,
            WidgetKind::Unknown => false,
            _ => true,
        })
        .collect()
}

fn hovered(ctx: &Ctx, a: &Act) -> bool {
    ctx.hover == Some(a)
}

/// Round icon button with a hover highlight.
fn icon_button(d: &mut CardDraw, ctx: &Ctx, r: Rect, name: &str, size: f64, color: Rgba, act: Act) {
    if hovered(ctx, &act) {
        d.rect(r, r.h / 2.0, W(0.12));
    }
    d.sym(r, name, size, Weight::Semibold, color);
    d.hit(r, act);
}

/// Draw one card at size `w`×`h` (card-local coordinates).
pub fn card(kind: WidgetKind, w: f64, h: f64, ctx: &Ctx) -> CardDraw {
    let mut d = CardDraw::default();
    let edge = if kind == WidgetKind::Note && ctx.editing_note { (ctx.accent.with_alpha(0.9), 1.5) } else { (CARD_EDGE, 0.5) };
    d.p(Prim::Rect { r: Rect::new(0.0, 0.0, w, h), radius: CARD_RADIUS, fill: CARD_FILL, stroke: Some(edge) });
    match kind {
        WidgetKind::NowPlaying => now_playing(&mut d, w, h, ctx),
        WidgetKind::Calendar => calendar_card(&mut d, w, h, ctx),
        WidgetKind::Weather => weather_card(&mut d, w, h, ctx),
        WidgetKind::Battery => battery(&mut d, w, h, ctx),
        WidgetKind::SystemStats => system_stats(&mut d, w, h, ctx),
        WidgetKind::Timer => timer_card(&mut d, w, h, ctx),
        WidgetKind::QuickToggles => controls(&mut d, w, h, ctx),
        WidgetKind::Note => note(&mut d, w, h, ctx),
        WidgetKind::Mirror => mirror(&mut d, w, h, ctx),
        WidgetKind::Unknown => {}
    }
    d
}

/// Header line: symbol + title.
fn header(d: &mut CardDraw, symbol: &str, color: Rgba, title: &str, w: f64) {
    d.sym(Rect::new(PAD - 1.0, PAD, 18.0, 16.0), symbol, 12.5, Weight::Semibold, color);
    d.text(Rect::new(PAD + 21.0, PAD, w - 2.0 * PAD - 21.0, 16.0), title, font(12.0, Weight::Semibold), W(0.92), Align::Left);
}

/// Centered status message with an optional button.
fn message(d: &mut CardDraw, ctx: &Ctx, w: f64, symbol: &str, text: &str, button: Option<(&str, Act)>) {
    let top = 34.0;
    d.sym(Rect::new(0.0, top, w, 22.0), symbol, 17.0, Weight::Regular, W(0.45));
    d.p(Prim::Text { r: Rect::new(PAD, top + 25.0, w - 2.0 * PAD, 30.0), text: text.to_string(), font: font(11.0, Weight::Medium), color: W(0.55), align: Align::Center, wrap: true });
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

fn now_playing(d: &mut CardDraw, w: f64, h: f64, ctx: &Ctx) {
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
            Some(MediaSource::MediaKeys) | None if !ctx.use_applescript => ("Media Controls".to_string(), "Controls any player".to_string()),
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

fn calendar_card(d: &mut CardDraw, w: f64, _h: f64, ctx: &Ctx) {
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
    let evs = calendar::upcoming(&ctx.data.events, ctx.unix_now, ctx.utc_offset, ctx.calendar_days, ctx.calendar_filter);
    if evs.is_empty() {
        let txt = if ctx.calendar_days > 1 { "Nothing on today or tomorrow" } else { "No more events today" };
        d.sym(Rect::new(0.0, 42.0, w, 24.0), "checkmark.circle", 19.0, Weight::Regular, W(0.4));
        d.text(Rect::new(PAD, 70.0, w - 2.0 * PAD, 16.0), txt, font(11.5, Weight::Medium), W(0.5), Align::Center);
        return;
    }
    let row_h = 21.5;
    for (i, e) in evs.iter().take(3).enumerate() {
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

fn weather_card(d: &mut CardDraw, w: f64, _h: f64, ctx: &Ctx) {
    let wn = match &ctx.data.weather {
        WeatherStatus::Ready(wn) => wn.clone(),
        WeatherStatus::Error(_, Some(wn)) => wn.clone(),
        WeatherStatus::Error(msg, None) => return message(d, ctx, w, "exclamationmark.icloud", msg, Some(("Retry", Act::WeatherRetry))),
        WeatherStatus::Loading => return message(d, ctx, w, "cloud.sun", "Loading weather…", None),
        WeatherStatus::Off | WeatherStatus::NoLocation => {
            let txt = if ctx.weather_enabled { "Set a location in Settings" } else { "Weather is off" };
            return message(d, ctx, w, "location.slash", txt, None);
        }
    };
    let stale = matches!(ctx.data.weather, WeatherStatus::Error(..));
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

fn battery(d: &mut CardDraw, w: f64, h: f64, _ctx: &Ctx) {
    let Some(b) = _ctx.data.battery else { return };
    let color = battery_color(&b);
    let c = Point::new(w / 2.0, PAD + 33.0);
    d.p(Prim::Ring { c, radius: 29.0, width: 5.0, frac: b.percent as f64 / 100.0, color, track: W(0.12) });
    d.text(Rect::new(c.x - 28.0, c.y - (if b.charging { 11.0 } else { 9.0 }), 56.0, 18.0), format!("{}%", b.percent), rounded(16.0, Weight::Semibold), W(0.97), Align::Center);
    if b.charging {
        d.sym(Rect::new(c.x - 8.0, c.y + 7.0, 16.0, 12.0), "bolt.fill", 9.5, Weight::Bold, GREEN);
    }
    d.text(Rect::new(4.0, h - PAD - 13.0, w - 8.0, 14.0), battery_caption(&b), font(11.0, Weight::Medium), W(0.55), Align::Center);
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
    let sw = (w - 2.0 * PAD - 96.0).clamp(56.0, 110.0);
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

fn timer_card(d: &mut CardDraw, w: f64, h: f64, ctx: &Ctx) {
    let t = ctx.timer;
    let now = ctx.mono_now;
    if t.is_idle() {
        header(d, "timer", ORANGE, "Timer", w);
        let sw_r = Rect::new(w - PAD - 30.0, PAD - 4.0, 30.0, 24.0);
        let act = Act::TimerStopwatch;
        d.rect(sw_r, 12.0, if hovered(ctx, &act) { W(0.18) } else { W(0.09) });
        d.sym(sw_r, "stopwatch", 12.0, Weight::Semibold, W(0.85));
        d.hit(sw_r, act);
        let n = timer::PRESETS.len() as f64;
        let gap = 7.0;
        let pw = (w - 2.0 * PAD - gap * (n - 1.0)) / n;
        for (i, secs) in timer::PRESETS.iter().enumerate() {
            let r = Rect::new(PAD + i as f64 * (pw + gap), 40.0, pw, h - PAD - 40.0);
            let act = Act::TimerPreset(*secs);
            let hot = hovered(ctx, &act);
            d.rect(r, 13.0, if hot { ORANGE.with_alpha(0.28) } else { W(0.08) });
            d.text(Rect::new(r.x, r.y + 9.0, r.w, 24.0), (secs / 60).to_string(), rounded(21.0, Weight::Semibold), if hot { ORANGE } else { W(0.95) }, Align::Center);
            d.text(Rect::new(r.x, r.y + 33.0, r.w, 13.0), "min", font(10.0, Weight::Semibold), W(0.45), Align::Center);
            d.hit(r, act);
        }
        return;
    }
    let finished = t.is_finished();
    let stopwatch = t.is_stopwatch();
    let color = if stopwatch { CYAN } else { ORANGE };
    let c = Point::new(PAD + 31.0, h / 2.0);
    let frac = if stopwatch { t.elapsed(now).map(|e| (e % 60.0) / 60.0).unwrap_or(0.0) } else { t.fraction_left(now).unwrap_or(0.0) };
    let pulse = if finished { 0.55 + 0.45 * (now * std::f64::consts::PI * 2.0).sin().abs() } else { 1.0 };
    let ring_color = if finished { RED.with_alpha(pulse) } else { color };
    d.p(Prim::Ring { c, radius: 29.0, width: 5.0, frac: if finished { 1.0 } else { frac }, color: ring_color, track: color.with_alpha(0.18) });
    let icon = if finished {
        "bell.fill"
    } else if t.is_paused() {
        "pause.fill"
    } else if stopwatch {
        "stopwatch.fill"
    } else {
        "timer"
    };
    d.sym(Rect::new(c.x - 15.0, c.y - 11.0, 30.0, 22.0), icon, 17.0, Weight::Semibold, if finished { RED.with_alpha(pulse) } else { color });
    let x0 = c.x + 29.0 + 14.0;
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

fn controls(d: &mut CardDraw, w: f64, h: f64, ctx: &Ctx) {
    let cells = layout::toggle_cells(Rect::new(0.0, 0.0, w, h), ctx.controls.len());
    let hot = ctx.hover.and_then(|a| match a {
        Act::Control(i) => ctx.controls.get(*i),
        _ => None,
    });
    let caption = hot.map(|c| c.title.clone()).unwrap_or_else(|| "Controls".into());
    d.text(Rect::new(6.0, PAD - 1.0, w - 12.0, 15.0), caption, font(11.0, Weight::Semibold), W(if hot.is_some() { 0.92 } else { 0.5 }), Align::Center);
    for (i, (c, r)) in ctx.controls.iter().zip(cells).enumerate() {
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

fn note(d: &mut CardDraw, w: f64, h: f64, ctx: &Ctx) {
    header(d, "note.text", YELLOW, "Note", w);
    let r = note_text_rect(w, h);
    if !ctx.editing_note {
        let (txt, color) = if ctx.note.trim().is_empty() { ("Click to jot something down…", W(0.35)) } else { (ctx.note, W(0.85)) };
        d.p(Prim::Text { r, text: txt.to_string(), font: font(12.0, Weight::Regular), color, align: Align::Left, wrap: true });
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
}

/// Pick the live activity: a running timer wins over music.
pub fn live_activity(ctx: &Ctx) -> Option<Live> {
    let t = ctx.timer;
    if t.is_active() {
        return Some(Live::Timer {
            text: if t.is_finished() { "0:00".into() } else { t.short_readout(ctx.mono_now) },
            frac: if t.is_stopwatch() || t.is_finished() { 1.0 } else { t.fraction_left(ctx.mono_now).unwrap_or(0.0) },
            stopwatch: t.is_stopwatch(),
            finished: t.is_finished(),
            paused: t.is_paused(),
        });
    }
    let np = ctx.data.now_playing.as_ref()?;
    (np.playing && np.has_track()).then(|| Live::Music { artwork: np.artwork.is_some(), playing: true })
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
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::{BatteryInfo, CalEvent, NowPlaying, WeatherNow};

    struct Fixture {
        data: WidgetData,
        timer: Timer,
        controls: Vec<Control>,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture { data: WidgetData::default(), timer: Timer::default(), controls: vec![] }
        }
        fn ctx<'a>(&'a self, hover: Option<&'a Act>) -> Ctx<'a> {
            Ctx {
                data: &self.data,
                timer: &self.timer,
                mono_now: 100.0,
                unix_now: 1_792_972_800.0 + 10.0 * 3600.0,
                utc_offset: 0.0,
                accent: Rgba(0.0, 0.5, 1.0, 1.0),
                controls: &self.controls,
                hover,
                note: "",
                editing_note: false,
                camera_on: false,
                calendar_days: 2,
                calendar_filter: &[],
                weather_enabled: true,
                use_applescript: false,
            }
        }
    }

    fn texts(d: &CardDraw) -> Vec<String> {
        d.prims.iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.clone()) } else { None }).collect()
    }

    fn inside(d: &CardDraw, w: f64, h: f64) {
        let card = Rect::new(-0.5, -0.5, w + 1.0, h + 1.0);
        for p in &d.prims {
            let r = match p {
                Prim::Rect { r, .. } | Prim::Text { r, .. } | Prim::Symbol { r, .. } | Prim::Image { r, .. } | Prim::Bars { r, .. } | Prim::Camera { r, .. } => *r,
                Prim::Ring { c, radius, width, .. } => Rect::new(c.x - radius - width / 2.0, c.y - radius - width / 2.0, 2.0 * radius + width, 2.0 * radius + width),
                Prim::Line { pts, .. } => {
                    for q in pts {
                        assert!(card.contains(*q), "{q:?}");
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
    fn every_card_stays_inside_at_min_and_max_width() {
        let mut f = Fixture::new();
        f.data.battery = Some(BatteryInfo { percent: 80, on_ac: true, charging: true, minutes_to_full: Some(65), minutes_to_empty: None });
        f.data.now_playing = Some(NowPlaying { title: "Song".into(), artist: "Artist".into(), playing: true, duration: Some(200.0), elapsed: Some(20.0), sampled_at: 1_792_972_800.0 + 36_000.0, artwork: Some(std::sync::Arc::new(vec![1])), ..Default::default() });
        f.data.calendar_access = Access::Granted;
        f.data.weather = WeatherStatus::Ready(WeatherNow { place: "Berlin".into(), temp: 11.6, high: 14.0, low: 6.0, code: 3, is_day: true, ..Default::default() });
        for i in 0..40 {
            f.data.stats.cpu.push(i as f64 / 40.0);
            f.data.stats.rx.push(1000.0 * i as f64);
        }
        f.controls = (0..5).map(|i| Control { title: format!("C{i}"), symbol: "bolt".into(), on: Some(i % 2 == 0) }).collect();
        for kind in WidgetKind::ALL {
            let s = layout::spec(kind, f.controls.len());
            for w in [s.min_w, s.max_w] {
                let d = card(kind, w, layout::CARD_H, &f.ctx(None));
                inside(&d, w, layout::CARD_H);
            }
        }
        f.timer.start(300.0, 50.0);
        let s = layout::spec(WidgetKind::Timer, 0);
        inside(&card(WidgetKind::Timer, s.min_w, layout::CARD_H, &f.ctx(None)), s.min_w, layout::CARD_H);
    }

    #[test]
    fn now_playing_controls_and_hover() {
        let mut f = Fixture::new();
        let d = card(WidgetKind::NowPlaying, 280.0, 112.0, &f.ctx(None));
        assert!(texts(&d).contains(&"Media Controls".to_string()));
        let acts: Vec<&Act> = d.hits.iter().map(|h| &h.act).collect();
        assert_eq!(acts, [&Act::Media(Command::Previous), &Act::Media(Command::PlayPause), &Act::Media(Command::Next)]);
        let play = d.hits[1].r;
        assert_eq!(d.hit_at(Point::new(play.center_x(), play.center_y())), Some(&Act::Media(Command::PlayPause)));
        assert!(d.prims.iter().any(|p| matches!(p, Prim::Symbol { name, .. } if name == "play.fill")));
        // Hover adds a highlight behind the button.
        let hov = Act::Media(Command::PlayPause);
        let dh = card(WidgetKind::NowPlaying, 280.0, 112.0, &f.ctx(Some(&hov)));
        assert_eq!(dh.prims.len(), d.prims.len() + 1);
        f.data.now_playing = Some(NowPlaying { title: "T".into(), artist: "A".into(), playing: true, duration: Some(100.0), elapsed: Some(50.0), sampled_at: 1_792_972_800.0 + 36_000.0, ..Default::default() });
        let d = card(WidgetKind::NowPlaying, 280.0, 112.0, &f.ctx(None));
        let t = texts(&d);
        assert!(t.contains(&"T".to_string()) && t.contains(&"0:50".to_string()) && t.contains(&"-0:50".to_string()));
        assert!(d.prims.iter().any(|p| matches!(p, Prim::Bars { playing: true, .. })));
        assert!(d.prims.iter().any(|p| matches!(p, Prim::Symbol { name, .. } if name == "pause.fill")));
    }

    #[test]
    fn calendar_states() {
        let mut f = Fixture::new();
        let d = card(WidgetKind::Calendar, 230.0, 112.0, &f.ctx(None));
        assert!(d.hits.iter().any(|h| h.act == Act::CalendarAllow));
        f.data.calendar_access = Access::Denied;
        let d = card(WidgetKind::Calendar, 230.0, 112.0, &f.ctx(None));
        assert!(d.hits.iter().any(|h| h.act == Act::OpenPrivacy("Calendars")));
        f.data.calendar_access = Access::Granted;
        let d = card(WidgetKind::Calendar, 230.0, 112.0, &f.ctx(None));
        assert!(texts(&d).contains(&"Nothing on today or tomorrow".to_string()));
        assert_eq!(texts(&d)[0], "Mon, Oct 26");
        let now = 1_792_972_800.0 + 36_000.0;
        f.data.events = (0..5)
            .map(|i| CalEvent { id: format!("e{i}"), title: format!("Event {i}"), start: now + 1800.0 * (i as f64 + 1.0), end: now + 1800.0 * (i as f64 + 2.0), time_label: "11:00".into(), color: RED, ..Default::default() })
            .collect();
        let d = card(WidgetKind::Calendar, 230.0, 112.0, &f.ctx(None));
        let rows: Vec<&Act> = d.hits.iter().filter(|h| matches!(h.act, Act::OpenEvent(_))).map(|h| &h.act).collect();
        assert_eq!(rows, [&Act::OpenEvent("e0".into()), &Act::OpenEvent("e1".into()), &Act::OpenEvent("e2".into())]);
        assert!(texts(&d).contains(&"in 30 min".to_string()));
    }

    #[test]
    fn timer_card_flows() {
        let mut f = Fixture::new();
        let d = card(WidgetKind::Timer, 180.0, 112.0, &f.ctx(None));
        let presets: Vec<&Act> = d.hits.iter().filter(|h| matches!(h.act, Act::TimerPreset(_))).map(|h| &h.act).collect();
        assert_eq!(presets.len(), 3);
        assert!(d.hits.iter().any(|h| h.act == Act::TimerStopwatch));
        f.timer.start(300.0, 40.0);
        let d = card(WidgetKind::Timer, 180.0, 112.0, &f.ctx(None));
        assert!(texts(&d).contains(&"4:00".to_string()));
        assert!(d.hits.iter().any(|h| h.act == Act::TimerPause) && d.hits.iter().any(|h| h.act == Act::TimerReset));
        f.timer.tick(1000.0);
        let d = card(WidgetKind::Timer, 180.0, 112.0, &f.ctx(None));
        assert!(texts(&d).contains(&"Time's up".to_string()));
        assert!(d.hits.iter().any(|h| h.act == Act::TimerAdd(60)));
        match live_activity(&f.ctx(None)) {
            Some(Live::Timer { finished: true, .. }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn controls_card_caption_follows_hover() {
        let mut f = Fixture::new();
        f.controls = vec![
            Control { title: "Awake".into(), symbol: "cup.and.saucer.fill".into(), on: Some(true) },
            Control { title: "Pick Color".into(), symbol: "eyedropper".into(), on: None },
        ];
        let w = layout::spec(WidgetKind::QuickToggles, 2).min_w;
        let d = card(WidgetKind::QuickToggles, w, 112.0, &f.ctx(None));
        assert_eq!(texts(&d), ["Controls"]);
        assert_eq!(d.hits.len(), 2);
        let h = Act::Control(1);
        let d = card(WidgetKind::QuickToggles, w, 112.0, &f.ctx(Some(&h)));
        assert_eq!(texts(&d), ["Pick Color"]);
    }

    #[test]
    fn visible_kinds_drop_empty_cards() {
        let mut data = WidgetData::default();
        let all = WidgetKind::ALL.to_vec();
        let v = visible_kinds(&all, &data, 0);
        assert!(!v.contains(&WidgetKind::Battery) && !v.contains(&WidgetKind::QuickToggles));
        data.battery = Some(BatteryInfo::default());
        assert_eq!(visible_kinds(&all, &data, 3).len(), all.len());
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
        assert_eq!(live_activity(&f.ctx(None)), None);
        f.data.now_playing = Some(NowPlaying { title: "x".into(), playing: true, ..Default::default() });
        assert_eq!(live_activity(&f.ctx(None)), Some(Live::Music { artwork: false, playing: true }));
        f.timer.start(60.0, 100.0);
        match live_activity(&f.ctx(None)) {
            Some(Live::Timer { text, .. }) => assert_eq!(text, "1:00"),
            other => panic!("{other:?}"),
        }
        let prims = live_prims(&live_activity(&f.ctx(None)).unwrap(), Rect::new(0.0, 0.0, 40.0, 32.0), Rect::new(200.0, 0.0, 40.0, 32.0), Rgba(0.0, 0.5, 1.0, 1.0), 0.0);
        assert!(prims.iter().any(|p| matches!(p, Prim::Ring { .. })));
    }
}
