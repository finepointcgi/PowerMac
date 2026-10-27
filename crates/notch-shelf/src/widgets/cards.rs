//! Cards of the configurable widgets: Script / xbar / Web content,
//! Shortcuts, Launcher, Bluetooth batteries, Sound, Reminders and World
//! Clocks. Same conventions as [`super::draw`]: card-local top-left
//! coordinates, primitives + hit regions.

use super::bluetooth::{self, BtDevice, BtKind, Part};
use super::catalog::{ClickAction, ClockStyle, KindSettings, LaunchItem, LaunchKind};
use super::clocks;
use super::content::{hash_bytes, Content, Item, ItemAction, Status, Style};
use super::draw::*;
use super::layout::{CARD_RADIUS, PAD, SMALL_W};
use super::reminders::{self, Urgency};
use super::{Access, Rgba, WidgetKind};
use crate::geometry::{Point, Rect};

/// Rough text width (system font) for layout decisions.
pub fn est_width(text: &str, size: f64) -> f64 {
    text.chars().map(|c| if c.is_ascii_uppercase() || c == 'm' || c == 'w' { 0.68 } else if c.is_ascii() { 0.55 } else { 1.0 }).sum::<f64>() * size
}

/// Width of a big SF Rounded / monospaced-digit value.
pub fn est_value_width(text: &str, size: f64) -> f64 {
    text.chars().map(|c| match c {
        '0'..='9' => 0.66,
        '.' | ',' | ':' | '\'' => 0.3,
        ' ' => 0.28,
        '%' => 0.8,
        c if c.is_ascii_uppercase() => 0.7,
        c if c.is_ascii() => 0.58,
        _ => 0.9,
    }).sum::<f64>() * size
}

/// The biggest size up to `max` (down to `min`) at which `text` fits `w`.
pub fn fit_size(text: &str, w: f64, max: f64, min: f64) -> f64 {
    let at1 = est_value_width(text, 1.0).max(0.01);
    (w / at1).clamp(min, max)
}

fn kind_color(kind: WidgetKind) -> Rgba {
    match kind {
        WidgetKind::Script => GREEN,
        WidgetKind::Xbar => ORANGE,
        WidgetKind::Web => BLUE,
        _ => CYAN,
    }
}

/// (title, symbol, gauge range, card click) from the settings.
struct CustomCfg<'a> {
    title: &'a str,
    symbol: &'a str,
    gauge: Option<(f64, f64)>,
    click: bool,
}

fn custom_cfg<'a>(c: &'a Card) -> CustomCfg<'a> {
    match c.settings {
        KindSettings::Script(s) => CustomCfg { title: &s.title, symbol: &s.symbol, gauge: s.gauge.then_some((s.min, s.max)), click: s.click != ClickAction::None },
        KindSettings::Web(s) => CustomCfg { title: &s.title, symbol: &s.symbol, gauge: Some((s.min, s.max)), click: !s.open_url.trim().is_empty() },
        KindSettings::Xbar(s) => CustomCfg { title: &s.title, symbol: "puzzlepiece.extension", gauge: None, click: false },
        _ => CustomCfg { title: "", symbol: "questionmark", gauge: None, click: false },
    }
}

/// The header: symbol (or the content's image) + title, a refresh button
/// that shows while the card is hovered and an error badge.
#[allow(clippy::too_many_arguments)]
fn custom_header(d: &mut CardDraw, ctx: &Ctx, w: f64, content: Option<&Content>, symbol: &str, color: Rgba, title: &str, error: bool) {
    let icon = Rect::new(PAD - 1.0, PAD, 18.0, 16.0);
    match content.and_then(|c| c.image.as_ref()) {
        Some(img) => d.image(Rect::new(PAD, PAD + 1.0, 15.0, 15.0), ImageKey::Data(img.key), ImageSource::Data { bytes: img.bytes.clone(), template: img.template }, 3.0),
        None => d.sym(icon, symbol, 12.5, Weight::Semibold, color),
    }
    let right = if error { 40.0 } else { 22.0 };
    d.text(Rect::new(PAD + 21.0, PAD, w - 2.0 * PAD - 21.0 - right, 16.0), title, font(12.0, Weight::Semibold), W(0.92), Align::Left);
    let rr = Rect::new(w - PAD - 20.0, PAD - 3.0, 22.0, 22.0);
    if ctx.hover.is_some() {
        let hot = hovered(ctx, &Act::Refresh);
        if hot {
            d.rect(rr, 11.0, W(0.12));
        }
        d.sym(rr, "arrow.clockwise", 10.5, Weight::Bold, W(if hot { 0.9 } else { 0.4 }));
    }
    d.hit(rr, Act::Refresh);
    if error {
        d.sym(Rect::new(w - PAD - 38.0, PAD, 16.0, 16.0), "exclamationmark.triangle.fill", 10.0, Weight::Bold, ORANGE);
    }
}

/// Script / xbar / Web card.
pub fn custom(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let cfg = custom_cfg(c);
    let kc = kind_color(c.kind);
    let fallback_title = super::catalog::kind_info(c.kind).title;
    let status = ctx.data.content(c.id);
    let (content, error) = match status {
        None | Some(Status::Loading) => {
            custom_header(d, ctx, w, None, cfg.symbol, kc, if cfg.title.is_empty() { fallback_title } else { cfg.title }, false);
            return message_at(d, ctx, w, 38.0, "hourglass", "Loading…", None);
        }
        Some(Status::Error { message: m, stale: None, .. }) => {
            custom_header(d, ctx, w, None, cfg.symbol, kc, if cfg.title.is_empty() { fallback_title } else { cfg.title }, false);
            return message_at(d, ctx, w, 34.0, "exclamationmark.triangle", m, Some(("Retry", Act::Refresh)));
        }
        Some(Status::Ready { content, .. }) => (content, None),
        Some(Status::Error { message, stale: Some(s), .. }) => (s.as_ref(), Some(message.as_str())),
    };
    let color = content.color.unwrap_or(kc);
    let symbol = content.symbol.as_deref().unwrap_or(cfg.symbol);
    // Card click (whole card, under everything else).
    let has_click = cfg.click || content.action.is_some();
    if has_click {
        if hovered(ctx, &Act::CardAction) {
            d.rect(Rect::new(0.0, 0.0, w, h), CARD_RADIUS, W(0.04));
        }
        d.hit(Rect::new(0.0, 0.0, w, h), Act::CardAction);
    }
    // xbar: several title lines cycle every 4 s.
    let content_title = if content.alt_titles.is_empty() {
        content.title.clone()
    } else {
        let n = content.alt_titles.len() + 1;
        let k = ((ctx.mono_now / 4.0).floor() as usize) % n;
        if k == 0 {
            content.title.clone()
        } else {
            content.alt_titles[k - 1].clone()
        }
    };
    if content.is_empty() {
        custom_header(d, ctx, w, Some(content), symbol, color, if cfg.title.is_empty() { fallback_title } else { cfg.title }, error.is_some());
        return message_at(d, ctx, w, 38.0, "text.page.slash", error.unwrap_or("No output"), None);
    }
    match content.style {
        Style::Number => {
            let title = if !cfg.title.is_empty() {
                cfg.title.to_string()
            } else if !content_title.is_empty() {
                content_title.clone()
            } else {
                fallback_title.to_string()
            };
            custom_header(d, ctx, w, Some(content), symbol, color, &title, error.is_some());
            number_body(d, content, &cfg, color, w, h, error);
        }
        Style::Text => {
            if cfg.title.is_empty() {
                custom_header(d, ctx, w, Some(content), symbol, color, &content_title, error.is_some());
                let body = if content.body.is_empty() { content.subtitle.as_str() } else { content.body.as_str() };
                d.wrap(Rect::new(PAD, PAD + 24.0, w - 2.0 * PAD, h - 2.0 * PAD - 24.0), body, font(12.0, Weight::Regular), W(0.72), Align::Left);
            } else {
                custom_header(d, ctx, w, Some(content), symbol, color, cfg.title, error.is_some());
                d.text(Rect::new(PAD, PAD + 22.0, w - 2.0 * PAD, 20.0), content_title.clone(), font(15.0, Weight::Semibold), W(0.96), Align::Left);
                d.wrap(Rect::new(PAD, PAD + 44.0, w - 2.0 * PAD, h - 2.0 * PAD - 44.0), content.body.clone(), font(11.5, Weight::Regular), W(0.65), Align::Left);
            }
            if let Some(e) = error {
                d.text(Rect::new(PAD, h - PAD - 13.0, w - 2.0 * PAD, 14.0), e, font(10.0, Weight::Medium), ORANGE.with_alpha(0.9), Align::Left);
            }
        }
        Style::List => {
            let title = if !cfg.title.is_empty() {
                cfg.title.to_string()
            } else if !content_title.is_empty() {
                content_title.clone()
            } else {
                fallback_title.to_string()
            };
            custom_header(d, ctx, w, Some(content), symbol, color, &title, error.is_some());
            let mut top = PAD + 23.0;
            if let Some(v) = content.value.as_ref().filter(|v| !v.is_empty()) {
                d.text(Rect::new(PAD, top, w - 2.0 * PAD, 17.0), format!("{v}{}", unit_suffix(&content.unit)), rounded(14.0, Weight::Semibold), W(0.95), Align::Left);
                top += 19.0;
            }
            list_rows(d, ctx, c.ui.scroll, Rect::new(PAD - 4.0, top, w - 2.0 * PAD + 8.0, h - top - 6.0), &content.items);
        }
    }
}

