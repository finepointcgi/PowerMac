//! Windows backend: a top-center drop shelf.
//!
//! A layered, topmost, non-activating popup on its own STA thread. Collapsed
//! it is a thin, practically invisible strip at the top center of the
//! primary monitor that is an OLE drop target; dragging files / text / links
//! onto it, resting the pointer there or clicking it slides the shelf down.
//! Tiles show shell icons and names; they can be dragged out (shell data
//! object for files, `CF_UNICODETEXT` for text and links), opened
//! (double-click), shown in Explorer, copied or removed (context menu).
//!
//! Compile-checked only (`cargo check --target x86_64-pc-windows-msvc`);
//! not yet run on Windows.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::Result;
use windows::core::{implement, w, BOOL, HRESULT, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::anim::ease_out_cubic;
use crate::geometry::{self as geo, Button, Hit, Layout, Metrics, Point, Rect};
use crate::logic::{ExpandLogic, Inputs, Mode};
use crate::store::{ItemKind, NewItem, ShelfItem};
use crate::{Request, Shared};

pub fn has_notch() -> bool {
    false
}

const WM_APP_WAKE: u32 = WM_APP + 1;
const TIMER_ID: usize = 1;
/// Collapsed drop strip, in DIPs.
const STRIP_W: f64 = 260.0;
const STRIP_H: f64 = 6.0;
const SLIDE_SECS: f64 = 0.20;
const CF_TEXT_U: u16 = 13; // CF_UNICODETEXT
const CF_FILES: u16 = 15; // CF_HDROP

pub struct Backend {
    hwnd: Arc<AtomicIsize>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Backend {
    pub fn start(shared: Arc<Shared>) -> Result<Backend> {
        let hwnd = Arc::new(AtomicIsize::new(0));
        let slot = hwnd.clone();
        let t = std::thread::Builder::new().name("notch-shelf".into()).spawn(move || run(shared, slot))?;
        Ok(Backend { hwnd, thread: Mutex::new(Some(t)) })
    }

    fn post(&self, msg: u32) {
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn wake(&self) {
        self.post(WM_APP_WAKE);
    }

    pub fn stop(&self) {
        self.post(WM_CLOSE);
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if self.hwnd.load(Ordering::SeqCst) != 0 {
                let _ = t.join();
            }
        }
    }
}

struct Ui {
    shared: Arc<Shared>,
    hwnd: HWND,
    cfg: crate::Config,
    cfg_gen: u64,
    items_gen: u64,
    items: Vec<ShelfItem>,
    scale: f64,
    m: Metrics,
    monitor: Rect,
    strip: Rect,
    panel: Rect,
    layout: Layout,
    logic: ExpandLogic,
    mode: Mode,
    /// 0 = collapsed, 1 = open.
    t: f64,
    t_from: f64,
    anim_start: Instant,
    clock: Instant,
    hover: Hit,
    drop_hover: bool,
    drag_inside: bool,
    scroll: f64,
    selected: BTreeSet<u64>,
    press: Option<(Point, Hit)>,
    own_drag: bool,
    menu_open: bool,
    button_down: bool,
    icons: HashMap<u64, HICON>,
    placed: Option<(Rect, u8)>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.try_borrow_mut().ok().and_then(|mut u| u.as_mut().map(f)))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn rect_i(r: Rect) -> RECT {
    RECT { left: r.x.round() as i32, top: r.y.round() as i32, right: r.right().round() as i32, bottom: r.bottom().round() as i32 }
}

fn run(shared: Arc<Shared>, slot: Arc<AtomicIsize>) {
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if OleInitialize(None).is_err() {
            log::warn!("notch-shelf: OleInitialize failed");
            return;
        }
        let Ok(module) = GetModuleHandleW(None) else { return };
        let hinst = HINSTANCE(module.0);
        let class = w!("PowerMacNotchShelf");
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = match CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("Shelf"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(hinst),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log::warn!("notch-shelf: CreateWindowExW: {e}");
                OleUninitialize();
                return;
            }
        };
        let now = Instant::now();
        let mut ui = Ui {
            shared,
            hwnd,
            cfg: crate::Config { enabled: false, ..crate::Config::default() },
            cfg_gen: 0,
            items_gen: 0,
            items: vec![],
            scale: 1.0,
            m: Metrics::default(),
            monitor: Rect::default(),
            strip: Rect::default(),
            panel: Rect::default(),
            layout: geo::layout(&Rect::default(), &Rect::default(), 0, 0.0, &Metrics::default()),
            logic: ExpandLogic::default(),
            mode: Mode::Collapsed,
            t: 0.0,
            t_from: 0.0,
            anim_start: now,
            clock: now,
            hover: Hit::Outside,
            drop_hover: false,
            drag_inside: false,
            scroll: 0.0,
            selected: BTreeSet::new(),
            press: None,
            own_drag: false,
            menu_open: false,
            button_down: false,
            icons: HashMap::new(),
            placed: None,
        };
        ui.relayout();
        UI.with(|u| *u.borrow_mut() = Some(ui));
        let target: IDropTarget = DropTarget.into();
        if let Err(e) = RegisterDragDrop(hwnd, &target) {
            log::warn!("notch-shelf: RegisterDragDrop: {e}");
        }
        slot.store(hwnd.0 as isize, Ordering::SeqCst);
        SetTimer(Some(hwnd), TIMER_ID, 50, None);
        with(|u| u.tick());
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        slot.store(0, Ordering::SeqCst);
        if let Some(mut u) = UI.with(|u| u.borrow_mut().take()) {
            for (_, i) in u.icons.drain() {
                let _ = DestroyIcon(i);
            }
        }
        OleUninitialize();
    }
}

