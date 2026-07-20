//! Sets the top-level window's caption/taskbar icon from the icon embedded in
//! the exe (resource id 1). WinUI 3 doesn't adopt the exe icon for the title-bar
//! automatically, so we push it in via Win32 once the window exists.

use windows::core::{BOOL, PCWSTR};
use windows::libloaderapi::GetModuleHandleW;
use windows::minwindef::{LPARAM, WPARAM};
use windows::processthreadsapi::GetCurrentThreadId;
use windows::windef::{HICON, HWND};
use windows::winuser::*;

const APP_ICON_ID: u16 = 1;
const WINDOW_TITLE: &str = "Network Monitor";

pub fn set_app_window_icon() {
    unsafe {
        let Some(hwnd) = find_main_window() else {
            return;
        };
        let hinst = GetModuleHandleW(PCWSTR::null());
        if hinst.0.is_null() {
            return;
        }

        let load = |cx: i32, cy: i32| -> Option<HICON> {
            let h = LoadImageW(
                Some(hinst.into()),
                PCWSTR(APP_ICON_ID as usize as *const u16),
                IMAGE_ICON,
                cx,
                cy,
                LR_DEFAULTCOLOR,
            );
            if h.0.is_null() {
                None
            } else {
                Some(HICON(h.0 as _))
            }
        };

        if let Some(big) = load(
            GetSystemMetrics(SM_CXICON as i32),
            GetSystemMetrics(SM_CYICON as i32),
        ) {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(big.0 as isize),
            );
        }
        if let Some(small) = load(
            GetSystemMetrics(SM_CXSMICON as i32),
            GetSystemMetrics(SM_CYSMICON as i32),
        ) {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_SMALL as usize),
                LPARAM(small.0 as isize),
            );
        }
    }
}

unsafe fn find_main_window() -> Option<HWND> {
    unsafe {
        let mut found = HWND::default();
        let _ = EnumThreadWindows(
            GetCurrentThreadId(),
            Some(enum_proc),
            LPARAM(&mut found as *mut HWND as isize),
        );
        if found.0.is_null() { None } else { Some(found) }
    }
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        // Top-level windows only (no owner).
        if !GetWindow(hwnd, GW_OWNER).0.is_null() {
            return BOOL(1);
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf);
        if String::from_utf16_lossy(&buf[..n as usize]) == WINDOW_TITLE {
            *(lparam.0 as *mut HWND) = hwnd;
            return BOOL(0);
        }
        BOOL(1)
    }
}
