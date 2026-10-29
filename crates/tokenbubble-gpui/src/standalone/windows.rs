//! Windows notification-area integration for the standalone host.
//!
//! A hidden top-level window on GPUI's thread receives the icon's callbacks; GPUI's
//! `GetMessageW(None)` loop dispatches them, so there is no second event loop. Clicks are
//! forwarded to GPUI over a channel; all panel content stays GPUI. The right-click menu is
//! the native one, shown synchronously from the callback.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use async_channel::Sender;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT,
    WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1CreateFactory,
    ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BI_BITFIELDS, BITMAPINFO, BITMAPV5HEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, GetDC, GetMonitorInfoW, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, ReleaseDC,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapCacheOnLoad,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, GetDpiForSystem, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONIDENTIFIER,
    Shell_NotifyIconGetRect, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, FindWindowW, GetCursorPos,
    GetWindowThreadProcessId, HICON, ICONINFO, MF_SEPARATOR, MF_STRING, PostMessageW,
    RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, TPM_BOTTOMALIGN,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WINDOW_EX_STYLE, WM_APP,
    WM_CLOSE, WM_CONTEXTMENU, WM_NULL, WM_SETTINGCHANGE, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use crate::placement::{self, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// Left click or keyboard selection, with the click position in physical pixels.
    Toggle {
        anchor: (i32, i32),
    },
    Refresh,
    Settings,
    About,
    Quit,
    /// A second launch asked this instance to show itself.
    Reveal,
    /// Explorer restarted; the icon must be added again.
    TaskbarCreated,
    /// System colors or theme changed.
    SettingsChanged,
}

const CLASS_NAME: PCWSTR = w!("TokenBubble.Tray");
const INSTANCE_MUTEX: PCWSTR = w!("Local\\TokenBubble.Standalone");
const CALLBACK_MESSAGE: u32 = WM_APP + 1;
const REVEAL_MESSAGE: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
/// `NIN_SELECT | NINF_KEY`: selection by keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

const MENU_REFRESH: usize = 1;
const MENU_SETTINGS: usize = 2;
const MENU_ABOUT: usize = 3;
const MENU_QUIT: usize = 4;