impl Ui {
    fn now(&self) -> f64 {
        self.clock.elapsed().as_secs_f64()
    }

    /// Geometry from the primary monitor and DPI.
    fn relayout(&mut self) {
        unsafe {
            let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let r = mi.rcMonitor;
                self.monitor = Rect::new(r.left as f64, r.top as f64, (r.right - r.left) as f64, (r.bottom - r.top) as f64);
            }
            let dpi = GetDpiForWindow(self.hwnd);
            self.scale = if dpi > 0 { dpi as f64 / 96.0 } else { 1.0 };
        }
        let s = self.scale;
        let base = self.cfg.metrics();
        self.m = Metrics {
            panel_width: base.panel_width * s,
            tile_w: base.tile_w * s,
            tile_h: base.tile_h * s,
            tile_gap: base.tile_gap * s,
            thumb: 40.0 * s,
            side_pad: base.side_pad * s,
            header_min_h: 34.0 * s,
            strip_gap: base.strip_gap * s,
            bottom_pad: base.bottom_pad * s,
            shoulder: 0.0,
            collapsed_radius: 0.0,
            expanded_radius: 16.0 * s,
            virtual_notch_w: STRIP_W * s,
            peek_wing: 0.0,
            margin: 0.0,
            button: 28.0 * s,
        };
        let mon = self.monitor;
        self.strip = Rect::new(mon.center_x() - STRIP_W * s / 2.0, mon.y, STRIP_W * s, STRIP_H * s);
        // A zero-width "notch" in the middle splits title and buttons.
        let notch = Rect::new(mon.center_x(), mon.y, 0.0, self.m.header_min_h);
        self.panel = geo::expanded_rect(&notch, &mon, &self.m);
        self.layout = geo::layout(&self.panel, &notch, self.items.len(), self.scroll, &self.m);
        self.scroll = self.layout.scroll;
        self.placed = None;
    }

    fn local(&self, r: Rect) -> Rect {
        r.offset(-self.panel.x, -self.panel.y)
    }

    fn tick(&mut self) {
        let (cfg, cfg_gen, items, items_gen, reqs, stopped) = {
            let mut st = self.shared.lock();
            let cfg = (st.config_gen != self.cfg_gen).then(|| st.config.clone());
            let items = (st.items_gen != self.items_gen).then(|| st.store.items().to_vec());
            (cfg, st.config_gen, items, st.items_gen, std::mem::take(&mut st.requests), st.stopped)
        };
        if stopped {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            return;
        }
        if let Some(c) = cfg {
            self.cfg = c;
            self.cfg_gen = cfg_gen;
            self.relayout();
        }
        if let Some(items) = items {
            self.items = items;
            self.items_gen = items_gen;
            let ids: BTreeSet<u64> = self.items.iter().map(|i| i.id).collect();
            self.selected.retain(|i| ids.contains(i));
            for id in self.icons.keys().copied().filter(|id| !ids.contains(id)).collect::<Vec<_>>() {
                if let Some(i) = self.icons.remove(&id) {
                    unsafe {
                        let _ = DestroyIcon(i);
                    }
                }
            }
            self.relayout();
            self.invalidate();
        }
        for r in reqs {
            match r {
                Request::Expand => self.logic.expand(),
                Request::Collapse => self.logic.collapse(false),
                // No Home view (widgets) on Windows yet: the shelf only.
                Request::RenderPng(_) | Request::Debug(_) | Request::SetView(_) | Request::SetPage(_) | Request::RefreshWidgets => {}
            }
        }
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let cur = Point::new(pt.x as f64, pt.y as f64);
        let btn = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
        let pressed = btn && !self.button_down;
        self.button_down = btn;
        let now = self.now();
        let enabled = self.cfg.enabled;
        let hover_zone = Rect::new(self.strip.x, self.strip.y, self.strip.w, self.strip.h + 2.0 * self.scale);
        let keep = self.panel.inflate(8.0 * self.scale, 0.0);
        let keep = Rect::new(keep.x, keep.y, keep.w, keep.h + 12.0 * self.scale);
        let in_keep = keep.contains(cur);
        let inputs = Inputs {
            now,
            in_hover_zone: enabled && hover_zone.contains(cur),
            in_keep_zone: in_keep,
            in_drag_zone: self.drag_inside,
            button_down: btn,
            external_drag: self.drag_inside,
            drag_over_panel: self.drop_hover,
            clicked_notch: enabled && pressed && hover_zone.contains(cur) && self.mode != Mode::Expanded,
            mouse_down_outside: pressed && !in_keep && self.mode == Mode::Expanded,
            pinned: self.menu_open,
        };
        let mode = if enabled { self.logic.update(&self.cfg.logic_settings(), &inputs) } else { Mode::Collapsed };
        if mode != self.mode {
            self.mode = mode;
            self.t_from = self.t;
            self.anim_start = Instant::now();
            if mode != Mode::Expanded {
                self.selected.clear();
                self.drop_hover = false;
            }
        }
        let target = if self.mode == Mode::Expanded { 1.0 } else { 0.0 };
        let k = (self.anim_start.elapsed().as_secs_f64() / SLIDE_SECS).min(1.0);
        self.t = self.t_from + (target - self.t_from) * ease_out_cubic(k);
        let animating = k < 1.0;
        let hover = if self.mode == Mode::Expanded && !animating {
            let close = match self.hover {
                Hit::Tile(i) | Hit::Close(i) => Some(i),
                _ => None,
            };
            geo::hit_test(&self.layout, cur, close, &self.m)
        } else {
            Hit::Outside
        };
        if hover != self.hover {
            self.hover = hover;
            self.invalidate();
        }
        self.place(enabled);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_ID, if animating || self.mode == Mode::Expanded { 16 } else { 50 }, None);
        }
    }

    /// Position, size and opacity for the current animation state.
    fn place(&mut self, enabled: bool) {
        let (r, alpha) = if !enabled {
            (Rect::new(self.strip.x, self.strip.y, 1.0, 1.0), 0u8)
        } else if self.t <= 0.001 {
            // Alpha 1: invisible, but still hit-testable as a drop target.
            (self.strip, 1u8)
        } else {
            let p = self.panel;
            let y = p.y - (1.0 - self.t) * p.h;
            (Rect::new(p.x, y, p.w, p.h), (40.0 + 205.0 * self.t).round() as u8)
        };
        if self.placed == Some((r, alpha)) {
            return;
        }
        let size_changed = self.placed.map(|(o, _)| o.w != r.w || o.h != r.h).unwrap_or(true);
        self.placed = Some((r, alpha));
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                r.x.round() as i32,
                r.y.round() as i32,
                r.w.round().max(1.0) as i32,
                r.h.round().max(1.0) as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = SetLayeredWindowAttributes(self.hwnd, COLORREF(0), alpha, LWA_ALPHA);
            if size_changed {
                if r.w > self.strip.w + 1.0 {
                    let rad = (self.m.expanded_radius * 2.0) as i32;
                    // Square top, rounded bottom corners.
                    let rgn = CreateRoundRectRgn(0, -rad, r.w as i32 + 1, r.h as i32 + 1, rad, rad);
                    SetWindowRgn(self.hwnd, Some(rgn), true);
                } else {
                    SetWindowRgn(self.hwnd, None, true);
                }
                self.invalidate();
            }
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn icon(&mut self, it: &ShelfItem) -> Option<HICON> {
        if let Some(i) = self.icons.get(&it.id) {
            return Some(*i);
        }
        let path = it.path()?;
        let wp = wide(&path.to_string_lossy());
        let mut sfi = SHFILEINFOW::default();
        unsafe {
            let ok = SHGetFileInfoW(PCWSTR(wp.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut sfi), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON);
            if ok == 0 || sfi.hIcon.is_invalid() {
                return None;
            }
        }
        self.icons.insert(it.id, sfi.hIcon);
        Some(sfi.hIcon)
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let mut cr = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut cr);
            let (w, h) = (cr.right - cr.left, cr.bottom - cr.top);
            let mem = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
            let old = SelectObject(mem, bmp.into());
            let bg = CreateSolidBrush(rgb(18, 18, 20));
            FillRect(mem, &cr, bg);
            let _ = DeleteObject(bg.into());
            if w as f64 > self.strip.w + 1.0 {
                self.draw_panel(mem);
            }
            let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    unsafe fn font(&self, size: f64, weight: i32, face: PCWSTR) -> HFONT {
        CreateFontW(
            -(size * self.scale).round() as i32,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            face,
        )
    }

    unsafe fn text(&self, hdc: HDC, s: &str, r: Rect, color: COLORREF, flags: DRAW_TEXT_FORMAT) {
        let mut buf: Vec<u16> = s.encode_utf16().collect();
        let mut rc = rect_i(r);
        SetTextColor(hdc, color);
        DrawTextW(hdc, &mut buf, &mut rc, flags | DT_SINGLELINE | DT_NOPREFIX);
    }

    unsafe fn round_fill(&self, hdc: HDC, r: Rect, radius: f64, color: COLORREF) {
        let b = CreateSolidBrush(color);
        let ob = SelectObject(hdc, b.into());
        let op = SelectObject(hdc, GetStockObject(NULL_PEN));
        let rc = rect_i(r);
        let d = (radius * 2.0 * self.scale) as i32;
        let _ = RoundRect(hdc, rc.left, rc.top, rc.right + 1, rc.bottom + 1, d, d);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(b.into());
    }

    unsafe fn draw_panel(&mut self, hdc: HDC) {
        SetBkMode(hdc, TRANSPARENT);
        let l = self.layout.clone();
        let ui_font = self.font(12.5, 600, w!("Segoe UI"));
        let small = self.font(11.0, 400, w!("Segoe UI"));
        let icons = self.font(14.0, 400, w!("Segoe Fluent Icons"));
        let big_icons = self.font(20.0, 400, w!("Segoe Fluent Icons"));
        let old = SelectObject(hdc, ui_font.into());
        // Header.
        let title = self.local(l.title);
        let n = self.items.len();
        let title_text = if n > 0 { format!("Shelf  ·  {n}") } else { "Shelf".to_string() };
        self.text(hdc, &title_text, title, rgb(240, 240, 242), DT_LEFT | DT_VCENTER);
        SelectObject(hdc, icons.into());
        for (b, r) in &l.buttons {
            let r = self.local(*r);
            if self.hover == Hit::Button(*b) && n > 0 {
                self.round_fill(hdc, r, 6.0, rgb(52, 52, 56));
            }
            let glyph = match b {
                Button::DragAll => "\u{E8B3}",
                Button::Share => "\u{E8C8}",
                Button::Clear => "\u{E74D}",
            };
            let c = if n > 0 { rgb(230, 230, 232) } else { rgb(90, 90, 94) };
            self.text(hdc, glyph, r, c, DT_CENTER | DT_VCENTER);
        }
        // Strip.
        let strip = self.local(l.strip);
        let saved = SaveDC(hdc);
        let sr = rect_i(strip);
        IntersectClipRect(hdc, sr.left, sr.top, sr.right, sr.bottom);
        if n == 0 {
            let border = if self.drop_hover { rgb(96, 160, 255) } else { rgb(70, 70, 76) };
            let pen = CreatePen(PS_DASH, 1, border);
            let op = SelectObject(hdc, pen.into());
            let fill = CreateSolidBrush(if self.drop_hover { rgb(24, 36, 56) } else { rgb(24, 24, 27) });
            let ob = SelectObject(hdc, fill.into());
            let d = (24.0 * self.scale) as i32;
            let _ = RoundRect(hdc, sr.left + 2, sr.top + 2, sr.right - 2, sr.bottom - 2, d, d);
            SelectObject(hdc, op);
            SelectObject(hdc, ob);
            let _ = DeleteObject(pen.into());
            let _ = DeleteObject(fill.into());
            SelectObject(hdc, ui_font.into());
            let msg = if self.drop_hover { "Release to add to the shelf" } else { "Drop files, links or text here" };
            self.text(hdc, msg, strip, if self.drop_hover { rgb(140, 190, 255) } else { rgb(150, 150, 156) }, DT_CENTER | DT_VCENTER);
        } else {
            if self.drop_hover {
                self.round_fill(hdc, strip, 14.0, rgb(24, 36, 56));
            }
            let items = self.items.clone();
            for (k, it) in items.iter().enumerate() {
                let Some(t) = l.tiles.get(k) else { continue };
                let t = self.local(*t);
                if !t.intersects(&strip) {
                    continue;
                }
                let sel = self.selected.contains(&it.id);
                let hov = matches!(self.hover, Hit::Tile(i) | Hit::Close(i) if i == k);
                if sel {
                    self.round_fill(hdc, t, 10.0, rgb(38, 64, 112));
                } else if hov {
                    self.round_fill(hdc, t, 10.0, rgb(40, 40, 44));
                }
                let parts = geo::tile_parts(&t, &self.m);
                let isz = (32.0 * self.scale).round();
                let ix = parts.thumb.center_x() - isz / 2.0;
                let iy = parts.thumb.center_y() - isz / 2.0;
                match &it.kind {
                    ItemKind::File { .. } => {
                        if let Some(icon) = self.icon(it) {
                            let _ = DrawIconEx(hdc, ix as i32, iy as i32, icon, isz as i32, isz as i32, 0, None, DI_NORMAL);
                        }
                    }
                    kind => {
                        let plate = Rect::new(parts.thumb.center_x() - 20.0 * self.scale, parts.thumb.center_y() - 20.0 * self.scale, 40.0 * self.scale, 40.0 * self.scale);
                        let (c, glyph) = if matches!(kind, ItemKind::Url { .. }) { (rgb(10, 100, 220), "\u{E71B}") } else { (rgb(58, 58, 62), "\u{E8C4}") };
                        self.round_fill(hdc, plate, 9.0, c);
                        SelectObject(hdc, big_icons.into());
                        self.text(hdc, glyph, plate, rgb(255, 255, 255), DT_CENTER | DT_VCENTER);
                    }
                }
                SelectObject(hdc, small.into());
                self.text(hdc, &it.display_name(), parts.label, rgb(225, 225, 228), DT_CENTER | DT_END_ELLIPSIS);
                if hov {
                    self.round_fill(hdc, parts.close, 9.0, if matches!(self.hover, Hit::Close(_)) { rgb(110, 110, 116) } else { rgb(76, 76, 82) });
                    SelectObject(hdc, icons.into());
                    let f = self.font(8.0, 700, w!("Segoe Fluent Icons"));
                    SelectObject(hdc, f.into());
                    self.text(hdc, "\u{E711}", parts.close, rgb(255, 255, 255), DT_CENTER | DT_VCENTER);
                    SelectObject(hdc, small.into());
                    let _ = DeleteObject(f.into());
                }
            }
        }
        let _ = RestoreDC(hdc, saved);
        SelectObject(hdc, old);
        for f in [ui_font, small, icons, big_icons] {
            let _ = DeleteObject(f.into());
        }
    }

    fn client_to_global(&self, lp: LPARAM) -> Point {
        let x = (lp.0 & 0xFFFF) as i16 as f64;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f64;
        let (r, _) = self.placed.unwrap_or((self.panel, 255));
        Point::new(r.x + x, r.y + y)
    }

    fn hit(&self, p: Point) -> Hit {
        if self.mode != Mode::Expanded || self.t < 0.99 {
            return Hit::Outside;
        }
        let close = match self.hover {
            Hit::Tile(i) | Hit::Close(i) => Some(i),
            _ => None,
        };
        geo::hit_test(&self.layout, p, close, &self.m)
    }

    fn ids_for_drag(&self, h: Hit) -> Vec<u64> {
        match h {
            Hit::Tile(k) => {
                let Some(id) = self.items.get(k).map(|i| i.id) else { return vec![] };
                if self.selected.contains(&id) {
                    self.items.iter().filter(|i| self.selected.contains(&i.id)).map(|i| i.id).collect()
                } else {
                    vec![id]
                }
            }
            Hit::Button(Button::DragAll) => self.items.iter().map(|i| i.id).collect(),
            _ => vec![],
        }
    }

    fn items_by(&self, ids: &[u64]) -> Vec<ShelfItem> {
        self.items.iter().filter(|i| ids.contains(&i.id)).cloned().collect()
    }
}

