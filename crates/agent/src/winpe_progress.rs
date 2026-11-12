//! Full-screen deployment progress window shown inside Windows PE.
//!
//! The window only displays state; the deployment flow never waits on it except
//! for the post-success restart countdown. All content and layout come from
//! `progress_view`. Drawing uses plain GDI because WinPE has no WebView2 or .NET.

use crate::deployment::LocalProgress;
use crate::progress_view::{
    BannerTone, JobInfo, Language, PowerAction, ProgressModel, Rect, Screen, ScreenLayout, Tone,
    ValueStyle, layout_panel, panel_size,
};
use easydeploymesh_core::DeploymentStage;
use std::{
    error::Error,
    process::Command,
    sync::{
        Condvar, LazyLock, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{
            COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT,
            POINT, RECT, WPARAM,
        },
        Graphics::Gdi::{
            BeginPaint, BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap,
            CreateCompatibleDC, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush,
            DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_EDITCONTROL, DT_END_ELLIPSIS, DT_LEFT,
            DT_NOPREFIX, DT_PATH_ELLIPSIS, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
            DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, EnumFontFamiliesExW, FF_SWISS,
            FW_NORMAL, FW_SEMIBOLD, FillRect, GB2312_CHARSET, GetDC, GetStockObject, HDC, HFONT,
            HGDIOBJ, InvalidateRect, LOGFONTW, NULL_BRUSH, NULL_PEN, OUT_DEFAULT_PRECIS,
            PAINTSTRUCT, PS_SOLID, Polyline, ReleaseDC, RoundRect, SRCCOPY, ScreenToClient,
            SelectClipRgn, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TEXTMETRICW,
            TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DI_NORMAL, DefWindowProcW, DispatchMessageW, DrawIconEx,
            GetClientRect, GetMessageW, GetSystemMetrics, GetWindowRect, HICON, HTCAPTION,
            HTCLIENT, IDC_ARROW, IMAGE_ICON, IsWindowVisible, LR_DEFAULTCOLOR, LoadCursorW,
            LoadIconW, LoadImageW, MSG, PostMessageW, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN,
            SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SetForegroundWindow, SetTimer,
            SetWindowPos, ShowWindow, TranslateMessage, WM_APP, WM_CLOSE, WM_ERASEBKGND,
            WM_KEYDOWN, WM_LBUTTONUP, WM_NCHITTEST, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_TOPMOST,
            WS_POPUP,
        },
    },
    core::{PCWSTR, w},
};

/// Resource id of the embedded brand mark icon; see `build.rs`.
const BRAND_MARK_RESOURCE_ID: u16 = 2;
const REFRESH_TIMER_ID: usize = 1;
const VK_ESCAPE: u16 = 0x1b;
/// Posted by other threads to resize and repaint the window.
const WM_REFRESH: u32 = WM_APP + 1;
const PANEL_RADIUS: i32 = 24;
const REFRESH_INTERVAL_MS: u32 = 1_000;
const MINIMUM_REPAINT_INTERVAL: Duration = Duration::from_millis(200);

/// Face names that render Simplified Chinese, in order of preference.
const CHINESE_FACES: [&str; 6] = [
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "DengXian",
    "SimHei",
    "SimSun",
    "NSimSun",
];
const LATIN_FACE: &str = "Segoe UI";
const MONOSPACE_FACE: &str = "Consolas";

static MODEL: LazyLock<Mutex<Option<ProgressModel>>> = LazyLock::new(|| Mutex::new(None));
static WINDOW: AtomicUsize = AtomicUsize::new(0);
static WINDOW_STARTED: AtomicBool = AtomicBool::new(false);
static LAST_REPAINT: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));
static FONTS: OnceLock<FontChoice> = OnceLock::new();
static POWER_REQUEST: LazyLock<(Mutex<Option<PowerAction>>, Condvar)> =
    LazyLock::new(|| (Mutex::new(None), Condvar::new()));