thread_local! {
    static EVENTS: RefCell<Option<Sender<TrayEvent>>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

fn send(event: TrayEvent) {
    EVENTS.with(|events| {
        if let Some(events) = events.borrow().as_ref() {
            let _ = events.try_send(event);
        }
    });
}

/// The notification-area icon. Main thread only; removed on drop.
pub struct Tray {
    hwnd: HWND,
    icon: Option<HICON>,
    icon_size: u32,
    tooltip: String,
}

impl Tray {
    pub fn create(events: Sender<TrayEvent>) -> windows::core::Result<Self> {
        EVENTS.with(|slot| *slot.borrow_mut() = Some(events));
        let instance = unsafe { GetModuleHandleW(None)? };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // Zero means already registered or failed; window creation reports real failures.
        unsafe { RegisterClassW(&class) };
        TASKBAR_CREATED.with(|id| id.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }));
        // A hidden top-level window (not message-only) so it receives the TaskbarCreated
        // broadcast and a second instance can find it.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                w!("TokenBubble"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )?
        };
        Ok(Self {
            hwnd,
            icon: None,
            icon_size: 0,
            tooltip: String::new(),
        })
    }

    /// Small-icon edge length for the system DPI, which the taskbar uses.
    pub fn icon_size() -> u32 {
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
        u32::try_from(size).unwrap_or(16).max(16)
    }

    /// Render Windows' native bubbles emoji only when the taskbar icon size changes.
    pub fn set(&mut self, size: u32, tooltip: &str) -> windows::core::Result<()> {
        let add = self.icon.is_none();
        if add || self.icon_size != size {
            let icon = create_icon(size)?;
            if let Some(previous) = self.icon.replace(icon) {
                let _ = unsafe { DestroyIcon(previous) };
            }
            self.icon_size = size;
        }
        self.tooltip.clear();
        self.tooltip.push_str(tooltip);
        self.publish(add)
    }

    /// Add the icon again after Explorer restarts.
    pub fn restore(&self) -> windows::core::Result<()> {
        self.publish(true)
    }

    fn publish(&self, add: bool) -> windows::core::Result<()> {
        let mut data = self.data();
        if add {
            if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
                return Err(windows::core::Error::from_thread());
            }
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
            Ok(())
        } else if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
            Ok(())
        } else {
            Err(windows::core::Error::from_thread())
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: CALLBACK_MESSAGE,
            ..Default::default()
        };
        if let Some(icon) = self.icon {
            data.uFlags |= NIF_ICON;
            data.hIcon = icon;
        }
        for (slot, unit) in data
            .szTip
            .iter_mut()
            .zip(self.tooltip.encode_utf16().take(127))
        {
            *slot = unit;
        }
        data
    }

    /// Center of the icon on screen, physical pixels.
    pub fn anchor(&self) -> Option<(i32, i32)> {
        let identifier = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        };
        let rect = unsafe { Shell_NotifyIconGetRect(&identifier) }.ok()?;
        Some(((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let data = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            if let Some(icon) = self.icon.take() {
                let _ = DestroyIcon(icon);
            }
            let _ = DestroyWindow(self.hwnd);
        }
        EVENTS.with(|slot| slot.borrow_mut().take());
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        CALLBACK_MESSAGE => {
            // NOTIFYICON_VERSION_4: event in LOWORD(lparam), anchor in wparam.
            let event = (lparam.0 as u32) & 0xffff;
            let x = (wparam.0 & 0xffff) as u16 as i16 as i32;
            let y = ((wparam.0 >> 16) & 0xffff) as u16 as i16 as i32;
            match event {
                NIN_SELECT | NIN_KEYSELECT => send(TrayEvent::Toggle { anchor: (x, y) }),
                WM_CONTEXTMENU => show_menu(hwnd, x, y),
                _ => {}
            }
            LRESULT(0)
        }
        // A polite external close request (for example `cargo dev` swapping builds) quits
        // cleanly so the icon is removed instead of lingering until hovered.
        WM_CLOSE => {
            send(TrayEvent::Quit);
            LRESULT(0)
        }
        REVEAL_MESSAGE => {
            send(TrayEvent::Reveal);
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            send(TrayEvent::SettingsChanged);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        message if message != 0 && message == TASKBAR_CREATED.with(Cell::get) => {
            send(TrayEvent::TaskbarCreated);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn show_menu(hwnd: HWND, x: i32, y: i32) {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, MENU_REFRESH, w!("Refresh"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_ABOUT, w!("About TokenBubble"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit TokenBubble"));
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenuEx(
            menu,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_NONOTIFY).0,
            x,
            y,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        let event = match command.0 as usize {
            MENU_REFRESH => Some(TrayEvent::Refresh),
            MENU_SETTINGS => Some(TrayEvent::Settings),
            MENU_ABOUT => Some(TrayEvent::About),
            MENU_QUIT => Some(TrayEvent::Quit),
            _ => None,
        };
        if let Some(event) = event {
            send(event);
        }
    }
}

/// Rasterize the installed Segoe UI Emoji glyph, retaining its native color and alpha.
fn create_icon(size: u32) -> windows::core::Result<HICON> {
    // GPUI initializes OLE on this thread before creating the tray host.
    let bitmap = unsafe {
        let imaging: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bitmap = imaging.CreateBitmap(
            size,
            size,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapCacheOnLoad,
        )?;
        let drawing: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let target = drawing.CreateWicBitmapRenderTarget(
            &bitmap,
            &D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            },
        )?;
        let writing: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let format = writing.CreateTextFormat(
            w!("Segoe UI Emoji"),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size as f32 * 0.9,
            w!("en-US"),
        )?;
        format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let brush = target.CreateSolidColorBrush(
            &D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            None,
        )?;
        target.BeginDraw();
        target.Clear(Some(&D2D1_COLOR_F::default()));
        target.DrawText(
            w!("\u{1FAE7}").as_wide(),
            &format,
            &D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: size as f32,
                bottom: size as f32,
            },
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        target.EndDraw(None, None)?;
        bitmap
    };
    let side = size as i32;
    let header = BITMAPV5HEADER {
        bV5Size: size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: side,
        // Negative height: top-down rows, matching the rasterizer.
        bV5Height: -side,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5RedMask: 0x00ff_0000,
        bV5GreenMask: 0x0000_ff00,
        bV5BlueMask: 0x0000_00ff,
        bV5AlphaMask: 0xff00_0000,
        ..Default::default()
    };
    unsafe {
        let dc = GetDC(None);
        let mut bits: *mut c_void = std::ptr::null_mut();
        let color = CreateDIBSection(
            Some(dc),
            &header as *const BITMAPV5HEADER as *const BITMAPINFO,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        );
        ReleaseDC(None, dc);
        let color = color?;
        let icon = (|| {
            // Copy directly into the icon bitmap, without an intermediate pixel allocation.
            bitmap.CopyPixels(
                std::ptr::null(),
                size * 4,
                std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize),
            )?;
            let mask_bytes = vec![0u8; size.div_ceil(16) as usize * 2 * size as usize];
            let mask = CreateBitmap(side, side, 1, 1, Some(mask_bytes.as_ptr() as *const c_void));
            if mask.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            let icon = CreateIconIndirect(&ICONINFO {
                fIcon: true.into(),
                hbmMask: mask,
                hbmColor: color,
                ..Default::default()
            });
            let _ = DeleteObject(HGDIOBJ(mask.0));
            icon
        })();
        let _ = DeleteObject(HGDIOBJ(color.0));
        icon
    }
}

/// Make `window` the foreground window. A tray click grants the icon's process this right;
/// without it the flyout never activates, so losing activation could not dismiss it.
pub fn bring_to_front(window: &gpui::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    if let RawWindowHandle::Win32(handle) = handle.as_raw() {
        let hwnd = HWND(handle.hwnd.get() as *mut c_void);
        let _ = unsafe { SetForegroundWindow(hwnd) };
    }
}

/// Held for the process lifetime; a second launch finds it and reveals this instance.
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// `None` when another instance already runs; that instance is asked to show its popup.
pub fn claim_instance() -> Option<InstanceGuard> {
    let handle = unsafe { CreateMutexW(None, false, INSTANCE_MUTEX) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = unsafe { CloseHandle(handle) };
        if let Ok(existing) = unsafe { FindWindowW(CLASS_NAME, PCWSTR::null()) } {
            // This launch came from the user, so it may pass foreground rights on; without
            // them the revealed flyout can't activate and click-away couldn't close it.
            let mut owner = 0;
            unsafe { GetWindowThreadProcessId(existing, Some(&mut owner)) };
            let _ = unsafe { AllowSetForegroundWindow(owner) };
            let _ = unsafe { PostMessageW(Some(existing), REVEAL_MESSAGE, WPARAM(0), LPARAM(0)) };
        }
        return None;
    }
    Some(InstanceGuard(handle))
}

/// Where to open a `width × height` (logical) popup near `anchor` (physical; the cursor
/// when absent). Returns the monitor handle as a GPUI display id and the logical origin
/// on that display.
pub fn popup_origin(
    anchor: Option<(i32, i32)>,
    width: f32,
    height: f32,
) -> Option<(u64, f32, f32)> {
    let anchor = anchor.or_else(|| {
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point) }.ok()?;
        Some((point.x, point.y))
    })?;
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: anchor.0,
                y: anchor.1,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.ok()?;
    let scale = dpi_x as f32 / 96.0;
    let rect = |r: RECT| Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    };
    let (x, y) = placement::popup_origin(
        anchor,
        rect(info.rcMonitor),
        rect(info.rcWork),
        (
            (width * scale).round() as i32,
            (height * scale).round() as i32,
        ),
        (12.0 * scale).round() as i32,
    );
    Some((monitor.0 as u64, x as f32 / scale, y as f32 / scale))
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("TokenBubble");

/// Whether a per-user Run entry starts TokenBubble at sign-in.
pub fn start_at_login() -> bool {
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    }
    .is_ok()
}

/// Add or remove the per-user Run entry for the current executable.
pub fn set_start_at_login(enabled: bool) -> std::io::Result<()> {
    let status = if enabled {
        let executable = std::env::current_exe()?;
        let command: Vec<u16> = format!("\"{}\"", executable.display())
            .encode_utf16()
            .chain([0])
            .collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(command.as_ptr() as *const c_void),
                (command.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE) }
    };
    if status.is_ok() || (!enabled && status.0 == 2) {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(status.0 as i32))
    }
}
