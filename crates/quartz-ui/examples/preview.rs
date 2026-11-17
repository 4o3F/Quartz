//! Opens the main window with mock data.
//!
//! `QUARTZ_PREVIEW_THEME=light|dark|system`, `QUARTZ_PREVIEW_STATE=today|today-details|today-hover`,
//! `QUARTZ_PREVIEW_LANG=zh-CN|en`, `QUARTZ_PREVIEW_OFFSCREEN=1` (the window is created hidden,
//! resized to 1280x800 logical, moved to (-32000, -32000) and shown without activation).
//!
//! GUI subsystem on Windows so launching it never opens a console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod offscreen {
    use quartz_ui::app::PlaceWindowError;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, ShowWindow,
        },
    };

    const FAR_AWAY: i32 = -32000;

    pub trait PreviewWindowExt {
        fn place_offscreen(&self) -> Result<(), PlaceWindowError>;
    }
    impl PreviewWindowExt for gpui_kit::Window {
        /// Moves the hidden window outside every display, then shows it without activating it.
        fn place_offscreen(&self) -> Result<(), PlaceWindowError> {
            let handle =
                HasWindowHandle::window_handle(self).map_err(|_| PlaceWindowError::WindowHandle)?;
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                return Err(PlaceWindowError::WindowHandle);
            };
            let hwnd = HWND(handle.hwnd.get() as *mut core::ffi::c_void);
            // SAFETY: `hwnd` is the live top-level window of this process, taken from GPUI just now;
            // the call only moves it, and the flags forbid resizing, restacking and activation.
            unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    FAR_AWAY,
                    FAR_AWAY,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
                )
            }
            .map_err(|error| PlaceWindowError::Platform {
                source: anyhow::Error::from(error),
            })?;
            // SAFETY: same live window handle; SW_SHOWNOACTIVATE shows it without taking focus. The
            // return value is the previous visibility, not an error.
            let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
            Ok(())
        }
    }
}

#[cfg(windows)]
fn main() -> Result<(), quartz_ui::app::RunPreviewError> {
    use quartz_ui::app::{PreviewOptions, PreviewPlacement, PreviewState, ThemeChoice};

    let theme = match std::env::var("QUARTZ_PREVIEW_THEME").as_deref() {
        Ok("light") => ThemeChoice::Light,
        Ok("dark") => ThemeChoice::Dark,
        _ => ThemeChoice::System,
    };
    let state = match std::env::var("QUARTZ_PREVIEW_STATE").as_deref() {
        Ok("today-details") => PreviewState::TodayDetails,
        Ok("today-hover") => PreviewState::TodayHover,
        _ => PreviewState::Today,
    };
    let language = match std::env::var("QUARTZ_PREVIEW_LANG").as_deref() {
        Ok("en") => "en",
        _ => "zh-CN",
    };
    let placement = match std::env::var("QUARTZ_PREVIEW_OFFSCREEN").as_deref() {
        Ok("1") => PreviewPlacement::Offscreen(
            <gpui_kit::Window as offscreen::PreviewWindowExt>::place_offscreen,
        ),
        _ => PreviewPlacement::Visible,
    };
    (PreviewOptions {
        placement,
        language,
        theme,
        state,
    })
    .run()?;
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