fn unit_suffix(u: &str) -> String {
    if u.is_empty() || u == "%" || u.starts_with('°') {
        u.to_string()
    } else {
        format!(" {u}")
    }
}

fn number_body(d: &mut CardDraw, content: &Content, cfg: &CustomCfg, color: Rgba, w: f64, h: f64, error: Option<&str>) {
    let value = content.value.clone().unwrap_or_default();
    let gauge = cfg.gauge.and_then(|(lo, hi)| content.gauge(lo, hi));
    let unit = content.unit.clone();
    let caption = error.map(String::from).unwrap_or_else(|| content.subtitle.clone());
    let cap_color = if error.is_some() { ORANGE.with_alpha(0.9) } else { W(0.5) };
    if w <= SMALL_W + 1.0 {
        let unit_w = if unit.is_empty() { 0.0 } else { est_width(&unit, 13.0) + 4.0 };
        let size = fit_size(&value, w - 2.0 * PAD - unit_w, 30.0, 16.0);
        let vw = est_value_width(&value, size).min(w - 2.0 * PAD);
        d.text(Rect::new(PAD - 1.0, 33.0, (w - 2.0 * PAD - unit_w + 1.0).max(vw + 4.0).min(w - PAD), size + 6.0), value, rounded(size, Weight::Semibold), W(0.97), Align::Left);
        if !unit.is_empty() {
            d.text(Rect::new(PAD + vw * 0.94 + 1.0, 33.0 + size * 0.38, (w - PAD - (PAD + vw + 2.0)).max(0.0), 16.0), unit, font(13.0, Weight::Semibold), W(0.55), Align::Left);
        }
        let mut y = 33.0 + size + 10.0;
        if let Some(g) = gauge {
            let bar = Rect::new(PAD, y, w - 2.0 * PAD, 5.0);
            d.rect(bar, 2.5, W(0.12));
            d.rect(Rect::new(bar.x, bar.y, (bar.w * g).max(5.0), bar.h), 2.5, color);
            y += 10.0;
        }
        if !caption.is_empty() && y + 14.0 <= h - 4.0 {
            d.text(Rect::new(PAD, y, w - 2.0 * PAD, 14.0), caption, font(10.5, Weight::Medium), cap_color, Align::Left);
        }
        return;
    }
    let ring_r = 31.0;
    let text_w = if gauge.is_some() { w - 2.0 * PAD - 2.0 * ring_r - 16.0 } else { w - 2.0 * PAD };
    let unit_w = if unit.is_empty() { 0.0 } else { est_width(&unit, 15.0) + 4.0 };
    let size = fit_size(&value, text_w - unit_w, 34.0, 18.0);
    let vw = est_value_width(&value, size).min(text_w);
    d.text(Rect::new(PAD - 1.0, 32.0, (text_w - unit_w + 1.0).max(vw + 4.0), size + 8.0), value, rounded(size, Weight::Semibold), W(0.97), Align::Left);
    if !unit.is_empty() {
        d.text(Rect::new(PAD + vw * 0.94 + 3.0, 32.0 + size * 0.42, (text_w - vw - 3.0).max(0.0), 18.0), unit, font(15.0, Weight::Semibold), W(0.55), Align::Left);
    }
    if !caption.is_empty() {
        d.text(Rect::new(PAD, h - PAD - 16.0, text_w, 15.0), caption, font(11.5, Weight::Medium), cap_color, Align::Left);
    }
    if let Some(g) = gauge {
        let cc = Point::new(w - PAD - ring_r - 2.0, h / 2.0 + 6.0);
        d.p(Prim::Ring { c: cc, radius: ring_r, width: 6.0, frac: g, color, track: color.with_alpha(0.18) });
        d.text(Rect::new(cc.x - ring_r + 8.0, cc.y - 9.0, 2.0 * ring_r - 16.0, 18.0), format!("{:.0}%", g * 100.0), rounded(13.0, Weight::Semibold), W(0.9), Align::Center);
    }
}

pub const ROW_H: f64 = 19.0;
const SEP_H: f64 = 9.0;
const HEADER_ROW_H: f64 = 21.0;

fn item_height(it: &Item) -> f64 {
    if it.separator {
        SEP_H
    } else if it.header {
        HEADER_ROW_H
    } else {
        ROW_H
    }
}

/// First item to show for a scroll offset (row-snapped) and the largest
/// useful offset, for items of `heights` in an area `area_h` tall.
pub fn list_window(heights: &[f64], area_h: f64, scroll: f64) -> (usize, f64) {
    let n = heights.len();
    // Smallest first index whose tail fits.
    let mut max_first = n;
    let mut acc = 0.0;
    for i in (0..n).rev() {
        acc += heights[i];
        if acc > area_h + 0.01 {
            break;
        }
        max_first = i;
    }
    let max_first = if n == 0 { 0 } else { max_first.min(n - 1) };
    let first = ((scroll / ROW_H).round().max(0.0) as usize).min(max_first);
    (first, max_first as f64 * ROW_H)
}

/// Item rows in `area`, scrolled by `scroll` (snapped to rows).
pub fn list_rows(d: &mut CardDraw, ctx: &Ctx, scroll: f64, area: Rect, items: &[Item]) {
    let heights: Vec<f64> = items.iter().map(item_height).collect();
    let (first, max) = list_window(&heights, area.h, scroll);
    d.scroll = Some((area, max));
    let mut y = area.y;
    let mut shown_last = first;
    for (i, it) in items.iter().enumerate().skip(first) {
        let ih = heights[i];
        if y + ih > area.bottom() + 0.01 {
            break;
        }
        shown_last = i;
        let indent = (it.depth.min(3) as f64) * 12.0;
        let x0 = area.x + 4.0 + indent;
        let row = Rect::new(area.x + indent, y, area.w - indent, ih - 1.0);
        if it.separator {
            d.rect(Rect::new(x0, y + ih / 2.0 - 0.5, area.right() - 4.0 - x0, 1.0), 0.5, W(0.1));
            y += ih;
            continue;
        }
        let act = Act::Item(i);
        let actionable = it.action.is_some() && !it.disabled;
        if actionable {
            if hovered(ctx, &act) {
                d.rect(row, 6.0, W(0.09));
            }
            d.hit(row, act.clone());
        }
        let mut tx = x0;
        let mid = y + (ih - 1.0) / 2.0;
        if let Some(img) = &it.image {
            d.image(Rect::new(tx, mid - 7.0, 14.0, 14.0), ImageKey::Data(img.key), ImageSource::Data { bytes: img.bytes.clone(), template: img.template }, 3.0);
            tx += 19.0;
        } else if it.checked {
            d.sym(Rect::new(tx - 1.0, mid - 7.0, 15.0, 14.0), "checkmark", 9.5, Weight::Bold, ctx.accent.with_alpha(1.0));
            tx += 17.0;
        } else if let Some(s) = &it.symbol {
            d.sym(Rect::new(tx - 1.0, mid - 7.0, 16.0, 14.0), s, 11.0, Weight::Semibold, it.color.unwrap_or(W(0.75)));
            tx += 19.0;
        }
        let dim = if it.disabled { 0.35 } else { 1.0 };
        if it.header {
            d.text(Rect::new(tx, y + 2.0, area.right() - tx - 4.0, ih - 3.0), it.title.clone(), font(11.0, Weight::Bold), it.color.unwrap_or(W(0.92)).with_alpha(0.95 * dim), Align::Left);
            if !it.subtitle.is_empty() {
                let sw = (est_width(&it.subtitle, 10.0) + 6.0).min((area.right() - tx) * 0.5);
                d.text(Rect::new(area.right() - 4.0 - sw, y + 2.0, sw, ih - 3.0), it.subtitle.clone(), font(10.0, Weight::Medium), W(0.4), Align::Right);
            }
            y += ih;
            continue;
        }
        let sub_w = if it.subtitle.is_empty() { 0.0 } else { (est_width(&it.subtitle, 11.0) + 6.0).min((area.right() - tx) * 0.45) };
        let tf = if it.mono { mono(11.0, Weight::Medium) } else { font(12.0, Weight::Medium) };
        let tc = match it.color {
            Some(c) if it.symbol.is_none() && !it.header => c.with_alpha(dim),
            _ => W(0.9 * dim),
        };
        let tc = if it.color.is_some() && it.symbol.is_some() { W(0.9 * dim) } else { tc };
        d.text(Rect::new(tx, y, area.right() - tx - 4.0 - sub_w, ih - 1.0), it.title.clone(), tf, tc, Align::Left);
        if sub_w > 0.0 {
            d.text(Rect::new(area.right() - 4.0 - sub_w, y, sub_w, ih - 1.0), it.subtitle.clone(), font(11.0, Weight::Medium), W(0.45 * dim), Align::Right);
        }
        y += ih;
    }
    // "More" hints at the edges.
    let more_below = shown_last + 1 < items.len();
    if first > 0 {
        d.sym(Rect::new(area.right() - 18.0, area.y - 15.0, 14.0, 12.0), "chevron.up", 8.0, Weight::Bold, W(0.35));
    }
    if more_below {
        d.sym(Rect::new(area.right() - 18.0, area.bottom() - 4.0, 14.0, 10.0), "chevron.down", 8.0, Weight::Bold, W(0.35));
    }
}