static POWER_ACTION_RUNNING: AtomicBool = AtomicBool::new(false);
/// Set when the operator hides a failure screen with Esc; cleared by the next job.
static HIDDEN_BY_OPERATOR: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
struct FontChoice {
    language: Language,
    face: String,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct ProgressWindow;

impl ProgressWindow {
    pub fn open() -> Self {
        if !WINDOW_STARTED.swap(true, Ordering::AcqRel) {
            thread::spawn(run_window);
        }
        Self
    }

    /// Starts a fresh view for a newly claimed job. A failure screen from the
    /// previous job stays visible until this is called.
    pub fn begin_job(&self, job: JobInfo) {
        let language = font_choice().language;
        {
            let mut model = lock(&MODEL);
            let disk_touched_earlier = model
                .as_ref()
                .is_some_and(ProgressModel::disk_may_be_modified);
            *model = Some(ProgressModel::new(
                language,
                job,
                disk_touched_earlier,
                Instant::now(),
            ));
        }
        *lock(&POWER_REQUEST.0) = None;
        // A failure screen may have been hidden with Esc; bring the window back.
        HIDDEN_BY_OPERATOR.store(false, Ordering::Release);
        repaint(true);
    }

    pub fn stage(&self, stage: DeploymentStage, percent: u8) {
        if let Some(model) = lock(&MODEL).as_mut() {
            model.stage(stage, percent);
        }
        repaint(true);
    }

    pub fn local(&self, event: &LocalProgress) {
        if let Some(model) = lock(&MODEL).as_mut() {
            model.local(event, Instant::now());
        }
        repaint(matches!(event, LocalProgress::Layout(_)));
    }

    pub fn failed(&self, message: &str) {
        if let Some(model) = lock(&MODEL).as_mut() {
            model.fail(message, Instant::now());
        }
        repaint(true);
    }

    pub fn failure_reported(&self) {
        if let Some(model) = lock(&MODEL).as_mut() {
            model.failure_reported();
        }
        repaint(true);
    }

