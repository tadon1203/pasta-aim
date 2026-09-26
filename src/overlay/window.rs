//! The overlay's Win32 window and message pump.
//!
//! The window is transparent to input, never takes focus, stays on top, and is
//! excluded from screen capture so Desktop Duplication (and therefore the
//! model) never sees the boxes it draws.

use std::time::Duration;

use anyhow::{Context, Result};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
            LibraryLoader::GetModuleHandleW,
            Threading::{GetCurrentThreadId, INFINITE},
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HMENU,
                HTTRANSPARENT, HWND_TOPMOST, MSG, MsgWaitForMultipleObjects, PM_NOREMOVE,
                PM_REMOVE, PeekMessageW, PostThreadMessageW, QS_ALLINPUT, RegisterClassExW,
                SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SetWindowDisplayAffinity, SetWindowPos,
                ShowWindow, TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_NCHITTEST,
                WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::w,
};

/// Thread message that tells the overlay thread its shared state changed.
const WAKE: u32 = WM_APP;

/// Per-thread setup for the overlay thread: a COM apartment, physical-pixel
/// coordinates, and a message queue (so other threads can post to it).
pub struct ThreadInit;

impl ThreadInit {
    pub fn new() -> Result<Self> {
        // SAFETY: called once on the overlay thread and balanced by Drop.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .context("failed to initialize COM on the overlay thread")?;
        // SAFETY: plain per-thread setting; the previous context is not needed.
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let mut message = MSG::default();
        // SAFETY: peeking without removal only creates the thread's message queue.
        let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
        Ok(Self)
    }
}

impl Drop for ThreadInit {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx in `new` on this thread.
        unsafe { CoUninitialize() };
    }
}

pub fn current_thread_id() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetCurrentThreadId() }
}

/// Wakes the overlay thread. Failure means the thread is already gone.
pub fn post_wake(thread_id: u32) {
    // SAFETY: posting a payload-free message to a thread id has no memory effects.
    let _ = unsafe { PostThreadMessageW(thread_id, WAKE, WPARAM(0), LPARAM(0)) };
}

/// Sleeps until a wake message, other window input, or `timeout`, then drains
/// the queue.
pub fn wait_and_pump(timeout: Option<Duration>) {
    let millis = timeout.map_or(INFINITE, |timeout| {
        u32::try_from(timeout.as_millis().max(1)).map_or(INFINITE - 1, |ms| ms.min(INFINITE - 1))
    });
    // SAFETY: waits on this thread's own message queue only.
    unsafe { MsgWaitForMultipleObjects(None, false, millis, QS_ALLINPUT) };
    let mut message = MSG::default();
    // SAFETY: `message` is a valid out buffer; messages are dispatched on the
    // thread that owns the windows.
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            if message.message != WAKE {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    // SAFETY: forwards the arguments this procedure received.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

pub struct OverlayWindow {
    hwnd: HWND,
}

impl OverlayWindow {
    /// Creates a hidden window at `(left, top, width, height)`.
    pub fn create((left, top, width, height): (i32, i32, u32, u32)) -> Result<Self> {
        // SAFETY: standard window class registration and creation on the
        // thread that pumps this window's messages.
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None)?.0);
            let class_name = w!("nice-aimassist-overlay");
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class_name,
                ..Default::default()
            };
            // Fails harmlessly when the class exists from an earlier window.
            RegisterClassExW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOREDIRECTIONBITMAP,
                class_name,
                w!("nice-aimassist overlay"),
                WS_POPUP,
                left,
                top,
                width as i32,
                height as i32,
                HWND::default(),
                HMENU::default(),
                instance,
                None,
            );
            if hwnd.0 == 0 {
                return Err(windows::core::Error::from_win32())
                    .context("failed to create the overlay window");
            }
            let window = Self { hwnd };
            // Keeps the boxes out of Desktop Duplication so the model never
            // detects the overlay itself.
            SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
                .context("failed to exclude the overlay from screen capture")?;
            Ok(window)
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Moves and resizes the window, keeping it topmost without activating it.
    pub fn place(&self, (left, top, width, height): (i32, i32, u32, u32)) -> Result<()> {
        // SAFETY: the handle belongs to this live window.
        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                left,
                top,
                width as i32,
                height as i32,
                SWP_NOACTIVATE,
            )
        }
        .context("failed to place the overlay window")
    }

    pub fn set_visible(&self, visible: bool) {
        // SAFETY: the handle belongs to this live window.
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // SAFETY: destroys the window this value created, on its own thread.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