enum After {
    None,
    DragOut(Vec<ShelfItem>),
    Menu(Vec<ShelfItem>, POINT),
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER | WM_APP_WAKE => {
            with(|u| u.tick());
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            if with(|u| u.paint()).is_none() {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let dbl = msg == WM_LBUTTONDBLCLK;
            let ctrl = wp.0 as u32 & 0x0008 != 0; // MK_CONTROL
            let shift = wp.0 as u32 & 0x0004 != 0; // MK_SHIFT
            with(|u| {
                let p = u.client_to_global(lp);
                let h = u.hit(p);
                u.press = Some((p, h));
                match h {
                    Hit::Close(k) => {
                        if let Some(id) = u.items.get(k).map(|i| i.id) {
                            u.shared.edit(|s| s.remove(&[id]));
                        }
                        u.press = None;
                    }
                    Hit::Tile(k) => {
                        let Some(it) = u.items.get(k).cloned() else { return };
                        if ctrl {
                            if !u.selected.remove(&it.id) {
                                u.selected.insert(it.id);
                            }
                        } else if shift {
                            u.selected.insert(it.id);
                        } else if !u.selected.contains(&it.id) {
                            u.selected = [it.id].into_iter().collect();
                        }
                        if dbl {
                            open(&[it]);
                        }
                    }
                    Hit::Button(_) => {}
                    _ => u.selected.clear(),
                }
                SetCapture(hwnd);
                u.invalidate();
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let after = with(|u| {
                let p = u.client_to_global(lp);
                let Some((start, h)) = u.press else { return After::None };
                if (p.x - start.x).hypot(p.y - start.y) < 4.0 * u.scale {
                    return After::None;
                }
                let ids = u.ids_for_drag(h);
                if ids.is_empty() {
                    return After::None;
                }
                u.press = None;
                u.own_drag = true;
                After::DragOut(u.items_by(&ids))
            });
            if let Some(After::DragOut(items)) = after {
                let _ = ReleaseCapture();
                let effect = drag_out(&items);
                with(|u| {
                    u.own_drag = false;
                    if effect.0 != 0 && u.cfg.remove_after_drag_out {
                        let ids: Vec<u64> = items.iter().map(|i| i.id).collect();
                        u.shared.edit(|s| s.remove(&ids));
                    }
                });
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            with(|u| {
                let p = u.client_to_global(lp);
                let Some((_, h)) = u.press.take() else { return };
                if let Hit::Button(b) = h {
                    if u.hit(p) == h {
                        match b {
                            Button::Clear => {
                                u.selected.clear();
                                u.shared.edit(|s| s.clear());
                            }
                            Button::Share => {
                                // No share sheet for a plain Win32 window: copy instead.
                                let items = if u.selected.is_empty() { u.items.clone() } else { u.items.iter().filter(|i| u.selected.contains(&i.id)).cloned().collect() };
                                copy_text(u.hwnd, &items);
                            }
                            Button::DragAll => u.selected = u.items.iter().map(|i| i.id).collect(),
                        }
                        u.invalidate();
                    }
                }
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let after = with(|u| {
                let p = u.client_to_global(lp);
                match u.hit(p) {
                    Hit::Tile(k) => {
                        let id = u.items[k].id;
                        if !u.selected.contains(&id) {
                            u.selected = [id].into_iter().collect();
                        }
                        u.menu_open = true;
                        u.invalidate();
                        let items = u.items.iter().filter(|i| u.selected.contains(&i.id)).cloned().collect();
                        After::Menu(items, POINT { x: p.x as i32, y: p.y as i32 })
                    }
                    Hit::Outside => After::None,
                    _ => {
                        u.menu_open = true;
                        After::Menu(vec![], POINT { x: p.x as i32, y: p.y as i32 })
                    }
                }
            });
            if let Some(After::Menu(items, pt)) = after {
                context_menu(hwnd, items, pt);
                with(|u| u.menu_open = false);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f64;
            with(|u| {
                let s = geo::clamp_scroll(u.scroll - delta / 120.0 * 60.0 * u.scale, u.layout.max_scroll);
                if s != u.scroll {
                    u.scroll = s;
                    u.layout = geo::layout(&u.panel, &u.layout.notch.clone(), u.items.len(), s, &u.m);
                    u.invalidate();
                }
            });
            LRESULT(0)
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            with(|u| u.relayout());
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CLOSE => {
            let _ = RevokeDragDrop(hwnd);
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn open(items: &[ShelfItem]) {
    for it in items {
        let target = match &it.kind {
            ItemKind::File { path, .. } => path.to_string_lossy().into_owned(),
            ItemKind::Url { url, .. } => url.clone(),
            ItemKind::Text { .. } => continue,
        };
        let w = wide(&target);
        unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(w.as_ptr()), None, None, SW_SHOWNORMAL);
        }
    }
}

fn reveal(items: &[ShelfItem]) {
    for p in items.iter().filter_map(|i| i.path()) {
        let args = wide(&format!("/select,\"{}\"", p.display()));
        unsafe {
            ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(args.as_ptr()), None, SW_SHOWNORMAL);
        }
    }
}

fn item_text(items: &[ShelfItem]) -> String {
    items
        .iter()
        .map(|i| match &i.kind {
            ItemKind::File { path, .. } => path.to_string_lossy().into_owned(),
            ItemKind::Url { url, .. } => url.clone(),
            ItemKind::Text { text } => text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn hglobal_text(s: &str) -> Option<HGLOBAL> {
    let w = wide(s);
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2).ok()?;
        let p = GlobalLock(h) as *mut u16;
        if p.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        let _ = GlobalUnlock(h);
        Some(h)
    }
}

fn copy_text(hwnd: HWND, items: &[ShelfItem]) {
    if items.is_empty() {
        return;
    }
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Some(h) = hglobal_text(&item_text(items)) {
            let _ = SetClipboardData(CF_TEXT_U as u32, Some(HANDLE(h.0)));
        }
        let _ = CloseClipboard();
    }
}

fn context_menu(hwnd: HWND, items: Vec<ShelfItem>, pt: POINT) {
    const OPEN: usize = 1;
    const REVEAL: usize = 2;
    const COPY: usize = 3;
    const REMOVE: usize = 4;
    const CLEAR: usize = 5;
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        if !items.is_empty() {
            let _ = AppendMenuW(menu, MF_STRING, OPEN, w!("Open"));
            let flags = if items.iter().any(|i| i.path().is_some()) { MF_STRING } else { MF_STRING | MF_GRAYED };
            let _ = AppendMenuW(menu, flags, REVEAL, w!("Show in Explorer"));
            let _ = AppendMenuW(menu, MF_STRING, COPY, w!("Copy"));
            let _ = AppendMenuW(menu, MF_STRING, REMOVE, w!("Remove"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
        let _ = AppendMenuW(menu, MF_STRING, CLEAR, w!("Clear Shelf"));
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None).0 as usize;
        let _ = DestroyMenu(menu);
        match cmd {
            OPEN => open(&items),
            REVEAL => reveal(&items),
            COPY => copy_text(hwnd, &items),
            REMOVE => {
                let ids: Vec<u64> = items.iter().map(|i| i.id).collect();
                with(|u| u.shared.edit(|s| s.remove(&ids)));
            }
            CLEAR => {
                with(|u| u.shared.edit(|s| s.clear()));
            }
            _ => {}
        }
    }
}

// ----- OLE drag and drop ------------------------------------------------

fn fmt(cf: u16) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

fn url_format() -> u16 {
    unsafe { RegisterClipboardFormatW(w!("UniformResourceLocatorW")) as u16 }
}

fn has_format(d: &IDataObject, cf: u16) -> bool {
    unsafe { d.QueryGetData(&fmt(cf)) == S_OK }
}

fn accepts(d: &IDataObject) -> bool {
    has_format(d, CF_FILES) || has_format(d, CF_TEXT_U) || has_format(d, url_format())
}

fn read_text(d: &IDataObject, cf: u16) -> Option<String> {
    unsafe {
        let mut m = d.GetData(&fmt(cf)).ok()?;
        let h = m.u.hGlobal;
        let p = GlobalLock(h) as *const u16;
        let out = if p.is_null() {
            None
        } else {
            let max = GlobalSize(h) / 2;
            let mut n = 0;
            while n < max && *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(h);
            Some(s)
        };
        ReleaseStgMedium(&mut m);
        out
    }
}

fn read_files(d: &IDataObject) -> Vec<PathBuf> {
    let mut out = vec![];
    unsafe {
        let Ok(mut m) = d.GetData(&fmt(CF_FILES)) else { return out };
        let hdrop = HDROP(m.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..got])));
        }
        ReleaseStgMedium(&mut m);
    }
    out
}

fn read_drop(d: &IDataObject) -> Vec<NewItem> {
    let files = read_files(d);
    if !files.is_empty() {
        return files.into_iter().map(NewItem::File).collect();
    }
    if let Some(u) = read_text(d, url_format()).filter(|u| !u.trim().is_empty()) {
        return vec![NewItem::Url { url: u.trim().to_string(), title: None }];
    }
    read_text(d, CF_TEXT_U).filter(|t| !t.trim().is_empty()).map(|t| vec![NewItem::Text(t)]).unwrap_or_default()
}

#[implement(IDropTarget)]
struct DropTarget;

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, data: windows::core::Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let ok = data.as_ref().is_some_and(accepts);
        let own = with(|u| u.own_drag).unwrap_or(false);
        let ok = ok && !own;
        with(|u| {
            u.drag_inside = ok;
            u.drop_hover = ok;
            if ok && u.cfg.expand_on_drag {
                u.logic.expand();
            }
            u.invalidate();
        });
        unsafe {
            if !effect.is_null() {
                *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let ok = with(|u| u.drop_hover).unwrap_or(false);
        unsafe {
            if !effect.is_null() {
                *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        with(|u| {
            u.drag_inside = false;
            u.drop_hover = false;
            u.invalidate();
        });
        Ok(())
    }

    fn Drop(&self, data: windows::core::Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let items = data.as_ref().map(read_drop).unwrap_or_default();
        let added = with(|u| {
            u.drag_inside = false;
            u.drop_hover = false;
            u.scroll = f64::MAX;
            let ids = u.shared.add(items);
            u.invalidate();
            !ids.is_empty()
        })
        .unwrap_or(false);
        unsafe {
            if !effect.is_null() {
                *effect = if added { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        Ok(())
    }
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// A data object for the items: the shell's own for files (so Explorer and
/// other apps get CF_HDROP, shell IDLists, …), or plain text otherwise.
fn data_object(items: &[ShelfItem]) -> Option<IDataObject> {
    let paths: Vec<_> = items.iter().filter_map(|i| i.path()).collect();
    unsafe {
        if !paths.is_empty() {
            let mut pidls: Vec<*mut ITEMIDLIST> = vec![];
            for p in &paths {
                let w = wide(&p.to_string_lossy());
                let mut pidl = std::ptr::null_mut();
                if SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                    pidls.push(pidl);
                }
            }
            let consts: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
            let obj = SHCreateShellItemArrayFromIDLists(&consts).and_then(|arr| arr.BindToHandler::<_, IDataObject>(None, &BHID_DataObject)).ok();
            for p in pidls {
                ILFree(Some(p));
            }
            return obj;
        }
        let obj: IDataObject = SHCreateDataObject(None, None, None).ok()?;
        let h = hglobal_text(&item_text(items))?;
        let medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: std::mem::ManuallyDrop::new(None) };
        obj.SetData(&fmt(CF_TEXT_U), &medium, true).ok()?;
        Some(obj)
    }
}

fn drag_out(items: &[ShelfItem]) -> DROPEFFECT {
    let Some(obj) = data_object(items) else { return DROPEFFECT_NONE };
    let src: IDropSource = DropSource.into();
    let mut effect = DROPEFFECT_NONE;
    unsafe {
        let _ = DoDragDrop(&obj, &src, DROPEFFECT_COPY | DROPEFFECT_LINK, &mut effect);
    }
    effect
}