    /// Shows the success screen and blocks until the operator picks a power
    /// action or the countdown ends. Without an operator it always returns
    /// [`PowerAction::Reboot`], so unattended deployments keep restarting.
    pub fn succeeded(&self, reboot_delay: Duration) -> PowerAction {
        let (request, signal) = &*POWER_REQUEST;
        // Clear stale requests before the buttons become clickable.
        lock(request).take();
        let deadline = Instant::now() + reboot_delay;
        if let Some(model) = lock(&MODEL).as_mut() {
            model.succeed(Instant::now(), reboot_delay);
        }
        repaint(true);
        let mut requested = lock(request);
        loop {
            if let Some(action) = requested.take() {
                return action;
            }
            let now = Instant::now();
            if now >= deadline {
                return PowerAction::Reboot;
            }
            requested = signal
                .wait_timeout(requested, deadline - now)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
    }
}

/// Runs `wpeutil reboot` or `wpeutil shutdown`. Never touches a disk.
pub fn run_power_action(action: PowerAction) -> Result<(), Box<dyn Error>> {
    let verb = match action {
        PowerAction::Reboot => "reboot",
        PowerAction::Shutdown => "shutdown",
    };
    let status = Command::new("wpeutil.exe").arg(verb).status()?;
    if !status.success() {
        return Err(format!("wpeutil {verb} failed: {status}").into());
    }
    Ok(())
}

fn request_power_action(action: PowerAction) {
    let succeeded = lock(&MODEL)
        .as_ref()
        .is_some_and(ProgressModel::is_succeeded);
    if succeeded {
        // The deployment thread is waiting in `succeeded` and runs the action.
        let (request, signal) = &*POWER_REQUEST;
        *lock(request) = Some(action);
        signal.notify_all();
        return;
    }
    // After a failure the Agent keeps polling for work, so run it here.
    if POWER_ACTION_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    thread::spawn(move || {
        if let Err(error) = run_power_action(action) {
            eprintln!("Progress window power action failed: {error}");
            POWER_ACTION_RUNNING.store(false, Ordering::Release);
        }
    });
}

fn repaint(force: bool) {
    {
        let mut last = lock(&LAST_REPAINT);
        let now = Instant::now();
        if !force && last.is_some_and(|last| now.duration_since(last) < MINIMUM_REPAINT_INTERVAL) {
            return;
        }
        *last = Some(now);
    }
    let raw = WINDOW.load(Ordering::Acquire);
    if raw != 0 {
        // Resizing must happen on the window thread, so ask it to refresh.
        unsafe {
            let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

/// Size of the panel for the current state, or `None` before the first job.
fn desired_size() -> Option<(i32, i32)> {
    let screen = lock(&MODEL).as_ref()?.screen(Instant::now());
    Some(panel_size(&screen, unsafe {
        GetSystemMetrics(SM_CXSCREEN)
    }))
}

/// Keeps the window exactly as large as the panel. The window stays centered
/// on the screen until the operator drags it; after that a size change keeps
/// its center where it is. The rest of the WinPE desktop stays visible.
unsafe fn sync_geometry(hwnd: HWND) {
    let Some((width, height)) = desired_size() else {
        return;
    };
    let (screen_width, screen_height) =
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let mut current = RECT::default();
    let _ = unsafe { GetWindowRect(hwnd, &mut current) };
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    if current.right - current.left != width || current.bottom - current.top != height {
        let (center_x, center_y) = if visible {
            (
                (current.left + current.right) / 2,
                (current.top + current.bottom) / 2,
            )
        } else {
            (screen_width / 2, screen_height / 2)
        };
        let left = (center_x - width / 2).clamp(0, (screen_width - width).max(0));
        let top = (center_y - height / 2).clamp(0, (screen_height - height).max(0));
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                left,
                top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            // The system owns the region after this call.
            let region =
                CreateRoundRectRgn(0, 0, width + 1, height + 1, PANEL_RADIUS, PANEL_RADIUS);
            SetWindowRgn(hwnd, Some(region), true);
        }
    }
    if !visible && !HIDDEN_BY_OPERATOR.load(Ordering::Acquire) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn font_choice() -> &'static FontChoice {
    FONTS.get_or_init(|| {
        let faces = chinese_font_faces();
        CHINESE_FACES
            .iter()
            .find(|preferred| {
                faces
                    .iter()
                    .any(|face| face.eq_ignore_ascii_case(preferred))
            })
            .map(|face| (*face).to_owned())
            .or_else(|| faces.into_iter().find(|face| !face.starts_with('@')))
            .map_or(
                FontChoice {
                    language: Language::English,
                    face: LATIN_FACE.to_owned(),
                },
                |face| FontChoice {
                    language: Language::Chinese,
                    face,
                },
            )
    })
}

/// Lists installed font families that cover the GB2312 character set. WinPE
/// media without the Chinese font package returns none, and the window then
/// falls back to English instead of drawing empty boxes.
fn chinese_font_faces() -> Vec<String> {
    unsafe extern "system" fn collect(
        font: *const LOGFONTW,
        _metrics: *const TEXTMETRICW,
        _font_type: u32,
        faces: LPARAM,
    ) -> i32 {
        let (Some(font), Some(faces)) = (unsafe { font.as_ref() }, unsafe {
            (faces.0 as *mut Vec<String>).as_mut()
        }) else {
            return 0;
        };
        let name = &font.lfFaceName;
        let length = name
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(name.len());
        let face = String::from_utf16_lossy(&name[..length]);
        if !face.is_empty() && !faces.contains(&face) {
            faces.push(face);
        }
        1
    }

    let mut faces = Vec::<String>::new();
    unsafe {
        let dc = GetDC(None);
        if dc.is_invalid() {
            return faces;
        }
        let filter = LOGFONTW {
            lfCharSet: GB2312_CHARSET,
            ..Default::default()
        };
        EnumFontFamiliesExW(
            dc,
            &filter,
            Some(collect),
            LPARAM(&mut faces as *mut Vec<String> as isize),
            0,
        );
        ReleaseDC(None, dc);
    }
    faces
}

fn run_window() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            WINDOW_STARTED.store(false, Ordering::Release);
            return;
        };
        let instance = HINSTANCE(module.0);
        let class_name = w!("EasyDeployMeshProgressWindow");
        let icon = LoadIconW(Some(instance), resource_id(1)).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class_name,
            ..Default::default()
        };
        // The class survives a failed earlier attempt to create the window.
        if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            WINDOW_STARTED.store(false, Ordering::Release);
            return;
        }
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST,
            class_name,
            w!("EasyDeployMesh"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        ) else {
            WINDOW_STARTED.store(false, Ordering::Release);
            return;
        };
        WINDOW.store(hwnd.0 as usize, Ordering::Release);
        SetTimer(Some(hwnd), REFRESH_TIMER_ID, REFRESH_INTERVAL_MS, None);
        // Shown by `sync_geometry` once a job has been started.
        sync_geometry(hwnd);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        WINDOW.store(0, Ordering::Release);
    }
}

