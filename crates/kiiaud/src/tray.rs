// SPDX-License-Identifier: MIT OR Apache-2.0

//! System Tray integration and background console window management for Windows.
//!
//! On Windows, when running in `--background` mode or with `--tray`:
//! - The console terminal window is hidden automatically (`ShowWindow(SW_HIDE)`).
//! - A system tray icon is placed in the taskbar notification area.
//! - Right-clicking opens a context menu with Endpoint info, WebUI link, Console Toggle, and Exit.
//! - Left-clicking or double-clicking directly opens the WebUI in default browser.

#[cfg(windows)]
use parking_lot::Mutex;

/// Hide the active console terminal window on Windows.
#[cfg(windows)]
pub fn hide_console_window() {
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    unsafe {
        let hwnd = GetConsoleWindow();
        if !hwnd.is_invalid() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn hide_console_window() {}

#[cfg(windows)]
struct TrayContext {
    daemon_name: String,
    web_url: Option<String>,
    device_name: String,
    backend: String,
    shutdown_tx: tokio::sync::mpsc::Sender<()>,
}

#[cfg(windows)]
static TRAY_STATE: Mutex<Option<TrayContext>> = Mutex::new(None);

/// Launch the Windows System Tray background thread and event loop.
#[cfg(windows)]
pub fn spawn_tray(
    daemon_name: String,
    web_url: Option<String>,
    device_name: String,
    backend: String,
    shutdown_tx: tokio::sync::mpsc::Sender<()>,
) {
    *TRAY_STATE.lock() = Some(TrayContext {
        daemon_name: daemon_name.clone(),
        web_url,
        device_name,
        backend,
        shutdown_tx,
    });

    std::thread::Builder::new()
        .name("kiiaud-system-tray".to_string())
        .spawn(move || {
            run_tray_message_loop(daemon_name);
        })
        .expect("Failed to spawn Windows System Tray thread");
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn spawn_tray(
    _daemon_name: String,
    _web_url: Option<String>,
    _device_name: String,
    _backend: String,
    _shutdown_tx: tokio::sync::mpsc::Sender<()>,
) {
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn open_browser(url: &str) {
    use windows::core::{PCWSTR, w};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = to_wide(url);
    unsafe {
        let _ = ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(windows)]
const WM_TRAYICON: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
#[cfg(windows)]
const IDM_OPEN_WEBUI: usize = 1001;
#[cfg(windows)]
const IDM_TOGGLE_CONSOLE: usize = 1002;
#[cfg(windows)]
const IDM_EXIT: usize = 1003;

#[cfg(windows)]
unsafe extern "system" fn tray_wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LRESULT, POINT, RECT};
    use windows::Win32::System::Console::GetConsoleWindow;
    use windows::Win32::UI::Shell::{
        NOTIFYICONDATAW, NIM_DELETE, Shell_NotifyIconW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DefWindowProcW, DestroyMenu, DestroyWindow, GetCursorPos,
        IsWindowVisible, PostQuitMessage, SetForegroundWindow, ShowWindow, TrackPopupMenu,
        MF_GRAYED, MF_SEPARATOR, MF_STRING, SW_HIDE, SW_SHOW,
        TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK,
        WM_LBUTTONUP, WM_RBUTTONUP,
    };

    unsafe {
        match msg {
            WM_TRAYICON => {
                let event = (lparam.0 & 0xffff) as u32;
                if event == WM_RBUTTONUP {
                    let mut pt: POINT = core::mem::zeroed();
                    let _ = GetCursorPos(&mut pt);

                    if let Ok(hmenu) = CreatePopupMenu() {
                        let lock = TRAY_STATE.lock();
                        if let Some(ctx) = lock.as_ref() {
                            let header = to_wide(&format!("kiiaud — {}", ctx.daemon_name));
                            let _ = AppendMenuW(hmenu, MF_STRING | MF_GRAYED, 0, PCWSTR(header.as_ptr()));

                            let dac_line = to_wide(&format!("DAC: {} ({})", ctx.device_name, ctx.backend));
                            let _ = AppendMenuW(hmenu, MF_STRING | MF_GRAYED, 0, PCWSTR(dac_line.as_ptr()));

                            let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());

                            if let Some(url) = &ctx.web_url {
                                let web_item = to_wide(&format!("Open WebUI ({url})"));
                                let _ = AppendMenuW(hmenu, MF_STRING, IDM_OPEN_WEBUI, PCWSTR(web_item.as_ptr()));
                            }

                            let con = GetConsoleWindow();
                            let con_visible = !con.is_invalid() && IsWindowVisible(con).as_bool();
                            let con_text = to_wide(if con_visible {
                                "Hide Console Window"
                            } else {
                                "Show Console Window"
                            });
                            let _ = AppendMenuW(hmenu, MF_STRING, IDM_TOGGLE_CONSOLE, PCWSTR(con_text.as_ptr()));

                            let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
                            let exit_text = to_wide("Exit kiiaud");
                            let _ = AppendMenuW(hmenu, MF_STRING, IDM_EXIT, PCWSTR(exit_text.as_ptr()));
                        }
                        drop(lock);

                        let _ = SetForegroundWindow(hwnd);
                        let _ = TrackPopupMenu(
                            hmenu,
                            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                            pt.x,
                            pt.y,
                            0,
                            hwnd,
                            None::<*const RECT>,
                        );
                        let _ = DestroyMenu(hmenu);
                    }
                } else if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                    let web_url: Option<String> = TRAY_STATE.lock().as_ref().and_then(|c| c.web_url.clone());
                    if let Some(ref url) = web_url {
                        open_browser(url);
                    } else {
                        let con = GetConsoleWindow();
                        if !con.is_invalid() {
                            if IsWindowVisible(con).as_bool() {
                                let _ = ShowWindow(con, SW_HIDE);
                            } else {
                                let _ = ShowWindow(con, SW_SHOW);
                            }
                        }
                    }
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xffff) as usize;
                match id {
                    IDM_OPEN_WEBUI => {
                        let web_url: Option<String> = TRAY_STATE.lock().as_ref().and_then(|c| c.web_url.clone());
                        if let Some(ref url) = web_url {
                            open_browser(url);
                        }
                    }
                    IDM_TOGGLE_CONSOLE => {
                        let con = GetConsoleWindow();
                        if !con.is_invalid() {
                            if IsWindowVisible(con).as_bool() {
                                let _ = ShowWindow(con, SW_HIDE);
                            } else {
                                let _ = ShowWindow(con, SW_SHOW);
                            }
                        }
                    }
                    IDM_EXIT => {
                        if let Some(ctx) = TRAY_STATE.lock().as_ref() {
                            let _ = ctx.shutdown_tx.try_send(());
                        }
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let mut nid: NOTIFYICONDATAW = core::mem::zeroed();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = hwnd;
                nid.uID = 1;
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(windows)]
fn run_tray_message_loop(daemon_name: String) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND};
    use windows::Win32::UI::Shell::{
        NOTIFYICONDATAW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, Shell_NotifyIconW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, LoadIconW, RegisterClassW,
        TranslateMessage, HMENU, IDI_APPLICATION, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
    };

    unsafe {
        let class_name = to_wide("kiiaud_tray_wnd_class");
        let mut wnd_class: WNDCLASSW = core::mem::zeroed();
        wnd_class.lpfnWndProc = Some(tray_wnd_proc);
        wnd_class.hInstance = HINSTANCE::default();
        wnd_class.lpszClassName = PCWSTR(class_name.as_ptr());

        let _ = RegisterClassW(&wnd_class);

        let window_title = to_wide("kiiaud Background Tray");
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_title.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            HINSTANCE::default(),
            None,
        );

        let hwnd = match hwnd {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("Failed to create hidden window for System Tray: {e}");
                return;
            }
        };

        let icon = LoadIconW(HINSTANCE::default(), IDI_APPLICATION).unwrap_or_default();

        let mut tip = [0u16; 128];
        let tip_str = format!("kiiaud — {daemon_name}");
        for (i, c) in tip_str.encode_utf16().take(127).enumerate() {
            tip[i] = c;
        }

        let mut nid: NOTIFYICONDATAW = core::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = icon;
        nid.szTip = tip;

        let _ = Shell_NotifyIconW(NIM_ADD, &nid);

        let mut msg: MSG = core::mem::zeroed();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