/// The live activity of a custom card (its value on the closed notch).
pub fn live_custom(c: &Card, ctx: &Ctx) -> Option<Live> {
    let live = match c.settings {
        KindSettings::Script(s) => s.live,
        KindSettings::Web(s) => s.live,
        KindSettings::Xbar(s) => s.live,
        _ => false,
    };
    if !live {
        return None;
    }
    let content = ctx.data.content(c.id)?.content()?;
    let cfg = custom_cfg(c);
    let text = match &content.value {
        Some(v) if !v.is_empty() => format!("{v}{}", if content.unit.chars().count() <= 2 { content.unit.as_str() } else { "" }),
        _ => content.title.clone(),
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(Live::Custom {
        symbol: content.symbol.clone().unwrap_or_else(|| cfg.symbol.to_string()),
        color: content.color.unwrap_or(kind_color(c.kind)),
        text,
        progress: cfg.gauge.and_then(|(lo, hi)| content.gauge(lo, hi)),
    })
}

/// What a click on content item `i` / the card does.
pub fn item_action(content: &Content, i: usize) -> Option<&ItemAction> {
    content.items.get(i).and_then(|it| it.action.as_ref())
}

// ----- Shortcuts -------------------------------------------------------------

/// Pill frames for `names` flowed into rows inside `area`.
pub fn flow_pills(names: &[String], area: Rect, pill_h: f64, gap: f64, full_width: bool) -> Vec<Rect> {
    let mut out = vec![];
    let (mut x, mut y) = (area.x, area.y);
    for n in names {
        let pw = if full_width { area.w } else { (est_width(n, 11.5) + 34.0).clamp(54.0, area.w) };
        if x > area.x && x + pw > area.right() + 0.01 {
            x = area.x;
            y += pill_h + gap;
        }
        if y + pill_h > area.bottom() + 0.01 {
            break;
        }
        out.push(Rect::new(x, y, pw, pill_h));
        x += pw + gap;
    }
    out
}

pub fn shortcuts(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let KindSettings::Shortcuts(s) = c.settings else { return };
    header(d, "square.2.layers.3d.fill", PINK, if s.title.is_empty() { "Shortcuts" } else { &s.title }, w);
    if s.shortcuts.is_empty() {
        return message_at(d, ctx, w, 38.0, "plus.circle", "Add shortcuts in Settings", None);
    }
    let st = ctx.data.shortcuts(c.id);
    let out_line = s.show_output && st.and_then(|s| s.last.as_ref()).is_some();
    let bottom = if out_line { h - PAD - 18.0 } else { h - PAD + 2.0 };
    let area = Rect::new(PAD, PAD + 24.0, w - 2.0 * PAD, bottom - (PAD + 24.0));
    let small = w <= SMALL_W + 1.0;
    let pills = flow_pills(&s.shortcuts, area, 25.0, 6.0, small);
    for (i, r) in pills.iter().enumerate() {
        let name = &s.shortcuts[i];
        let act = Act::Shortcut(i);
        let running = st.and_then(|s| s.running.as_ref()) == Some(name);
        let flash = c.ui.pressed.is_some_and(|(k, t)| k == i && ctx.mono_now - t < 0.3);
        let fill = if running {
            ctx.accent.with_alpha(0.85)
        } else if hovered(ctx, &act) || flash {
            W(0.2)
        } else {
            W(0.1)
        };
        d.rect(*r, r.h / 2.0, fill);
        let icon = if running { "hourglass" } else { "play.fill" };
        d.sym(Rect::new(r.x + 7.0, r.y, 14.0, r.h), icon, 8.5, Weight::Bold, if running { W(1.0) } else { PINK });
        d.text(Rect::new(r.x + 22.0, r.y, r.w - 30.0, r.h), name.clone(), font(11.5, Weight::Semibold), W(0.92), Align::Left);
        d.hit(*r, act);
    }
    if pills.len() < s.shortcuts.len() {
        let more = format!("+{}", s.shortcuts.len() - pills.len());
        d.text(Rect::new(w - PAD - 40.0, PAD, 40.0, 16.0), more, font(10.5, Weight::Semibold), W(0.4), Align::Right);
    }
    if let Some((name, res, _)) = st.and_then(|s| s.last.as_ref()).filter(|_| out_line) {
        let y = h - PAD - 14.0;
        let (sym, col, txt) = match res {
            Ok(o) if o.is_empty() => ("checkmark.circle.fill", GREEN, format!("{name} — done")),
            Ok(o) => ("checkmark.circle.fill", GREEN, o.lines().next().unwrap_or("").to_string()),
            Err(e) => ("xmark.octagon.fill", RED, format!("{name}: {e}")),
        };
        d.sym(Rect::new(PAD - 1.0, y, 14.0, 14.0), sym, 10.0, Weight::Bold, col);
        d.text(Rect::new(PAD + 16.0, y, w - 2.0 * PAD - 16.0, 14.0), txt, font(11.0, Weight::Medium), W(0.65), Align::Left);
    }
}

// ----- Launcher ----------------------------------------------------------------

/// Display label of a launcher item.
pub fn launch_label(it: &LaunchItem) -> String {
    if !it.label.trim().is_empty() {
        return it.label.trim().to_string();
    }
    let t = it.target.trim();
    match it.kind {
        LaunchKind::App => {
            let last = t.rsplit('/').next().unwrap_or(t).trim_end_matches(".app");
            // A bundle id: the last component, capitalized.
            if !t.contains('/') && t.matches('.').count() >= 2 && !t.contains(' ') {
                let s = t.rsplit('.').next().unwrap_or(t);
                let mut c = s.chars();
                return c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
            }
            last.to_string()
        }
        LaunchKind::File | LaunchKind::Folder => t.trim_end_matches('/').rsplit('/').next().unwrap_or(t).to_string(),
        LaunchKind::Url => crate::store::url_host(t).unwrap_or(t).to_string(),
        LaunchKind::Command => t.split_whitespace().next().unwrap_or("Command").rsplit('/').next().unwrap_or("Command").to_string(),
    }
}

fn launch_symbol(it: &LaunchItem) -> &str {
    if !it.symbol.trim().is_empty() {
        return it.symbol.trim();
    }
    match it.kind {
        LaunchKind::Url => "globe",
        LaunchKind::Command => "terminal.fill",
        LaunchKind::Folder => "folder.fill",
        LaunchKind::File => "doc.fill",
        LaunchKind::App => "app.fill",
    }
}

/// What the platform uses to find an item's icon (`app:` prefix for app
/// names / bundle ids, else an expanded path).
pub fn icon_source(it: &LaunchItem) -> Option<String> {
    match it.kind {
        LaunchKind::App => Some(format!("app:{}", it.target.trim())),
        LaunchKind::File | LaunchKind::Folder => Some(super::xbar::expand_tilde(&it.target).to_string_lossy().into_owned()),
        _ => None,
    }
}

/// Launcher grid cells: (columns, rows, cell frames).
pub fn launcher_cells(w: f64, h: f64, n: usize, labels: bool, with_title: bool) -> Vec<Rect> {
    let top = if with_title { PAD + 22.0 } else { 6.0 };
    let avail_h = h - top - 6.0;
    let cell_h = if labels { 50.0 } else { 46.0 };
    let rows = ((avail_h / cell_h).floor() as usize).max(1);
    let cols = (((w - 12.0) / 56.0).floor() as usize).max(1);
    let n = n.min(rows * cols);
    let used_rows = n.div_ceil(cols).max(1);
    let cw = (w - 12.0) / cols as f64;
    let y0 = top + (avail_h - used_rows as f64 * cell_h) / 2.0;
    (0..n).map(|i| Rect::new(6.0 + (i % cols) as f64 * cw, y0 + (i / cols) as f64 * cell_h, cw, cell_h)).collect()
}

pub fn launcher(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let KindSettings::Launcher(s) = c.settings else { return };
    let with_title = !s.title.trim().is_empty();
    if with_title {
        header(d, "square.grid.2x2.fill", TEAL, &s.title, w);
    }
    if s.items.is_empty() {
        return message_at(d, ctx, w, 30.0, "square.grid.2x2", "Add apps, files and links in Settings", None);
    }
    let cells = launcher_cells(w, h, s.items.len(), s.labels, with_title);
    for (i, cell) in cells.iter().enumerate() {
        let it = &s.items[i];
        let act = Act::Launch(i);
        let hot = hovered(ctx, &act);
        let pressed = c.ui.pressed.is_some_and(|(k, t)| k == i && ctx.mono_now - t < 0.25);
        if hot {
            d.rect(cell.inflate(-2.0, -1.0), 10.0, W(0.08));
        }
        let size = if s.labels { 32.0 } else { 36.0 } - if pressed { 4.0 } else { 0.0 };
        let icon = Rect::new(cell.center_x() - size / 2.0, cell.y + if s.labels { 4.0 } else { (cell.h - size) / 2.0 } + if pressed { 2.0 } else { 0.0 }, size, size);
        match icon_source(it) {
            Some(src) => {
                let key = ImageKey::Icon(hash_bytes(src.as_bytes()));
                d.image(icon, key, ImageSource::Icon(src), 0.0);
            }
            None => {
                let bg = if it.kind == LaunchKind::Url { BLUE } else { Rgba(0.22, 0.22, 0.24, 1.0) };
                let ir = icon.inflate(-2.0, -2.0);
                d.p(Prim::Rect { r: ir, radius: ir.w * 0.24, fill: bg, stroke: Some((W(0.12), 0.5)) });
                d.sym(ir, launch_symbol(it), ir.w * 0.46, Weight::Semibold, W(0.95));
            }
        }
        if s.labels {
            d.text(Rect::new(cell.x + 1.0, cell.y + 37.0, cell.w - 2.0, 13.0), launch_label(it), font(10.0, Weight::Medium), W(if hot { 0.95 } else { 0.72 }), Align::Center);
        }
        d.hit(*cell, act);
    }
}

// ----- Bluetooth -------------------------------------------------------------------

/// One ring of the Bluetooth card.
#[derive(Clone, Debug, PartialEq)]
pub struct BtEntry {
    pub symbol: &'static str,
    pub label: String,
    /// `None`: connected, but the device doesn't report a battery.
    pub level: Option<u8>,
}

/// Devices → rings: AirPods get one ring for the buds (the lower one) and
/// one for the case; a device without a battery reading gets an empty ring.
pub fn bt_entries(devices: &[BtDevice], only: &[String]) -> Vec<BtEntry> {
    let mut out = vec![];
    for dev in devices {
        if !only.is_empty() && !only.iter().any(|o| !o.trim().is_empty() && dev.name.to_lowercase().contains(&o.trim().to_lowercase())) {
            continue;
        }
        let short = short_name(&dev.name);
        if dev.levels.is_empty() {
            out.push(BtEntry { symbol: dev.kind.symbol(), label: short, level: None });
            continue;
        }
        let buds: Vec<u8> = dev.levels.iter().filter(|(p, _)| matches!(p, Part::Left | Part::Right)).map(|(_, l)| *l).collect();
        if let Some(m) = buds.iter().min() {
            out.push(BtEntry { symbol: dev.kind.symbol(), label: short.clone(), level: Some(*m) });
        }
        if let Some((_, l)) = dev.levels.iter().find(|(p, _)| *p == Part::Main) {
            out.push(BtEntry { symbol: dev.kind.symbol(), label: short.clone(), level: Some(*l) });
        }
        if let Some((_, l)) = dev.levels.iter().find(|(p, _)| *p == Part::Case) {
            let sym = if dev.kind == BtKind::AirPodsPro { "airpodspro.chargingcase.wireless.fill" } else { "airpods.chargingcase.fill" };
            out.push(BtEntry { symbol: sym, label: "Case".into(), level: Some(*l) });
        }
    }
    out
}

/// "Mitch's AirPods Pro" → "AirPods Pro", "Magic Keyboard" → "Keyboard".
pub fn short_name(n: &str) -> String {
    let mut s = n;
    for sep in ["\u{2019}s ", "'s "] {
        if let Some((_, rest)) = s.split_once(sep) {
            if !rest.is_empty() {
                s = rest;
                break;
            }
        }
    }
    s.strip_prefix("Magic ").filter(|r| !r.is_empty()).unwrap_or(s).to_string()
}

pub fn bluetooth(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let only = match c.settings {
        KindSettings::Bluetooth(s) => s.devices.as_slice(),
        _ => &[],
    };
    let Some(devs) = ctx.data.bluetooth.as_ref() else {
        return message_at(d, ctx, w, 30.0, "dot.radiowaves.left.and.right", "Looking for devices…", None);
    };
    let entries = bt_entries(devs, only);
    if entries.is_empty() {
        return message_at(d, ctx, w, 28.0, "dot.radiowaves.left.and.right", "No Bluetooth device is connected", Some(("Bluetooth Settings", Act::OpenUrl("x-apple.systempreferences:com.apple.BluetoothSettings".into()))));
    }
    let cols = ((w / 66.0).floor() as usize).clamp(1, 8);
    let n = entries.len().min(cols);
    let pitch = (w - 2.0 * 6.0) / cols as f64;
    let x0 = w / 2.0 - pitch * n as f64 / 2.0;
    let r = 21.0;
    for (i, e) in entries.iter().take(n).enumerate() {
        let cx = x0 + pitch * (i as f64 + 0.5);
        let cc = Point::new(cx, PAD + r + 6.0);
        let (frac, col, text) = match e.level {
            Some(l) => (l as f64 / 100.0, if bluetooth::level_is_low(l) { RED } else { GREEN }, format!("{l}%")),
            // No battery reading: just the track.
            None => (0.0, GREEN, "Connected".to_string()),
        };
        d.p(Prim::Ring { c: cc, radius: r, width: 4.5, frac, color: col, track: W(0.12) });
        d.sym(Rect::new(cc.x - 14.0, cc.y - 11.0, 28.0, 22.0), e.symbol, 15.0, Weight::Medium, W(0.92));
        let size = if e.level.is_some() { 13.0 } else { 10.5 };
        d.text(Rect::new(cx - pitch / 2.0, cc.y + r + 7.0, pitch, 16.0), text, rounded(size, Weight::Semibold), W(if e.level.is_some() { 0.95 } else { 0.7 }), Align::Center);
        d.text(Rect::new(cx - pitch / 2.0 + 2.0, cc.y + r + 23.0, pitch - 4.0, 13.0), e.label.clone(), font(10.0, Weight::Medium), W(0.5), Align::Center);
    }
    let _ = h;
}

// ----- Sound -------------------------------------------------------------------------

/// Volume slider geometry: (track rect, x0, x1) for a card `w` wide.
pub fn sound_track(w: f64, y: f64) -> Rect {
    Rect::new(PAD + 24.0, y, w - 2.0 * PAD - 48.0, 6.0)
}

pub fn sound(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let show_devices = !matches!(c.settings, KindSettings::Sound(s) if !s.show_devices);
    let st = &ctx.data.sound;
    if !st.available {
        return message(d, ctx, w, "speaker.slash", "Sound control isn't available", None);
    }
    let cur = st.current_device();
    let name = cur.map(|d| d.name.clone()).unwrap_or_else(|| "No output".into());
    let sym = cur.map(|d| d.symbol().to_string()).unwrap_or_else(|| "speaker.slash".into());
    let top = if show_devices { PAD } else { PAD + 12.0 };
    d.sym(Rect::new(PAD - 2.0, top, 22.0, 20.0), &sym, 14.0, Weight::Semibold, ctx.accent.with_alpha(1.0));
    d.text(Rect::new(PAD + 24.0, top + 1.0, w - 2.0 * PAD - 60.0, 18.0), name, font(13.0, Weight::Semibold), W(0.95), Align::Left);
    // Mute.
    let mr = Rect::new(w - PAD - 28.0, top - 4.0, 28.0, 28.0);
    let hot = hovered(ctx, &Act::SoundMute);
    d.rect(mr, 14.0, if st.muted { RED.with_alpha(if hot { 0.4 } else { 0.28 }) } else { W(if hot { 0.18 } else { 0.1 }) });
    d.sym(mr, st.level_symbol(), 11.5, Weight::Semibold, if st.muted { RED } else { W(0.9) });
    d.hit(mr, Act::SoundMute);
    // Volume.
    let ty = if show_devices { PAD + 36.0 } else { h / 2.0 + 8.0 };
    let track = sound_track(w, ty);
    let v = st.volume.unwrap_or(0.0).clamp(0.0, 1.0) as f64;
    d.sym(Rect::new(PAD - 2.0, ty - 6.0, 20.0, 18.0), "speaker.fill", 9.5, Weight::Semibold, W(0.45));
    d.sym(Rect::new(w - PAD - 20.0, ty - 6.0, 22.0, 18.0), "speaker.wave.3.fill", 9.5, Weight::Semibold, W(0.45));
    d.rect(track, 3.0, W(0.14));
    let fill_c = if st.muted { W(0.3) } else { W(0.92) };
    if st.volume.is_some() {
        d.rect(Rect::new(track.x, track.y, (track.w * v).max(6.0), track.h), 3.0, fill_c);
        let kx = track.x + track.w * v;
        let knob = Rect::new(kx - 8.0, track.center_y() - 8.0, 16.0, 16.0);
        d.p(Prim::Rect { r: knob, radius: 8.0, fill: W(if st.muted { 0.6 } else { 1.0 }), stroke: Some((Rgba(0.0, 0.0, 0.0, 0.25), 0.5)) });
    }
    d.hit(Rect::new(track.x - 8.0, track.y - 10.0, track.w + 16.0, track.h + 20.0), Act::SoundSlider(track.x, track.right()));
    if !show_devices {
        return;
    }
    // Device chips.
    let y = h - PAD - 26.0;
    let mut x = PAD;
    let mut shown = 0;
    for (i, dev) in st.devices.iter().enumerate() {
        let label = short_name(&dev.name);
        let cw = (est_width(&label, 11.0).min(84.0) + 32.0).min(w - 2.0 * PAD);
        if x + cw > w - PAD + 0.01 {
            break;
        }
        let r = Rect::new(x, y, cw, 26.0);
        let act = Act::SoundDevice(i);
        let on = st.current.as_deref() == Some(dev.id.as_str());
        let fill = if on {
            ctx.accent.with_alpha(0.9)
        } else if hovered(ctx, &act) {
            W(0.18)
        } else {
            W(0.09)
        };
        d.rect(r, 13.0, fill);
        d.sym(Rect::new(r.x + 6.0, r.y, 18.0, r.h), dev.symbol(), 10.0, Weight::Semibold, W(if on { 1.0 } else { 0.75 }));
        d.text(Rect::new(r.x + 24.0, r.y, r.w - 30.0, r.h), label, font(11.0, Weight::Semibold), W(if on { 1.0 } else { 0.8 }), Align::Left);
        d.hit(r, act);
        x += cw + 6.0;
        shown += 1;
    }
    if shown < st.devices.len() {
        d.text(Rect::new(w - PAD - 30.0, y, 30.0, 26.0), format!("+{}", st.devices.len() - shown), font(10.5, Weight::Semibold), W(0.4), Align::Right);
    }
}

// ----- Reminders ---------------------------------------------------------------------

/// Seconds a ticked reminder stays visible (checked) before it goes.
pub const DONE_LINGER: f64 = 0.6;

pub fn reminders(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let KindSettings::Reminders(s) = c.settings else { return };
    let st = &ctx.data.reminders;
    header(d, "checklist", ORANGE, "Reminders", w);
    d.hit(Rect::new(0.0, 0.0, w - 50.0, PAD + 20.0), Act::OpenReminders);
    match st.access {
        Access::Granted => {}
        Access::Requesting => return message(d, ctx, w, "lock.fill", "Waiting for permission…", None),
        Access::Denied => return message(d, ctx, w, "lock.fill", "No access to your reminders", Some(("Open Settings", Act::OpenPrivacy("Reminders")))),
        Access::MissingUsageKey | Access::Unavailable => return message(d, ctx, w, "checklist", "Reminders isn't available in this build", None),
        Access::NotDetermined | Access::Unknown => return message(d, ctx, w, "checklist", "Show your reminders here", Some(("Allow Access", Act::RemindersAllow))),
    }
    let gone: Vec<String> = c.ui.done.iter().filter(|(_, t)| ctx.mono_now - t >= DONE_LINGER).map(|(id, _)| id.clone()).collect();
    let items = reminders::visible(&st.items, s, ctx.unix_now, ctx.utc_offset, &gone);
    if !items.is_empty() {
        let n = items.len().to_string();
        let bw = est_width(&n, 11.0) + 14.0;
        let br = Rect::new(w - PAD - bw, PAD, bw, 16.0);
        d.rect(br, 8.0, W(0.12));
        d.text(br, n, font(10.5, Weight::Bold), W(0.8), Align::Center);
    }
    if items.is_empty() {
        d.sym(Rect::new(0.0, 42.0, w, 24.0), "checkmark.circle", 19.0, Weight::Regular, W(0.4));
        d.text(Rect::new(PAD, 70.0, w - 2.0 * PAD, 16.0), "All done", font(11.5, Weight::Medium), W(0.5), Align::Center);
        return;
    }
    let row_h = 21.0;
    let area = Rect::new(PAD - 4.0, PAD + 22.0, w - 2.0 * PAD + 8.0, h - PAD - 22.0 - 4.0);
    let heights = vec![row_h; items.len()];
    let (first, max) = list_window(&heights, area.h, c.ui.scroll * row_h / ROW_H);
    d.scroll = Some((area, max * ROW_H / row_h));
    let rows = ((area.h / row_h).floor() as usize).max(1);
    for (k, r) in items.iter().skip(first).take(rows).enumerate() {
        let y = area.y + k as f64 * row_h;
        let ticking = c.ui.done.iter().any(|(id, _)| *id == r.id);
        let circle = Rect::new(PAD, y + (row_h - 14.0) / 2.0, 14.0, 14.0);
        let tick = Act::ReminderDone(r.id.clone());
        let hot = hovered(ctx, &tick);
        if ticking {
            d.rect(circle, 7.0, r.color.with_alpha(1.0));
            d.sym(circle, "checkmark", 8.0, Weight::Bold, W(1.0));
        } else {
            d.p(Prim::Rect { r: circle, radius: 7.0, fill: if hot { r.color.with_alpha(0.3) } else { Rgba(0.0, 0.0, 0.0, 0.0) }, stroke: Some((r.color.with_alpha(0.95), 1.4)) });
        }
        d.hit(Rect::new(PAD - 5.0, y, 24.0, row_h), tick);
        let (label, urg) = reminders::due_label(r, ctx.unix_now, ctx.utc_offset);
        let lw = if label.is_empty() { 0.0 } else { est_width(&label, 11.0) + 6.0 };
        let lc = match urg {
            Urgency::Overdue => RED,
            Urgency::Today => ORANGE,
            _ => W(0.45),
        };
        if lw > 0.0 {
            d.text(Rect::new(w - PAD - lw, y, lw, row_h), label, font(11.0, Weight::Medium), lc, Align::Right);
        }
        let row = Rect::new(PAD + 20.0, y, w - 2.0 * PAD - 20.0 - lw - 4.0, row_h);
        d.text(row, r.title.clone(), font(12.5, Weight::Medium), W(if ticking { 0.4 } else { 0.92 }), Align::Left);
        let open = Act::OpenReminders;
        d.hit(row, open);
    }
    if first + rows < items.len() {
        d.sym(Rect::new(w - PAD - 10.0, h - 11.0, 12.0, 9.0), "chevron.down", 7.5, Weight::Bold, W(0.35));
    }
}

// ----- World clocks ---------------------------------------------------------------

pub fn world_clock(d: &mut CardDraw, c: &Card, w: f64, h: f64, ctx: &Ctx) {
    let KindSettings::WorldClock(s) = c.settings else { return };
    if s.clocks.is_empty() {
        return message(d, ctx, w, "globe", "Add time zones in Settings", None);
    }
    let h24 = clocks::use_24h(s.hours, ctx.hour24);
    let faces: Vec<Result<clocks::Face, String>> = s
        .clocks
        .iter()
        .map(|e| clocks::face(e, ctx.unix_now, ctx.utc_offset, h24, s.seconds && s.style == ClockStyle::Digital).ok_or_else(|| if e.label.is_empty() { e.zone.clone() } else { e.label.clone() }))
        .collect();
    match s.style {
        ClockStyle::Analog => analog_clocks(d, &faces, w, h, ctx, s.seconds),
        ClockStyle::Digital => digital_clocks(d, &faces, w, h),
    }
}

const NIGHT: Rgba = Rgba(0.62, 0.66, 1.0, 1.0);

fn day_symbol(is_day: bool) -> (&'static str, Rgba) {
    if is_day {
        ("sun.max.fill", YELLOW)
    } else {
        ("moon.fill", NIGHT)
    }
}

fn digital_clocks(d: &mut CardDraw, faces: &[Result<clocks::Face, String>], w: f64, h: f64) {
    if w <= SMALL_W + 1.0 && faces.len() >= 2 {
        // Two compact rows: label + offset, then the time.
        for (i, f) in faces.iter().take(2).enumerate() {
            let y = PAD - 2.0 + i as f64 * 47.0;
            if i == 1 {
                d.rect(Rect::new(PAD, y - 4.5, w - 2.0 * PAD, 0.5), 0.25, W(0.12));
            }
            match f {
                Ok(f) => {
                    let (sym, col) = day_symbol(f.is_day);
                    d.sym(Rect::new(PAD - 2.0, y + 1.0, 14.0, 14.0), sym, 9.5, Weight::Semibold, col);
                    let off = clocks::diff_label(f.diff);
                    let ow = est_width(&off, 10.0) + 4.0;
                    d.text(Rect::new(PAD + 13.0, y, w - 2.0 * PAD - 13.0 - ow, 15.0), f.label.clone(), font(11.0, Weight::Semibold), W(0.7), Align::Left);
                    d.text(Rect::new(w - PAD - ow, y, ow, 15.0), off, font(10.0, Weight::Medium), W(0.42), Align::Right);
                    let size = 22.0;
                    let tw = est_value_width(&f.time, size);
                    d.text(Rect::new(PAD - 1.0, y + 15.0, tw + 6.0, 27.0), f.time.clone(), rounded(size, Weight::Semibold), W(0.97), Align::Left);
                    if let Some(ap) = f.ampm {
                        d.text(Rect::new(PAD + tw + 4.0, y + 23.0, 26.0, 14.0), ap, font(10.5, Weight::Semibold), W(0.5), Align::Left);
                    }
                }
                Err(z) => bad_zone(d, Rect::new(PAD, y, w - 2.0 * PAD, 40.0), z),
            }
        }
        return;
    }
    let max_cols = ((w / 88.0).floor() as usize).max(1);
    let n = faces.len().min(max_cols);
    let cw = (w - 2.0 * PAD) / n as f64;
    for (i, f) in faces.iter().take(n).enumerate() {
        let x = PAD + i as f64 * cw;
        if i > 0 {
            d.rect(Rect::new(x - 0.25, PAD + 4.0, 0.5, h - 2.0 * PAD - 8.0), 0.25, W(0.1));
        }
        let ix = x + if i > 0 { 9.0 } else { 0.0 };
        let iw = cw - if i > 0 { 9.0 } else { 0.0 };
        match f {
            Ok(f) => {
                let (sym, col) = day_symbol(f.is_day);
                d.sym(Rect::new(ix - 2.0, PAD + 1.0, 15.0, 15.0), sym, 10.5, Weight::Semibold, col);
                d.text(Rect::new(ix + 15.0, PAD, iw - 15.0, 16.0), f.label.clone(), font(11.5, Weight::Semibold), W(0.72), Align::Left);
                let ap_w = if f.ampm.is_some() { 24.0 } else { 0.0 };
                let max = if n == 1 { 40.0 } else { 30.0 };
                let size = fit_size(&f.time, iw - ap_w - 4.0, max, 16.0);
                let tw = est_value_width(&f.time, size);
                let ty = PAD + 20.0 + (max - size) * 0.5;
                d.text(Rect::new(ix - 1.0, ty, tw + 6.0, size + 8.0), f.time.clone(), rounded(size, Weight::Semibold), W(0.97), Align::Left);
                if let Some(ap) = f.ampm {
                    d.text(Rect::new(ix + tw + 3.0, ty + size * 0.5, ap_w, 14.0), ap, font(10.5, Weight::Semibold), W(0.5), Align::Left);
                }
                let long = clocks::caption(f);
                let cap = if est_width(&long, 10.0) <= iw { long } else { format!("{} \u{00b7} {}", clocks::diff_label(f.diff), match f.day_delta { 0 => "Today", d if d > 0 => "Tmrw", _ => "Yday" }) };
                d.text(Rect::new(ix, h - PAD - 14.0, iw, 13.0), cap, font(10.0, Weight::Medium), W(0.42), Align::Left);
            }
            Err(z) => bad_zone(d, Rect::new(ix, PAD, iw, h - 2.0 * PAD), z),
        }
    }
}

fn bad_zone(d: &mut CardDraw, r: Rect, z: &str) {
    d.sym(Rect::new(r.x, r.y, 16.0, 16.0), "exclamationmark.triangle.fill", 10.0, Weight::Bold, ORANGE);
    d.wrap(Rect::new(r.x + 18.0, r.y, r.w - 18.0, r.h.min(40.0)), format!("Unknown time zone \u{201c}{z}\u{201d}"), font(10.5, Weight::Medium), W(0.55), Align::Left);
}

fn analog_clocks(d: &mut CardDraw, faces: &[Result<clocks::Face, String>], w: f64, h: f64, ctx: &Ctx, seconds: bool) {
    let max_cols = ((w / 64.0).floor() as usize).max(1);
    let n = faces.len().min(max_cols);
    let pitch = (w - 8.0) / max_cols.min(faces.len().max(1)) as f64;
    let x0 = w / 2.0 - pitch * n as f64 / 2.0;
    let r = (pitch / 2.0 - 7.0).clamp(16.0, 26.0);
    for (i, f) in faces.iter().take(n).enumerate() {
        let cx = x0 + pitch * (i as f64 + 0.5);
        let cc = Point::new(cx, PAD + 2.0 + r);
        let Ok(f) = f else {
            d.p(Prim::Rect { r: Rect::new(cc.x - r, cc.y - r, 2.0 * r, 2.0 * r), radius: r, fill: W(0.06), stroke: Some((W(0.15), 1.0)) });
            d.sym(Rect::new(cc.x - 10.0, cc.y - 9.0, 20.0, 18.0), "questionmark", 13.0, Weight::Semibold, ORANGE);
            if let Err(z) = f {
                d.text(Rect::new(cx - pitch / 2.0 + 2.0, cc.y + r + 8.0, pitch - 4.0, 14.0), z.clone(), font(10.5, Weight::Semibold), W(0.55), Align::Center);
            }
            continue;
        };
        let (face, tick, hand) = if f.is_day { (W(0.94), Rgba(0.0, 0.0, 0.0, 0.45), Rgba(0.08, 0.08, 0.09, 1.0)) } else { (Rgba(0.16, 0.16, 0.18, 1.0), W(0.4), W(0.95)) };
        d.p(Prim::Rect { r: Rect::new(cc.x - r, cc.y - r, 2.0 * r, 2.0 * r), radius: r, fill: face, stroke: Some((W(0.14), 0.5)) });
        for k in 0..12 {
            let a = k as f64 / 12.0 * std::f64::consts::TAU;
            let major = k % 3 == 0;
            let (r0, r1) = (r - if major { 6.0 } else { 4.5 }, r - 2.5);
            let p = |rr: f64| Point::new(cc.x + rr * a.sin(), cc.y - rr * a.cos());
            d.p(Prim::Line { pts: vec![p(r0), p(r1)], color: tick, width: if major { 1.6 } else { 1.0 }, fill: None });
        }
        let secs = f.second as f64 + if seconds { (ctx.unix_now.fract()).max(0.0) } else { 0.0 };
        let (ha, ma, sa) = clocks::hand_angles(f.hour, f.minute, secs);
        let hand_to = |a: f64, len: f64| vec![Point::new(cc.x - 0.12 * len * a.sin(), cc.y + 0.12 * len * a.cos()), Point::new(cc.x + len * a.sin(), cc.y - len * a.cos())];
        d.p(Prim::Line { pts: hand_to(ha, r * 0.52), color: hand, width: 2.6, fill: None });
        d.p(Prim::Line { pts: hand_to(ma, r * 0.8), color: hand, width: 1.8, fill: None });
        if seconds {
            d.p(Prim::Line { pts: hand_to(sa, r * 0.86), color: ORANGE, width: 0.9, fill: None });
        }
        d.rect(Rect::new(cc.x - 2.2, cc.y - 2.2, 4.4, 4.4), 2.2, if seconds { ORANGE } else { hand });
        let ly = cc.y + r + 6.0;
        d.text(Rect::new(cx - pitch / 2.0 + 2.0, ly, pitch - 4.0, 14.0), f.label.clone(), font(11.0, Weight::Semibold), W(0.85), Align::Center);
        if ly + 28.0 <= h - 1.0 {
            let cap = format!("{}{}", f.time, f.ampm.map(|a| format!(" {a}")).unwrap_or_default());
            d.text(Rect::new(cx - pitch / 2.0 + 2.0, ly + 14.0, pitch - 4.0, 13.0), cap, mono(10.0, Weight::Medium), W(0.45), Align::Center);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::catalog::{self, ClockEntry, LauncherSettings, ShortcutsSettings, WorldClockSettings};
    use crate::widgets::content::{parse_output, Status};
    use crate::widgets::draw::tests::{draw, inside, settings, texts, Fixture};
    use crate::widgets::instance::WidgetSize;
    use crate::widgets::layout::CARD_H;
    use crate::widgets::sound::{AudioOutput, SoundState};
    use crate::widgets::{InstanceData, ShortcutsState};
    use serde_json::json;

    fn sizes(kind: WidgetKind) -> Vec<f64> {
        catalog::kind_info(kind).sizes.iter().map(|s| s.width()).collect()
    }

    fn with_content(f: &mut Fixture, r: Result<Content, String>) {
        f.data.instances.insert("x".into(), InstanceData::Content(Status::Loading.next(r, 0.0)));
    }

    #[test]
    fn custom_card_states() {
        let mut f = Fixture::new();
        let s = KindSettings::parse(WidgetKind::Script, &json!({"command": "x", "title": "Disk"}));
        for w in sizes(WidgetKind::Script) {
            // Loading.
            let d = draw(WidgetKind::Script, &s, &f.ui, w, &f.ctx(None));
            assert!(texts(&d).contains(&"Loading…".to_string()));
            inside(&d, w, CARD_H);
        }
        // Error without data: a retry button.
        f.data.instances.insert("x".into(), InstanceData::Content(Status::Loading.next(Err("zsh: command not found: dfx".into()), 0.0)));
        let d = draw(WidgetKind::Script, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(d.hits.iter().any(|h| h.act == Act::Refresh));
        assert!(texts(&d).iter().any(|t| t.contains("command not found")));
        // Number with a gauge.
        with_content(&mut f, parse_output("73%\nof 1 TB", catalog::OutputFormat::Auto));
        for w in sizes(WidgetKind::Script) {
            let d = draw(WidgetKind::Script, &s, &f.ui, w, &f.ctx(None));
            inside(&d, w, CARD_H);
            let t = texts(&d);
            assert!(t.contains(&"73".to_string()) && t.contains(&"%".to_string()), "{t:?}");
            assert!(t.contains(&"of 1 TB".to_string()));
        }
        let d = draw(WidgetKind::Script, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(d.prims.iter().any(|p| matches!(p, Prim::Ring { frac, .. } if (*frac - 0.73).abs() < 1e-9)));
        // Stale data with an error: the data and a badge.
        let st = Status::Loading.next(parse_output("73%", catalog::OutputFormat::Auto), 0.0).next(Err("Timed out".into()), 1.0);
        f.data.instances.insert("x".into(), InstanceData::Content(st));
        let d = draw(WidgetKind::Script, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(texts(&d).contains(&"Timed out".to_string()));
        assert!(d.prims.iter().any(|p| matches!(p, Prim::Symbol { name, .. } if name == "exclamationmark.triangle.fill")));
        // Text and lists.
        with_content(&mut f, parse_output("Build passed\nmain · 3 min ago", catalog::OutputFormat::Text));
        let plain = KindSettings::parse(WidgetKind::Script, &json!({"command": "x"}));
        let d = draw(WidgetKind::Script, &plain, &f.ui, 136.0, &f.ctx(None));
        assert_eq!(&texts(&d)[..2], ["Build passed", "main · 3 min ago"]);
        let many: String = (0..30).map(|i| format!("item {i}\n")).collect();
        with_content(&mut f, parse_output(&many, catalog::OutputFormat::List));
        for w in sizes(WidgetKind::Script) {
            let d = draw(WidgetKind::Script, &plain, &f.ui, w, &f.ctx(None));
            inside(&d, w, CARD_H);
            let (_, max) = d.scroll.unwrap();
            assert!(max > 0.0);
        }
        f.ui.scroll = 1e6;
        let d = draw(WidgetKind::Script, &plain, &f.ui, 280.0, &f.ctx(None));
        assert!(texts(&d).contains(&"item 29".to_string()));
        assert!(!texts(&d).contains(&"item 0".to_string()));
    }

    #[test]
    fn xbar_card_items_are_clickable() {
        let mut f = Fixture::new();
        let out = "☀️ 21° | color=orange\n---\nOpen | href=https://a.b\nPlain\n---\nRun | bash=/bin/echo terminal=false";
        with_content(&mut f, Ok(crate::widgets::xbar::to_content(&crate::widgets::xbar::parse(out))));
        let s = settings(WidgetKind::Xbar);
        for w in sizes(WidgetKind::Xbar) {
            let d = draw(WidgetKind::Xbar, &s, &f.ui, w, &f.ctx(None));
            inside(&d, w, CARD_H);
            let items: Vec<&Act> = d.hits.iter().filter(|h| matches!(h.act, Act::Item(_))).map(|h| &h.act).collect();
            assert_eq!(items, vec![&Act::Item(0), &Act::Item(3)], "plain rows and separators aren't clickable");
            assert_eq!(texts(&d)[0], "☀️ 21°");
        }
        // Title cycling.
        with_content(&mut f, Ok(crate::widgets::xbar::to_content(&crate::widgets::xbar::parse("A\nB\nC"))));
        let mut ctx = f.ctx(None);
        ctx.mono_now = 4.5;
        assert_eq!(texts(&draw(WidgetKind::Xbar, &s, &f.ui, 280.0, &ctx))[0], "B");
    }

    #[test]
    fn web_card_and_live_activity() {
        let mut f = Fixture::new();
        let ws = catalog::WebSettings { title: "BTC".into(), template: "{{p}}".into(), value_path: "pct".into(), live: true, open_url: "https://x.y".into(), ..Default::default() };
        let c = crate::widgets::web::to_content(r#"{"p":"$67,212","pct":40}"#, &ws, 0.0).unwrap();
        with_content(&mut f, Ok(c));
        let s = KindSettings::Web(ws);
        let d = draw(WidgetKind::Web, &s, &f.ui, 280.0, &f.ctx(None));
        assert_eq!(d.hits[0].act, Act::CardAction);
        assert!(texts(&d).contains(&"$67,212".to_string()));
        let ui = Default::default();
        let card = Card { id: "x", kind: WidgetKind::Web, size: WidgetSize::Medium, settings: &s, ui: &ui };
        assert_eq!(live_custom(&card, &f.ctx(None)), Some(Live::Custom { symbol: "network".into(), color: BLUE, text: "$67,212".into(), progress: Some(0.4) }));
        assert!(live_activity(&f.ctx(None), &[card]).is_some());
    }

    #[test]
    fn list_windowing() {
        let hs = vec![ROW_H; 10];
        assert_eq!(list_window(&hs, 60.0, 0.0), (0, 7.0 * ROW_H));
        assert_eq!(list_window(&hs, 60.0, 2.4 * ROW_H), (2, 7.0 * ROW_H));
        assert_eq!(list_window(&hs, 60.0, 1e9), (7, 7.0 * ROW_H));
        assert_eq!(list_window(&hs, 1000.0, 50.0), (0, 0.0));
        assert_eq!(list_window(&[], 50.0, 10.0), (0, 0.0));
    }

    #[test]
    fn shortcuts_card() {
        let mut f = Fixture::new();
        let s = KindSettings::Shortcuts(ShortcutsSettings { shortcuts: vec!["Log Water".into(), "Focus 25".into(), "Toggle VPN".into(), "Very Long Shortcut Name Here".into()], show_output: true, title: "Shortcuts".into() });
        f.data.instances.insert("x".into(), InstanceData::Shortcuts(ShortcutsState { running: Some("Focus 25".into()), last: Some(("Log Water".into(), Ok("250 ml logged".into()), 0.0)) }));
        for w in sizes(WidgetKind::Shortcuts) {
            let d = draw(WidgetKind::Shortcuts, &s, &f.ui, w, &f.ctx(None));
            inside(&d, w, CARD_H);
            assert!(d.hits.iter().any(|h| h.act == Act::Shortcut(0)));
            assert!(texts(&d).contains(&"250 ml logged".to_string()));
        }
        let d = draw(WidgetKind::Shortcuts, &s, &f.ui, 424.0, &f.ctx(None));
        assert!(d.prims.iter().any(|p| matches!(p, Prim::Symbol { name, .. } if name == "hourglass")));
        let empty = settings(WidgetKind::Shortcuts);
        assert!(texts(&draw(WidgetKind::Shortcuts, &empty, &f.ui, 280.0, &f.ctx(None))).contains(&"Add shortcuts in Settings".to_string()));
        let pills = flow_pills(&["a".into(), "b".into(), "c".into()], Rect::new(0.0, 0.0, 120.0, 60.0), 25.0, 6.0, false);
        assert_eq!(pills.len(), 3);
        assert!(pills[2].y > pills[0].y);
    }

    #[test]
    fn launcher_card() {
        let f = Fixture::new();
        let items = vec![
            LaunchItem { kind: LaunchKind::App, target: "Safari".into(), ..Default::default() },
            LaunchItem { kind: LaunchKind::Folder, target: "~/Downloads".into(), ..Default::default() },
            LaunchItem { kind: LaunchKind::Url, target: "https://github.com/notifications".into(), ..Default::default() },
            LaunchItem { kind: LaunchKind::Command, target: "open -a Terminal ~".into(), label: "Shell".into(), symbol: "terminal".into() },
            LaunchItem { kind: LaunchKind::App, target: "com.apple.Music".into(), ..Default::default() },
        ];
        assert_eq!(items.iter().map(launch_label).collect::<Vec<_>>(), vec!["Safari", "Downloads", "github.com", "Shell", "Music"]);
        let s = KindSettings::Launcher(LauncherSettings { items: items.clone(), labels: true, title: String::new() });
        for w in sizes(WidgetKind::Launcher) {
            let d = draw(WidgetKind::Launcher, &s, &f.ui, w, &f.ctx(None));
            inside(&d, w, CARD_H);
            let n = d.hits.len();
            assert!((4..=5).contains(&n), "{w}: {n}");
            assert!(d.images.iter().any(|(_, s)| matches!(s, ImageSource::Icon(p) if p == "app:Safari")));
            assert!(d.images.iter().any(|(_, s)| matches!(s, ImageSource::Icon(p) if p.ends_with("/Downloads") && !p.starts_with('~'))));
        }
        let cells = launcher_cells(136.0, CARD_H, 9, true, false);
        assert_eq!(cells.len(), 4);
        assert!(texts(&draw(WidgetKind::Launcher, &settings(WidgetKind::Launcher), &f.ui, 280.0, &f.ctx(None))).iter().any(|t| t.contains("Add apps")));
    }

    #[test]
    fn bluetooth_card() {
        let mut f = Fixture::new();
        let s = settings(WidgetKind::Bluetooth);
        assert!(texts(&draw(WidgetKind::Bluetooth, &s, &f.ui, 280.0, &f.ctx(None))).contains(&"Looking for devices…".to_string()));
        f.data.bluetooth = Some(vec![]);
        let d = draw(WidgetKind::Bluetooth, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(d.hits.iter().any(|h| matches!(&h.act, Act::OpenUrl(_))));
        f.data.bluetooth = Some(vec![
            BtDevice { name: "Mitch’s AirPods Pro".into(), kind: BtKind::AirPodsPro, address: String::new(), levels: vec![(Part::Left, 90), (Part::Right, 15), (Part::Case, 60)] },
            BtDevice { name: "Magic Mouse".into(), kind: BtKind::Mouse, address: String::new(), levels: vec![(Part::Main, 71)] },
        ]);
        let e = bt_entries(f.data.bluetooth.as_ref().unwrap(), &[]);
        assert_eq!(e.iter().map(|e| (e.label.as_str(), e.level)).collect::<Vec<_>>(), vec![("AirPods Pro", Some(15)), ("Case", Some(60)), ("Mouse", Some(71))]);
        // Headphones without a battery reading still show up.
        let mut with_buds = f.data.bluetooth.clone().unwrap();
        with_buds.push(BtDevice { name: "Pixel Buds Pro 2".into(), kind: BtKind::Headphones, address: String::new(), levels: vec![] });
        let e = bt_entries(&with_buds, &[]);
        assert_eq!((e[3].label.as_str(), e[3].level), ("Pixel Buds Pro 2", None));
        let mut g = Fixture::new();
        g.data.bluetooth = Some(with_buds);
        assert!(texts(&draw(WidgetKind::Bluetooth, &s, &g.ui, 340.0, &g.ctx(None))).contains(&"Connected".to_string()));
        for w in sizes(WidgetKind::Bluetooth) {
            let d = draw(WidgetKind::Bluetooth, &s, &f.ui, w, &f.ctx(None));
            inside(&d, w, CARD_H);
            assert!(d.prims.iter().any(|p| matches!(p, Prim::Ring { color, .. } if *color == RED)));
        }
        assert_eq!(bt_entries(f.data.bluetooth.as_ref().unwrap(), &["mouse".into()]).len(), 1);
        assert_eq!(short_name("Mitch's Keyboard"), "Keyboard");
    }

    #[test]
    fn sound_card() {
        let mut f = Fixture::new();
        let s = settings(WidgetKind::Sound);
        assert!(texts(&draw(WidgetKind::Sound, &s, &f.ui, 280.0, &f.ctx(None))).contains(&"Sound control isn't available".to_string()));
        f.data.sound = SoundState { devices: vec![AudioOutput::new("a", "MacBook Pro Speakers"), AudioOutput::new("b", "AirPods Pro"), AudioOutput::new("c", "LG UltraFine")], current: Some("b".into()), volume: Some(0.6), muted: false, available: true };
        for w in sizes(WidgetKind::Sound) {
            let d = draw(WidgetKind::Sound, &s, &f.ui, w, &f.ctx(None));
            inside(&d, w, CARD_H);
            assert!(d.hits.iter().any(|h| matches!(h.act, Act::SoundSlider(..))));
            assert!(d.hits.iter().any(|h| h.act == Act::SoundMute));
            assert!(d.hits.iter().any(|h| h.act == Act::SoundDevice(1)));
            assert!(texts(&d).contains(&"AirPods Pro".to_string()));
        }
        let (x0, x1) = match draw(WidgetKind::Sound, &s, &f.ui, 280.0, &f.ctx(None)).hits.iter().find_map(|h| if let Act::SoundSlider(a, b) = h.act { Some((a, b)) } else { None }) {
            Some(v) => v,
            None => panic!(),
        };
        assert_eq!(crate::widgets::sound::slider_value((x0 + x1) / 2.0, x0, x1), 0.5);
    }

    #[test]
    fn reminders_card() {
        let mut f = Fixture::new();
        let s = settings(WidgetKind::Reminders);
        let d = draw(WidgetKind::Reminders, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(d.hits.iter().any(|h| h.act == Act::RemindersAllow));
        f.data.reminders.access = Access::Denied;
        let d = draw(WidgetKind::Reminders, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(d.hits.iter().any(|h| h.act == Act::OpenPrivacy("Reminders")));
        f.data.reminders.access = Access::Granted;
        f.data.reminders.items = reminders::demo(crate::widgets::draw::tests::NOW);
        for w in sizes(WidgetKind::Reminders) {
            let d = draw(WidgetKind::Reminders, &s, &f.ui, w, &f.ctx(None));
            inside(&d, w, CARD_H);
            assert!(d.hits.iter().any(|h| h.act == Act::ReminderDone("r1".into())));
            assert!(texts(&d).contains(&"Overdue".to_string()));
            assert!(texts(&d).contains(&"5".to_string()));
        }
        // Ticked: shown checked, then gone.
        f.ui.done = vec![("r1".into(), 99.9)];
        let d = draw(WidgetKind::Reminders, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(texts(&d).contains(&"Renew passport".to_string()));
        f.ui.done = vec![("r1".into(), 10.0)];
        let d = draw(WidgetKind::Reminders, &s, &f.ui, 280.0, &f.ctx(None));
        assert!(!texts(&d).contains(&"Renew passport".to_string()));
        f.data.reminders.items.clear();
        assert!(texts(&draw(WidgetKind::Reminders, &s, &f.ui, 280.0, &f.ctx(None))).contains(&"All done".to_string()));
    }

    #[test]
    fn world_clock_cards() {
        let f = Fixture::new();
        let zones = |n: usize| -> Vec<ClockEntry> {
            ["America/Los_Angeles", "Europe/London", "Asia/Tokyo", "Australia/Sydney", "Asia/Kolkata", "America/New_York"].iter().take(n).map(|z| ClockEntry { zone: z.to_string(), label: String::new() }).collect()
        };
        for style in [ClockStyle::Digital, ClockStyle::Analog] {
            for n in [1, 2, 3, 6] {
                let s = KindSettings::WorldClock(WorldClockSettings { clocks: zones(n), style, seconds: true, ..Default::default() });
                for w in sizes(WidgetKind::WorldClock) {
                    let d = draw(WidgetKind::WorldClock, &s, &f.ui, w, &f.ctx(None));
                    inside(&d, w, CARD_H);
                    assert!(texts(&d).iter().any(|t| t == "Los Angeles"), "{style:?} {n} {w}: {:?}", texts(&d));
                }
            }
        }
        // 10:00 UTC on Oct 26: Tokyo is 19:00 (night), LA 3:00 (night), London 10:00 (day).
        let s = KindSettings::WorldClock(WorldClockSettings { clocks: zones(3), ..Default::default() });
        let t = texts(&draw(WidgetKind::WorldClock, &s, &f.ui, 424.0, &f.ctx(None)));
        assert!(t.contains(&"19:00".to_string()) && t.contains(&"3:00".to_string()) && t.contains(&"10:00".to_string()), "{t:?}");
        let analog = KindSettings::WorldClock(WorldClockSettings { clocks: zones(2), style: ClockStyle::Analog, ..Default::default() });
        let d = draw(WidgetKind::WorldClock, &analog, &f.ui, 280.0, &f.ctx(None));
        assert_eq!(d.prims.iter().filter(|p| matches!(p, Prim::Line { .. })).count(), 2 * (12 + 2));
        let bad = KindSettings::WorldClock(WorldClockSettings { clocks: vec![ClockEntry { zone: "Mars/Base".into(), label: String::new() }], ..Default::default() });
        assert!(texts(&draw(WidgetKind::WorldClock, &bad, &f.ui, 280.0, &f.ctx(None))).iter().any(|t| t.contains("Unknown time zone")));
    }
}