/// Content and layout for a window whose client area is `width` x `height`.
fn current_screen(width: i32, height: i32) -> Option<(Screen, ScreenLayout)> {
    let screen = lock(&MODEL).as_ref()?.screen(Instant::now());
    let layout = layout_panel(&screen, Rect::new(0, 0, width, height));
    Some((screen, layout))
}

fn button_at(hwnd: HWND, x: i32, y: i32) -> Option<PowerAction> {
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let (_, layout) = current_screen(client.right, client.bottom)?;
    layout
        .buttons
        .iter()
        .find(|(_, rect)| rect.contains(x, y))
        .map(|(action, _)| *action)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // The window has no close affordance; the Agent owns its lifetime.
        WM_CLOSE => LRESULT(0),
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER | WM_REFRESH => {
            unsafe {
                sync_geometry(hwnd);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            // Drag the panel anywhere except on its buttons.
            let mut point = POINT {
                x: i32::from((lparam.0 & 0xffff) as u16 as i16),
                y: i32::from(((lparam.0 >> 16) & 0xffff) as u16 as i16),
            };
            let _ = unsafe { ScreenToClient(hwnd, &mut point) };
            if button_at(hwnd, point.x, point.y).is_some() {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTCAPTION as isize)
            }
        }
        WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE) => {
            // Let on-site staff reach the WinPE shell and the log after a failure.
            if lock(&MODEL).as_ref().is_some_and(ProgressModel::is_failed) {
                HIDDEN_BY_OPERATOR.store(true, Ordering::Release);
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = i32::from((lparam.0 & 0xffff) as u16 as i16);
            let y = i32::from(((lparam.0 >> 16) & 0xffff) as u16 as i16);
            if let Some(action) = button_at(hwnd, x, y) {
                request_power_action(action);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            unsafe { paint(hwnd) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | (green as u32) << 8 | (blue as u32) << 16)
}

// Tailwind slate / cyan / emerald / rose / amber, matching the desktop app.
const SLATE_50: COLORREF = rgb(0xf8, 0xfa, 0xfc);
const SLATE_200: COLORREF = rgb(0xe2, 0xe8, 0xf0);
const SLATE_300: COLORREF = rgb(0xcb, 0xd5, 0xe1);
const SLATE_500: COLORREF = rgb(0x64, 0x74, 0x8b);
const SLATE_600: COLORREF = rgb(0x47, 0x55, 0x69);
const SLATE_900: COLORREF = rgb(0x0f, 0x17, 0x2a);
const WHITE: COLORREF = rgb(0xff, 0xff, 0xff);
const CYAN_500: COLORREF = rgb(0x06, 0xb6, 0xd4);
const CYAN_700: COLORREF = rgb(0x0e, 0x74, 0x90);
const EMERALD_600: COLORREF = rgb(0x05, 0x96, 0x69);
const EMERALD_700: COLORREF = rgb(0x04, 0x78, 0x57);
const ROSE_50: COLORREF = rgb(0xff, 0xf1, 0xf2);
const ROSE_600: COLORREF = rgb(0xe1, 0x1d, 0x48);
const ROSE_700: COLORREF = rgb(0xbe, 0x12, 0x3c);
const AMBER_50: COLORREF = rgb(0xff, 0xfb, 0xeb);
const AMBER_800: COLORREF = rgb(0x92, 0x40, 0x0e);

struct Fonts {
    product: HFONT,
    regular: HFONT,
    strong: HFONT,
    title: HFONT,
    monospace: HFONT,
}

impl Fonts {
    unsafe fn create(face: &str) -> Self {
        Self {
            product: unsafe { create_font(face, 15, FW_SEMIBOLD.0) },
            regular: unsafe { create_font(face, 14, FW_NORMAL.0) },
            strong: unsafe { create_font(face, 14, FW_SEMIBOLD.0) },
            title: unsafe { create_font(face, 20, FW_SEMIBOLD.0) },
            monospace: unsafe { create_font(MONOSPACE_FACE, 13, FW_NORMAL.0) },
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for font in [
            self.product,
            self.regular,
            self.strong,
            self.title,
            self.monospace,
        ] {
            unsafe {
                let _ = DeleteObject(font.into());
            }
        }
    }
}

unsafe fn create_font(face: &str, pixel_height: i32, weight: u32) -> HFONT {
    let face = wide(face);
    unsafe {
        CreateFontW(
            -pixel_height,
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            u32::from(FF_SWISS.0),
            PCWSTR(face.as_ptr()),
        )
    }
}

/// Equivalent of `MAKEINTRESOURCEW`.
fn resource_id(id: u16) -> PCWSTR {
    PCWSTR(std::ptr::without_provenance(usize::from(id)))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win_rect(rect: Rect) -> RECT {
    RECT {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    let window_dc = unsafe { BeginPaint(hwnd, &mut paint) };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let (width, height) = (client.right.max(1), client.bottom.max(1));
    unsafe {
        let dc = CreateCompatibleDC(Some(window_dc));
        let bitmap = CreateCompatibleBitmap(window_dc, width, height);
        let previous_bitmap = SelectObject(dc, bitmap.into());
        fill(dc, Rect::new(0, 0, width, height), WHITE);
        if let Some((screen, layout)) = current_screen(width, height) {
            thread_local! {
                static FONTS_CACHE: std::cell::OnceCell<Fonts> = const { std::cell::OnceCell::new() };
            }
            FONTS_CACHE.with(|cache| {
                let fonts = cache.get_or_init(|| Fonts::create(&font_choice().face));
                draw_screen(dc, &screen, &layout, fonts);
            });
        }
        let _ = BitBlt(window_dc, 0, 0, width, height, Some(dc), 0, 0, SRCCOPY);
        SelectObject(dc, previous_bitmap);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        let _ = EndPaint(hwnd, &paint);
    }
}

unsafe fn draw_screen(dc: HDC, screen: &Screen, layout: &ScreenLayout, fonts: &Fonts) {
    let panel = layout.panel;
    unsafe {
        SetBkMode(dc, TRANSPARENT);
        // Clip section fills to the rounded panel, then draw its border last.
        let clip = CreateRoundRectRgn(
            panel.left,
            panel.top,
            panel.right + 1,
            panel.bottom + 1,
            PANEL_RADIUS,
            PANEL_RADIUS,
        );
        SelectClipRgn(dc, Some(clip));
        fill(dc, panel, WHITE);

        draw_brand_mark(dc, layout.brand_mark);
        text(
            dc,
            screen.product,
            layout.product,
            fonts.product,
            SLATE_900,
            DT_LEFT,
        );
        text(
            dc,
            &screen.device,
            layout.device,
            fonts.regular,
            SLATE_500,
            DT_RIGHT,
        );

        let (bar_color, icon_color) = match screen.tone {
            Tone::Running => (CYAN_500, CYAN_500),
            Tone::Success => (EMERALD_600, EMERALD_600),
            Tone::Failure => (ROSE_600, ROSE_600),
        };
        if let Some(icon) = layout.status_icon {
            draw_status_icon(dc, icon, icon_color, screen.tone == Tone::Success);
        }
        text(
            dc,
            &screen.title,
            layout.title,
            fonts.title,
            SLATE_900,
            DT_LEFT,
        );
        if let Some(caption) = &screen.step_caption {
            text(
                dc,
                caption,
                layout.step_caption,
                fonts.regular,
                SLATE_500,
                DT_RIGHT,
            );
        }
        rounded(dc, layout.bar, 8, SLATE_200, None);
        let filled = layout.bar.width() * i32::from(screen.percent.min(100)) / 100;
        if filled > 0 {
            let mut bar = layout.bar;
            bar.right = bar.left + filled.max(8);
            rounded(dc, bar, 8, bar_color, None);
        }
        if let Some(percent) = &screen.percent_label {
            text(
                dc,
                percent,
                layout.percent,
                fonts.strong,
                SLATE_900,
                DT_LEFT,
            );
        }
        if let Some(remaining) = &screen.remaining_label {
            text(
                dc,
                remaining,
                layout.remaining,
                fonts.regular,
                SLATE_600,
                DT_RIGHT,
            );
        }
        if let Some(summary) = &screen.summary {
            text(
                dc,
                summary,
                layout.summary,
                fonts.regular,
                SLATE_600,
                DT_LEFT,
            );
        }

        for (column, rows) in screen.columns.iter().zip(&layout.rows) {
            for (row, (label, value)) in column.iter().zip(rows) {
                text(dc, &row.label, *label, fonts.regular, SLATE_500, DT_LEFT);
                let (font, color, flags) = match row.style {
                    ValueStyle::Normal => (fonts.regular, SLATE_900, DT_END_ELLIPSIS),
                    ValueStyle::Monospace => (fonts.monospace, SLATE_900, DT_PATH_ELLIPSIS),
                    ValueStyle::Positive => (fonts.strong, EMERALD_700, DT_END_ELLIPSIS),
                };
                if row.lines > 1 {
                    wrapped_text(dc, &row.value, *value, font, color);
                } else {
                    text(dc, &row.value, *value, font, color, DT_RIGHT | flags);
                }
            }
        }
        if let Some(separator) = layout.column_separator {
            fill(dc, separator, SLATE_200);
        }

        if let (Some(banner), Some(area)) = (&screen.banner, layout.banner) {
            let (background, foreground) = match banner.tone {
                BannerTone::Warning => (AMBER_50, AMBER_800),
                BannerTone::Danger => (ROSE_50, ROSE_700),
                BannerTone::Neutral => (SLATE_50, SLATE_600),
            };
            fill(dc, area, background);
            let mut inner = area;
            inner.left += 24;
            inner.right -= 24;
            text(
                dc,
                &banner.text,
                inner,
                fonts.regular,
                foreground,
                DT_LEFT | DT_END_ELLIPSIS,
            );
        }

        if let (Some(footer), Some(area)) = (&screen.footer, layout.footer_text) {
            text(
                dc,
                &footer.text,
                area,
                fonts.regular,
                SLATE_600,
                DT_LEFT | DT_END_ELLIPSIS,
            );
            for (button, (_, rect)) in footer.buttons.iter().zip(&layout.buttons) {
                let (background, border, foreground, font) = if button.primary {
                    (CYAN_700, None, WHITE, fonts.strong)
                } else {
                    (WHITE, Some(SLATE_300), SLATE_900, fonts.regular)
                };
                rounded(dc, *rect, 12, background, border);
                text(
                    dc,
                    &button.label,
                    *rect,
                    font,
                    foreground,
                    windows::Win32::Graphics::Gdi::DT_CENTER,
                );
            }
        }

        for separator in &layout.separators {
            fill(dc, *separator, SLATE_200);
        }
        SelectClipRgn(dc, None);
        let _ = DeleteObject(clip.into());
        outline(dc, panel, PANEL_RADIUS, SLATE_200);
    }
}

unsafe fn fill(dc: HDC, rect: Rect, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, &win_rect(rect), brush);
        let _ = DeleteObject(brush.into());
    }
}

/// Draws a filled rounded rectangle, optionally with a one-pixel border.
unsafe fn rounded(dc: HDC, rect: Rect, radius: i32, color: COLORREF, border: Option<COLORREF>) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let pen = border.map(|border| CreatePen(PS_SOLID, 1, border));
        let previous_brush = SelectObject(dc, brush.into());
        let previous_pen = SelectObject(
            dc,
            pen.map_or_else(|| GetStockObject(NULL_PEN), HGDIOBJ::from),
        );
        // A NULL pen leaves the right and bottom edge undrawn, so extend by one.
        let extra = i32::from(border.is_none());
        let _ = RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right + extra,
            rect.bottom + extra,
            radius,
            radius,
        );
        SelectObject(dc, previous_pen);
        SelectObject(dc, previous_brush);
        let _ = DeleteObject(brush.into());
        if let Some(pen) = pen {
            let _ = DeleteObject(pen.into());
        }
    }
}

