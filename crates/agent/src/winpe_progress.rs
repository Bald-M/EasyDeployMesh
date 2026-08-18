use std::{
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject,
            DrawTextW, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, SetBkMode, SetTextColor,
            TRANSPARENT, UpdateWindow,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
            DispatchMessageW, GetClientRect, GetMessageW, GetSystemMetrics, IDC_ARROW, LoadCursorW,
            LoadIconW, MSG, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, ShowWindow,
            TranslateMessage, WM_CLOSE, WM_PAINT, WNDCLASSW, WS_CAPTION, WS_EX_TOPMOST,
            WS_OVERLAPPED, WS_SYSMENU,
        },
    },
    core::{PCWSTR, w},
};

const WIDTH: i32 = 520;
const HEIGHT: i32 = 190;

#[derive(Clone)]
struct ViewState {
    percent: u8,
    message: String,
    failed: bool,
}

static STATE: LazyLock<Mutex<ViewState>> = LazyLock::new(|| {
    Mutex::new(ViewState {
        percent: 0,
        message: "Waiting for a deployment task".to_owned(),
        failed: false,
    })
});
static WINDOW: AtomicUsize = AtomicUsize::new(0);

pub struct ProgressWindow;

impl ProgressWindow {
    pub fn open() -> Self {
        if WINDOW.load(Ordering::Acquire) == 0 {
            thread::spawn(run_window);
        }
        Self
    }

    pub fn update(&self, percent: u8, message: &str) {
        set_state(percent.min(100), message, false);
    }

    pub fn failed(&self, message: &str) {
        let percent = STATE.lock().map(|state| state.percent).unwrap_or(0);
        set_state(percent, message, true);
    }
}

fn set_state(percent: u8, message: &str, failed: bool) {
    if let Ok(mut state) = STATE.lock() {
        state.percent = percent;
        state.message = message.to_owned();
        state.failed = failed;
    }
    let raw = WINDOW.load(Ordering::Acquire);
    if raw != 0 {
        let hwnd = HWND(raw as *mut _);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
            let _ = UpdateWindow(hwnd);
        }
    }
}

fn run_window() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let instance = HINSTANCE(module.0);
        let class_name = w!("EasyDeployMeshProgressWindow");
        let icon = LoadIconW(Some(instance), PCWSTR(1 as *const u16)).unwrap_or_default();
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return;
        }
        let x = (GetSystemMetrics(SM_CXSCREEN) - WIDTH) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - HEIGHT) / 2;
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST,
            class_name,
            w!("EasyDeployMesh - System Deployment"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            if x >= 0 { x } else { CW_USEDEFAULT },
            if y >= 0 { y } else { CW_USEDEFAULT },
            WIDTH,
            HEIGHT,
            None,
            None,
            Some(instance),
            None,
        ) else {
            return;
        };
        WINDOW.store(hwnd.0 as usize, Ordering::Release);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        WINDOW.store(0, Ordering::Release);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE {
        return LRESULT(0);
    }
    if message != WM_PAINT {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let state = STATE
        .lock()
        .map(|state| state.clone())
        .unwrap_or(ViewState {
            percent: 0,
            message: "Deployment in progress".to_owned(),
            failed: false,
        });
    let mut paint = PAINTSTRUCT::default();
    let dc = unsafe { BeginPaint(hwnd, &mut paint) };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };

    let background = unsafe { CreateSolidBrush(COLORREF(0x0025_2525)) };
    unsafe { FillRect(dc, &client, background) };
    let _ = unsafe { DeleteObject(background.into()) };
    unsafe {
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00f5_f5f5));
    }

    draw_text(
        dc,
        "Deploying Windows",
        RECT {
            left: 28,
            top: 18,
            right: client.right - 28,
            bottom: 48,
        },
    );
    let percent_text = format!("{}%", state.percent);
    draw_text(
        dc,
        &percent_text,
        RECT {
            left: client.right - 90,
            top: 18,
            right: client.right - 28,
            bottom: 48,
        },
    );

    let track = RECT {
        left: 28,
        top: 61,
        right: client.right - 28,
        bottom: 79,
    };
    let track_brush = unsafe { CreateSolidBrush(COLORREF(0x0050_5050)) };
    unsafe { FillRect(dc, &track, track_brush) };
    let _ = unsafe { DeleteObject(track_brush.into()) };
    let fill_width = (track.right - track.left) * i32::from(state.percent) / 100;
    let fill = RECT {
        right: track.left + fill_width,
        ..track
    };
    let fill_color = if state.failed {
        COLORREF(0x0045_45e8)
    } else {
        COLORREF(0x00d8_a532)
    };
    let fill_brush = unsafe { CreateSolidBrush(fill_color) };
    unsafe { FillRect(dc, &fill, fill_brush) };
    let _ = unsafe { DeleteObject(fill_brush.into()) };

    unsafe {
        SetTextColor(
            dc,
            if state.failed {
                COLORREF(0x0070_70ff)
            } else {
                COLORREF(0x00d0_d0d0)
            },
        )
    };
    draw_text(
        dc,
        &state.message,
        RECT {
            left: 28,
            top: 94,
            right: client.right - 28,
            bottom: 130,
        },
    );
    let _ = unsafe { EndPaint(hwnd, &paint) };
    LRESULT(0)
}

fn draw_text(dc: windows::Win32::Graphics::Gdi::HDC, text: &str, mut rect: RECT) {
    let mut wide = text.encode_utf16().collect::<Vec<_>>();
    unsafe {
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );
    }
}