unsafe fn outline(dc: HDC, rect: Rect, radius: i32, color: COLORREF) {
    unsafe {
        let pen = CreatePen(PS_SOLID, 1, color);
        let previous_pen = SelectObject(dc, pen.into());
        let previous_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
        let _ = RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius,
            radius,
        );
        SelectObject(dc, previous_brush);
        SelectObject(dc, previous_pen);
        let _ = DeleteObject(pen.into());
    }
}

unsafe fn text(
    dc: HDC,
    value: &str,
    rect: Rect,
    font: HFONT,
    color: COLORREF,
    flags: DRAW_TEXT_FORMAT,
) {
    let mut wide = value.encode_utf16().collect::<Vec<_>>();
    let mut rect = win_rect(rect);
    unsafe {
        let previous = SelectObject(dc, font.into());
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
        SelectObject(dc, previous);
    }
}

/// Right-aligned text that wraps onto the lines its rectangle allows. The first
/// line lines up with single-line rows, whose text is vertically centered.
unsafe fn wrapped_text(dc: HDC, value: &str, mut rect: Rect, font: HFONT, color: COLORREF) {
    const FIRST_LINE_OFFSET: i32 = 7;
    rect.top += FIRST_LINE_OFFSET;
    let mut wide = value.encode_utf16().collect::<Vec<_>>();
    let mut rect = win_rect(rect);
    unsafe {
        let previous = SelectObject(dc, font.into());
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_RIGHT | DT_WORDBREAK | DT_EDITCONTROL | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(dc, previous);
    }
}

unsafe fn draw_brand_mark(dc: HDC, rect: Rect) {
    static ICON: AtomicUsize = AtomicUsize::new(0);
    let size = rect.width();
    let mut raw = ICON.load(Ordering::Acquire);
    if raw == 0 {
        raw = unsafe {
            GetModuleHandleW(None)
                .ok()
                .and_then(|module| {
                    LoadImageW(
                        Some(HINSTANCE(module.0)),
                        resource_id(BRAND_MARK_RESOURCE_ID),
                        IMAGE_ICON,
                        size,
                        size,
                        LR_DEFAULTCOLOR,
                    )
                    .ok()
                })
                .map_or(0, |handle| handle.0 as usize)
        };
        ICON.store(raw, Ordering::Release);
    }
    unsafe {
        if raw != 0 {
            let _ = DrawIconEx(
                dc,
                rect.left,
                rect.top,
                HICON(raw as *mut _),
                size,
                size,
                0,
                None,
                DI_NORMAL,
            );
        } else {
            draw_disc(dc, rect, CYAN_500);
        }
    }
}

unsafe fn draw_disc(dc: HDC, rect: Rect, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let previous_brush = SelectObject(dc, brush.into());
        let previous_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        let _ = Ellipse(dc, rect.left, rect.top, rect.right + 1, rect.bottom + 1);
        SelectObject(dc, previous_pen);
        SelectObject(dc, previous_brush);
        let _ = DeleteObject(brush.into());
    }
}

/// A filled circle with a white check mark (success) or cross (failure).
unsafe fn draw_status_icon(dc: HDC, rect: Rect, color: COLORREF, success: bool) {
    let point = |x: i32, y: i32| POINT {
        x: rect.left + x,
        y: rect.top + y,
    };
    unsafe {
        draw_disc(dc, rect, color);
        let pen = CreatePen(PS_SOLID, 2, WHITE);
        let previous_pen = SelectObject(dc, pen.into());
        if success {
            let _ = Polyline(dc, &[point(6, 10), point(9, 13), point(14, 7)]);
        } else {
            let _ = Polyline(dc, &[point(7, 7), point(13, 13)]);
            let _ = Polyline(dc, &[point(13, 7), point(7, 13)]);
        }
        SelectObject(dc, previous_pen);
        let _ = DeleteObject(pen.into());
    }
}
