//! Windows appearance events marshalled to the GPUI foreground thread (11 sections 2 and 3.2).
//!
//! Transparency: https://learn.microsoft.com/uwp/api/windows.ui.viewmanagement.uisettings.advancedeffectsenabled
//! Contrast broadcasts: https://learn.microsoft.com/windows/win32/winmsg/wm-settingchange

mod notification_window;

use notification_window::NotificationWindow;

use super::{
    runtime::ThemeBundle,
    theme_mapping::ComponentMapping,
    tokens::{AccentColors, Material, SystemColors, Tokens},
};
use gpui_kit::{
    App, Global, Hsla, Rgba, Task, Window, WindowBackgroundAppearance,
    component::{Theme, ThemeMode},
};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle};
use snafu::{ResultExt as _, Snafu};
use std::{
    future::poll_fn,
    rc::Rc,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};
use windows::{
    Foundation::{EventHandler, TypedEventHandler},
    System::Power::{EnergySaverStatus, PowerManager},
    UI::ViewManagement::{UIColorType, UISettings},
    Wdk::System::SystemServices::RtlGetVersion,
    Win32::{
        Foundation::HWND,
        Graphics::{
            Dwm::{
                DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
                DwmExtendFrameIntoClientArea, DwmGetWindowAttribute, DwmSetWindowAttribute,
            },
            Gdi::{
                COLOR_BTNFACE, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
                COLOR_WINDOWTEXT, GetSysColor, SYS_COLOR_INDEX,
            },
        },
        System::SystemInformation::OSVERSIONINFOW,
        UI::{
            Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
            Controls::MARGINS,
            WindowsAndMessaging::{
                SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
            },
        },
    },
};

/// Windows 11 build introducing DWMWA_SYSTEMBACKDROP_TYPE (11 section 2).
const MICA_MIN_BUILD: u32 = 22621;
/// DWM extends the frame through the entire custom client area with negative margins (11 section 2).
const MICA_FRAME_MARGIN: i32 = -1;
/// Byte channel normalization for system colors (11 section 3.1).
const COLOR_CHANNEL_MAX: f32 = 255.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackdropSupport {
    Supported,
    Unsupported { build: u32 },
    Failed,
}

#[derive(Debug, Clone, Copy)]
enum DwmOperation {
    SetDarkMode,
    ExtendFrame,
    ReadBackdrop,
}

#[derive(Debug, Snafu)]
enum ApplyBackdropError {
    #[snafu(display("could not obtain the window handle"))]
    GetWindowHandle { source: HandleError },
    #[snafu(display("the window handle is not a Win32 handle"))]
    RequireWin32Handle,
    #[snafu(display("could not read the Windows build number"))]
    ReadWindowsBuild { source: windows::core::Error },
    #[snafu(display("DWM operation {operation:?} failed"))]
    CallDwm {
        operation: DwmOperation,
        source: windows::core::Error,
    },
    #[snafu(display("GPUI requested Mica but DWM reports backdrop {actual}"))]
    VerifyBackdrop { actual: i32 },
}

#[derive(Debug)]
struct NativeBackdrop {
    hwnd: HWND,
}
impl NativeBackdrop {
    fn new(window: &Window) -> Result<Self, ApplyBackdropError> {
        let handle = HasWindowHandle::window_handle(window).context(GetWindowHandleSnafu)?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return RequireWin32HandleSnafu.fail();
        };
        Ok(Self {
            hwnd: HWND(std::ptr::with_exposed_provenance_mut(
                handle.hwnd.get().cast_unsigned(),
            )),
        })
    }
    fn support() -> Result<BackdropSupport, ApplyBackdropError> {
        let mut version = OSVERSIONINFOW {
            dwOSVersionInfoSize: u32::try_from(std::mem::size_of::<OSVERSIONINFOW>())
                .expect("OSVERSIONINFOW fits u32"),
            ..Default::default()
        };
        // SAFETY: the writable OSVERSIONINFOW has its documented size set before this call.
        unsafe { RtlGetVersion(&mut version) }
            .ok()
            .context(ReadWindowsBuildSnafu)?;
        Ok(if version.dwBuildNumber >= MICA_MIN_BUILD {
            BackdropSupport::Supported
        } else {
            BackdropSupport::Unsupported {
                build: version.dwBuildNumber,
            }
        })
    }
    fn apply(
        &self,
        window: &Window,
        material: Material,
        mode: ThemeMode,
    ) -> Result<(), ApplyBackdropError> {
        let dark = windows::core::BOOL::from(mode.is_dark());
        // SAFETY: this is GPUI's live HWND; DWM copies the BOOL during the call.
        unsafe {
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                std::ptr::from_ref(&dark).cast(),
                u32::try_from(std::mem::size_of_val(&dark)).expect("BOOL fits u32"),
            )
        }
        .context(CallDwmSnafu {
            operation: DwmOperation::SetDarkMode,
        })?;
        // GPUI is the sole writer of DWMWA_SYSTEMBACKDROP_TYPE.
        window.set_background_appearance(match material {
            Material::Mica => WindowBackgroundAppearance::MicaBackdrop,
            Material::Solid => WindowBackgroundAppearance::Opaque,
        });
        let margin = match material {
            Material::Mica => MICA_FRAME_MARGIN,
            Material::Solid => 0,
        };
        let margins = MARGINS {
            cxLeftWidth: margin,
            cxRightWidth: margin,
            cyTopHeight: margin,
            cyBottomHeight: margin,
        };
        // SAFETY: the margins live for this call; the HWND belongs to the current window.
        unsafe { DwmExtendFrameIntoClientArea(self.hwnd, &margins) }.context(CallDwmSnafu {
            operation: DwmOperation::ExtendFrame,
        })?;
        if material == Material::Mica {
            let mut actual = 0_i32;
            // SAFETY: DWM writes exactly one enum-sized integer into this valid buffer.
            unsafe {
                DwmGetWindowAttribute(
                    self.hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE,
                    std::ptr::from_mut(&mut actual).cast(),
                    u32::try_from(std::mem::size_of_val(&actual)).expect("DWM enum fits u32"),
                )
            }
            .context(CallDwmSnafu {
                operation: DwmOperation::ReadBackdrop,
            })?;
            if actual != DWMSBT_MAINWINDOW.0 {
                return VerifyBackdropSnafu { actual }.fail();
            }
        }
        Ok(())
    }
}

impl SystemColors {
    fn read() -> Self {
        Self {
            window: Self::color(COLOR_WINDOW),
            window_text: Self::color(COLOR_WINDOWTEXT),
            highlight: Self::color(COLOR_HIGHLIGHT),
            highlight_text: Self::color(COLOR_HIGHLIGHTTEXT),
            gray_text: Self::color(COLOR_GRAYTEXT),
            button_face: Self::color(COLOR_BTNFACE),
        }
    }
    fn color(index: SYS_COLOR_INDEX) -> Hsla {
        // SAFETY: the supplied indices are documented GetSysColor constants.
        let [red, green, blue, _] = unsafe { GetSysColor(index) }.to_le_bytes();
        Self::channels(red, green, blue)
    }
    fn channels(red: u8, green: u8, blue: u8) -> Hsla {
        Rgba {
            r: f32::from(red) / COLOR_CHANNEL_MAX,
            g: f32::from(green) / COLOR_CHANNEL_MAX,
            b: f32::from(blue) / COLOR_CHANNEL_MAX,
            a: 1.,
        }
        .into()
    }
}
impl AccentColors {
    fn read(ui: &UISettings) -> windows::core::Result<Self> {
        let read = |kind| -> windows::core::Result<Hsla> {
            let value = ui.GetColorValue(kind)?;
            Ok(SystemColors::channels(value.R, value.G, value.B))
        };
        Ok(Self {
            accent: read(UIColorType::Accent)?,
            light1: read(UIColorType::AccentLight1)?,
            light2: read(UIColorType::AccentLight2)?,
            light3: read(UIColorType::AccentLight3)?,
            dark1: read(UIColorType::AccentDark1)?,
            dark2: read(UIColorType::AccentDark2)?,
            dark3: read(UIColorType::AccentDark3)?,
        })
    }
}

/// Sources share one foreground refresh, which re-reads the current Windows settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeChange {
    Settings,
    SystemColors,
    AccentColors,
    Transparency,
    EnergySaver,
}

impl ThemeChange {
    fn refresh(self, cx: &mut App) {
        match self {
            Self::Settings
            | Self::SystemColors
            | Self::AccentColors
            | Self::Transparency
            | Self::EnergySaver => SystemTheme::refresh(cx),
        }
    }
}

#[derive(Debug, Default)]
struct EventState {
    pending: Option<ThemeChange>,
    waker: Option<Waker>,
}

/// Coalesces event bursts without polling or a timer, preserving the last system state.
#[derive(Debug, Clone, Default)]
struct ThemeEvents(Arc<Mutex<EventState>>);

impl ThemeEvents {
    fn signal(&self, change: ThemeChange) {
        let waker = {
            let mut state = self.0.lock().expect("theme event lock poisoned");
            state.pending = Some(change);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    async fn next(&self) -> ThemeChange {
        poll_fn(|cx| {
            let mut state = self.0.lock().expect("theme event lock poisoned");
            if let Some(change) = state.pending.take() {
                Poll::Ready(change)
            } else {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

/// Owns event registrations until application shutdown; callbacks capture only the event signal.
#[derive(Debug, Default)]
struct SettingsEvents {
    ui: Option<UISettings>,
    colors: Option<i64>,
    effects: Option<i64>,
    energy: Option<i64>,
    #[cfg(test)]
    drop_probe: Option<tests::DropProbe>,
}

impl SettingsEvents {
    fn register(events: &ThemeEvents) -> Self {
        let mut subscriptions = Self::default();
        match UISettings::new() {
            Ok(ui) => {
                let signal = events.clone();
                subscriptions.colors = Self::registration(
                    ui.ColorValuesChanged(&TypedEventHandler::new(move |_, _| {
                        signal.signal(ThemeChange::AccentColors);
                        Ok(())
                    })),
                    "ColorValuesChanged",
                );
                let signal = events.clone();
                subscriptions.effects = Self::registration(
                    ui.AdvancedEffectsEnabledChanged(&TypedEventHandler::new(move |_, _| {
                        signal.signal(ThemeChange::Transparency);
                        Ok(())
                    })),
                    "AdvancedEffectsEnabledChanged",
                );
                subscriptions.ui = Some(ui);
            }
            Err(error) => {
                tracing::warn!(%error, "could not create UISettings; using embedded accent and solid background")
            }
        }
        let signal = events.clone();
        subscriptions.energy = Self::registration(
            PowerManager::EnergySaverStatusChanged(&EventHandler::new(move |_, _| {
                signal.signal(ThemeChange::EnergySaver);
                Ok(())
            })),
            "EnergySaverStatusChanged",
        );
        subscriptions
    }

    fn registration(result: windows::core::Result<i64>, event: &str) -> Option<i64> {
        match result {
            Ok(token) => Some(token),
            Err(error) => {
                tracing::warn!(%error, event, "could not register appearance event; refreshing on window activation");
                None
            }
        }
    }
}

impl Drop for SettingsEvents {
    fn drop(&mut self) {
        if let Some(ui) = &self.ui {
            if let Some(token) = self.colors {
                Self::removed(ui.RemoveColorValuesChanged(token), "ColorValuesChanged");
            }
            if let Some(token) = self.effects {
                Self::removed(
                    ui.RemoveAdvancedEffectsEnabledChanged(token),
                    "AdvancedEffectsEnabledChanged",
                );
            }
        }
        if let Some(token) = self.energy {
            Self::removed(
                PowerManager::RemoveEnergySaverStatusChanged(token),
                "EnergySaverStatusChanged",
            );
        }
        #[cfg(test)]
        drop(self.drop_probe.take());
    }
}

impl SettingsEvents {
    fn removed(result: windows::core::Result<()>, event: &str) {
        if let Err(error) = result {
            tracing::warn!(%error, event, "could not unregister appearance event");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeUpdate {
    Unchanged,
    Changed,
}

/// Embedded typed defaults and a resolved palette; system changes never decode JSON.
#[derive(Debug)]
pub(crate) struct SystemTheme {
    base: Rc<Tokens>,
    tokens: Rc<Tokens>,
    mapping: ComponentMapping,
    material: Material,
    applied_mode: Option<ThemeMode>,
    support: BackdropSupport,
    subscriptions: Option<SettingsEvents>,
    notifications: Option<NotificationWindow>,
    listener: Option<Task<()>>,
    #[cfg(feature = "preview")]
    preview_colors: SystemColors,
}
impl Global for SystemTheme {}
impl SystemTheme {
    pub(crate) fn install(
        theme: ThemeBundle,
        appearance: super::runtime::Appearance,
        cx: &mut App,
    ) {
        let events = ThemeEvents::default();
        let subscriptions = SettingsEvents::register(&events);
        let notifications = match NotificationWindow::create(&events) {
            Ok(window) => Some(window),
            Err(error) => {
                tracing::warn!(%error, "could not observe desktop appearance broadcasts; refreshing on window activation");
                None
            }
        };
        let support = match NativeBackdrop::support() {
            Ok(support) => support,
            Err(error) => {
                tracing::warn!(%error,"could not read backdrop support; using bg.base");
                BackdropSupport::Failed
            }
        };
        if let BackdropSupport::Unsupported { build } = support {
            tracing::warn!(
                build,
                "system backdrop requires Windows build 22621; using bg.base"
            );
        }
        theme.install(cx);
        cx.set_global(Self {
            base: theme.tokens(),
            tokens: theme.tokens(),
            mapping: theme.mapping,
            material: Material::Solid,
            applied_mode: None,
            support,
            subscriptions: Some(subscriptions),
            notifications,
            listener: None,
            #[cfg(feature = "preview")]
            preview_colors: PreviewTheme::load_colors(),
        });
        cx.on_app_quit(|cx| {
            cx.global_mut::<Self>().shutdown();
            std::future::ready(())
        })
        .detach();
        appearance.apply(cx);
        Self::refresh(cx);
        let listener = cx.spawn(async move |cx| {
            loop {
                let change = events.next().await;
                cx.update(|cx| change.refresh(cx));
            }
        });
        cx.global_mut::<Self>().listener = Some(listener);
    }
    fn shutdown(&mut self) {
        // GPUI drops its Windows platform (and uninitializes OLE) before app globals.
        // Its quit observers run earlier, so revoke events and release WinRT objects here.
        drop(self.listener.take());
        drop(self.notifications.take());
        drop(self.subscriptions.take());
    }
    pub(crate) fn tokens(cx: &App) -> Rc<Tokens> {
        Rc::clone(&cx.global::<Self>().tokens)
    }
    pub(crate) fn window_appearance(cx: &App) -> WindowBackgroundAppearance {
        match cx.global::<Self>().material {
            Material::Mica => WindowBackgroundAppearance::MicaBackdrop,
            Material::Solid => WindowBackgroundAppearance::Opaque,
        }
    }
    fn high_contrast() -> windows::core::Result<bool> {
        let mut setting = HIGHCONTRASTW {
            cbSize: u32::try_from(std::mem::size_of::<HIGHCONTRASTW>())
                .expect("HIGHCONTRASTW fits u32"),
            ..Default::default()
        };
        // SAFETY: the setting is writable and has the documented HIGHCONTRASTW size.
        unsafe {
            SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                setting.cbSize,
                Some(std::ptr::from_mut(&mut setting).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
            )
        }?;
        Ok(setting.dwFlags.contains(HCF_HIGHCONTRASTON))
    }
    fn contrast_colors(&self, _cx: &App) -> windows::core::Result<Option<SystemColors>> {
        #[cfg(feature = "preview")]
        if *_cx.global::<PreviewTheme>() == PreviewTheme::HighContrast {
            return Ok(Some(self.preview_colors.clone()));
        }
        Self::high_contrast().map(|enabled| enabled.then(SystemColors::read))
    }
    /// The window owner installs one activation refresh; child views only observe their text size.
    pub(crate) fn observe_activation<T: 'static>(window: &mut Window, cx: &gpui_kit::Context<T>) {
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                Self::refresh(cx);
            }
        })
        .detach();
    }
    fn replace_resolved(
        &mut self,
        tokens: Tokens,
        material: Material,
        mode: ThemeMode,
    ) -> ThemeUpdate {
        if *self.tokens == tokens && self.material == material && self.applied_mode == Some(mode) {
            return ThemeUpdate::Unchanged;
        }
        self.tokens = Rc::new(tokens);
        self.material = material;
        self.applied_mode = Some(mode);
        ThemeUpdate::Changed
    }
    pub(crate) fn refresh(cx: &mut App) {
        let state = cx.global::<Self>();
        let Some(subscriptions) = &state.subscriptions else {
            return;
        };
        let contrast = match state.contrast_colors(cx) {
            Ok(colors) => colors,
            Err(error) => {
                tracing::warn!(%error,"could not read high contrast; retaining current theme");
                return;
            }
        };
        let mut material = Material::Solid;
        let mut accent = None;
        if let Some(ui) = &subscriptions.ui {
            match AccentColors::read(ui) {
                Ok(colors) => accent = Some(colors),
                Err(error) => {
                    tracing::warn!(%error,"could not read system accent; using embedded accent")
                }
            }
            match ui.AdvancedEffectsEnabled() {
                Ok(true) => material = Material::Mica,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error,"could not read transparency effects; using bg.base")
                }
            }
        }
        match PowerManager::EnergySaverStatus() {
            Ok(status) if status == EnergySaverStatus::On => material = Material::Solid,
            Ok(_) => {}
            Err(error) => {
                material = Material::Solid;
                tracing::warn!(%error,"could not read energy saver; using bg.base");
            }
        }
        #[cfg(feature = "preview")]
        if *cx.global::<PreviewTheme>() == PreviewTheme::Fallback {
            material = Material::Solid;
        }
        if contrast.is_some() || state.support != BackdropSupport::Supported {
            material = Material::Solid;
        }
        let tokens = state
            .base
            .resolved(accent.as_ref(), material, contrast.as_ref());
        let mode = Theme::global(cx).mode;
        if cx
            .global_mut::<Self>()
            .replace_resolved(tokens, material, mode)
            == ThemeUpdate::Unchanged
        {
            return;
        }
        Self::apply_components(cx);
        cx.defer(|cx| {
            for handle in cx.windows() {
                if let Err(error) =
                    handle.update(cx, |_, window, cx| Self::apply_window(window, cx))
                {
                    tracing::warn!(%error,"could not update window material");
                }
            }
        });
    }
    /// Writes typed palette colors into both the component and Base projections, including opaque HC selections.
    pub(crate) fn apply_components(cx: &mut App) {
        let state = cx.global::<Self>();
        let tokens = Rc::clone(&state.tokens);
        let mapping = state.mapping.clone();
        Theme::update(cx, |theme| mapping.apply(tokens.palette(theme.mode), theme));
    }
    pub(crate) fn observe_window(window: &mut Window, cx: &mut App) {
        Self::apply_window(window, cx);
    }
    fn apply_window(window: &mut Window, cx: &mut App) {
        let material = cx.global::<Self>().material;
        let result = NativeBackdrop::new(window)
            .and_then(|native| native.apply(window, material, Theme::global(cx).mode));
        if let Err(error) = result {
            tracing::warn!(%error,"could not configure backdrop; using bg.base");
            let state = cx.global_mut::<Self>();
            state.support = BackdropSupport::Failed;
            state.material = Material::Solid;
            state.tokens = Rc::new(state.tokens.resolved(None, Material::Solid, None));
            Self::apply_components(cx);
            window.set_background_appearance(WindowBackgroundAppearance::Opaque);
        }
        #[cfg(feature = "preview")]
        Self::diagnose_next_frame(window);
        window.refresh();
    }
    #[cfg(feature = "preview")]
    fn diagnose_next_frame(window: &Window) {
        let Some(path) = std::env::var_os("QUARTZ_PREVIEW_DIAGNOSTICS") else {
            return;
        };
        window.on_next_frame(move |window, cx| {
            let report = PreviewDiagnostics::capture(window, cx);
            if let Err(error) = report.write(&std::path::PathBuf::from(path)) {
                tracing::warn!(%error,"could not write preview diagnostics");
            }
        });
    }
}

#[cfg(feature = "preview")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewTheme {
    #[default]
    System,
    Fallback,
    HighContrast,
}
#[cfg(feature = "preview")]
impl Global for PreviewTheme {}
#[cfg(feature = "preview")]
impl PreviewTheme {
    fn load_colors() -> SystemColors {
        serde_json::from_str(include_str!("../../themes/preview-high-contrast.json"))
            .expect("preview-only high contrast colors")
    }
}

#[cfg(feature = "preview")]
#[derive(Debug, serde::Serialize)]
struct PaintLayer {
    name: &'static str,
    color: Hsla,
    alpha: f32,
    paint_transparent: bool,
}
#[cfg(feature = "preview")]
impl PaintLayer {
    fn color(name: &'static str, color: Hsla) -> Self {
        Self {
            name,
            color,
            alpha: color.a,
            paint_transparent: color.a == 0.,
        }
    }
}
#[cfg(feature = "preview")]
#[derive(Debug, serde::Serialize)]
struct PreviewDiagnostics {
    active: bool,
    material: &'static str,
    bg_base: Hsla,
    frame_extended: bool,
    layers: [PaintLayer; 6],
}
#[cfg(feature = "preview")]
#[derive(Debug, Snafu)]
enum WritePreviewDiagnosticsError {
    #[snafu(display("could not encode preview diagnostics"))]
    EncodeDiagnostics { source: serde_json::Error },
    #[snafu(display("could not write preview diagnostics"))]
    WriteDiagnostics { source: std::io::Error },
}
#[cfg(feature = "preview")]
impl PreviewDiagnostics {
    fn capture(window: &Window, cx: &App) -> Self {
        let theme = Theme::global(cx);
        let state = cx.global::<SystemTheme>();
        let palette = state.tokens.palette(theme.mode);
        let root = PaintLayer {
            name: "gpui-component Root",
            color: theme.tokens.background.color,
            alpha: theme.tokens.background.color.a,
            paint_transparent: theme.tokens.background.background.is_transparent(),
        };
        Self {
            active: window.is_window_active(),
            material: match state.material {
                Material::Mica => "Mica",
                Material::Solid => "Solid",
            },
            bg_base: palette.bg_base,
            frame_extended: state.material == Material::Mica,
            layers: [
                PaintLayer::color("Quartz root / custom TitleBar", palette.window_background),
                root,
                PaintLayer::color("component theme.background", theme.background),
                PaintLayer::color("component theme.title_bar", theme.title_bar),
                PaintLayer::color("component theme.sidebar", theme.sidebar),
                PaintLayer::color(
                    "gpui-base background",
                    gpui_kit::base::Theme::global(cx).tokens.colors.background,
                ),
            ],
        }
    }
    fn write(&self, path: &std::path::Path) -> Result<(), WritePreviewDiagnosticsError> {
        let bytes = serde_json::to_vec_pretty(self).context(EncodeDiagnosticsSnafu)?;
        std::fs::write(path, bytes).context(WriteDiagnosticsSnafu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        future::Future as _,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Wake},
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ApartmentState {
        Initialized,
        Uninitialized,
    }

    #[derive(Debug)]
    pub(super) struct DropProbe {
        apartment: Rc<Cell<ApartmentState>>,
        drops: Rc<Cell<usize>>,
    }
    impl Drop for DropProbe {
        fn drop(&mut self) {
            assert_eq!(self.apartment.get(), ApartmentState::Initialized);
            self.drops.set(self.drops.get() + 1);
        }
    }

    #[test]
    fn initial_refresh_follows_appearance_selection() {
        let source = include_str!("system_theme.rs");
        let install = source
            .split("pub(crate) fn install(")
            .nth(1)
            .expect("install method")
            .split("    fn shutdown(")
            .next()
            .expect("install body");
        let appearance = install
            .find("appearance.apply(cx)")
            .expect("initial appearance selection");
        let refresh = install.find("Self::refresh(cx)").expect("initial refresh");
        assert!(appearance < refresh);
    }

    #[test]
    fn quit_releases_subscriptions_before_platform_teardown_and_global_drop() {
        let apartment = Rc::new(Cell::new(ApartmentState::Initialized));
        let drops = Rc::new(Cell::new(0));
        let released = Rc::new(RefCell::new(Vec::new()));
        let theme = ThemeBundle::embedded().expect("embedded typed theme");
        let subscriptions = SettingsEvents {
            ui: None,
            colors: None,
            effects: None,
            energy: None,
            drop_probe: Some(DropProbe {
                apartment: Rc::clone(&apartment),
                drops: Rc::clone(&drops),
            }),
        };
        let mut state = SystemTheme {
            base: theme.tokens(),
            tokens: theme.tokens(),
            mapping: theme.mapping,
            material: Material::Solid,
            applied_mode: Some(ThemeMode::Light),
            support: BackdropSupport::Supported,
            subscriptions: Some(subscriptions),
            notifications: Some(NotificationWindow::for_shutdown_test(Rc::clone(&released))),
            listener: Some(Task::ready(())),
            #[cfg(feature = "preview")]
            preview_colors: PreviewTheme::load_colors(),
        };

        state.shutdown();
        assert!(state.subscriptions.is_none());
        assert!(state.notifications.is_none());
        use notification_window::tests::ReleasedResource;
        assert_eq!(
            *released.borrow(),
            [ReleasedResource::Window, ReleasedResource::Class]
        );
        assert!(state.listener.is_none());
        assert_eq!(drops.get(), 1);

        apartment.set(ApartmentState::Uninitialized);
        state.shutdown();
        drop(state);
        assert_eq!(drops.get(), 1, "global drop must not release WinRT again");
        assert_eq!(
            *released.borrow(),
            [ReleasedResource::Window, ReleasedResource::Class]
        );
    }

    #[derive(Debug, Default)]
    struct WakeCounter(AtomicUsize);
    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn injected_theme_reads_apply_only_changed_resolved_states() {
        let theme = ThemeBundle::embedded().expect("theme");
        let base = theme.tokens();
        let mut state = SystemTheme {
            base: Rc::clone(&base),
            tokens: Rc::clone(&base),
            mapping: theme.mapping,
            material: Material::Solid,
            applied_mode: Some(ThemeMode::Light),
            support: BackdropSupport::Supported,
            subscriptions: None,
            notifications: None,
            listener: None,
            #[cfg(feature = "preview")]
            preview_colors: PreviewTheme::load_colors(),
        };
        let reads = Cell::new(0);
        let component_updates = Cell::new(0);
        let window_updates = Cell::new(0);
        let mut refresh = |material, mode, colors: Option<&SystemColors>| {
            reads.set(reads.get() + 1);
            let resolved = base.resolved(None, material, colors);
            if state.replace_resolved(resolved, material, mode) == ThemeUpdate::Changed {
                component_updates.set(component_updates.get() + 1);
                window_updates.set(window_updates.get() + 1);
            }
        };
        refresh(Material::Solid, ThemeMode::Light, None);
        refresh(Material::Solid, ThemeMode::Light, None);
        assert_eq!(component_updates.get(), 0);
        let colors = PreviewTheme::load_colors();
        refresh(Material::Solid, ThemeMode::Light, Some(&colors));
        refresh(Material::Solid, ThemeMode::Light, Some(&colors));
        assert_eq!(component_updates.get(), 1);
        refresh(Material::Mica, ThemeMode::Light, None);
        refresh(Material::Mica, ThemeMode::Light, None);
        assert_eq!(reads.get(), 6);
        assert_eq!(component_updates.get(), 2);
        assert_eq!(window_updates.get(), 2);
        refresh(Material::Mica, ThemeMode::Dark, None);
        refresh(Material::Mica, ThemeMode::Dark, None);
        assert_eq!(
            component_updates.get(),
            3,
            "mode changes must update DWM even with identical light/dark token bundles"
        );
        assert_eq!(window_updates.get(), 3);
    }

    #[test]
    fn system_events_wake_the_foreground_future_and_coalesce() {
        let events = ThemeEvents::default();
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&counter));
        let mut context = Context::from_waker(&waker);
        let mut next = std::pin::pin!(events.next());
        assert!(next.as_mut().poll(&mut context).is_pending());
        events.signal(ThemeChange::Settings);
        events.signal(ThemeChange::SystemColors);
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            next.as_mut().poll(&mut context),
            Poll::Ready(ThemeChange::SystemColors)
        );
        let mut next = std::pin::pin!(events.next());
        assert!(next.as_mut().poll(&mut context).is_pending());
        events.signal(ThemeChange::Settings);
        assert_eq!(
            next.as_mut().poll(&mut context),
            Poll::Ready(ThemeChange::Settings)
        );
    }

    #[test]
    fn typed_overlays_are_reversible_and_component_roots_follow_material() {
        let theme = ThemeBundle::embedded().expect("embedded typed theme");
        let light = theme.tokens.palette(ThemeMode::Light);
        let accent = AccentColors {
            accent: light.status_neutral,
            light1: light.text_primary,
            light2: light.status_success,
            light3: light.status_caution,
            dark1: light.status_critical,
            dark2: light.stroke_surface,
            dark3: light.stroke_control,
        };
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for material in [Material::Mica, Material::Solid] {
                let resolved = theme.tokens.resolved(Some(&accent), material, None);
                let palette = resolved.palette(mode);
                let mut components = Theme::default();
                theme.mapping.apply(palette, &mut components);
                theme.mapping.assert_applied(palette, &components);
                assert_eq!(
                    components.tokens.background.color.a,
                    if material == Material::Mica { 0. } else { 1. }
                );
                assert_eq!(components.sidebar.a, components.background.a);
                assert_eq!(components.title_bar.a, components.background.a);
                assert_eq!(
                    palette.accent_fill,
                    if mode.is_dark() {
                        accent.light2
                    } else {
                        accent.dark1
                    }
                );
                assert_eq!(
                    palette.accent_text,
                    if mode.is_dark() {
                        accent.light3
                    } else {
                        accent.dark2
                    }
                );
                assert!((palette.accent_fill_hover.a - 0.9).abs() < f32::EPSILON);
            }
        }
        assert_eq!(theme.tokens.palette(ThemeMode::Light), light);
    }
    #[test]
    fn high_contrast_fields_are_opaque_and_ignore_previous_accent_and_material() {
        let colors = PreviewTheme::load_colors();
        let high = super::super::tokens::Palette::high_contrast(&colors);
        for token in super::super::tokens::PaletteToken::ALL {
            assert_eq!(token.color(&high).a, 1., "{token:?}");
        }
        for slot in crate::model::PaletteSlot::ALL {
            assert_eq!(high.categories.get(slot).a, 1.);
        }
        assert_eq!(high.window_background, colors.window);
        assert_eq!(high.stroke_card, colors.window_text);
        assert_eq!(high.accent_fill, colors.highlight);
    }
    #[test]
    fn production_theme_contains_no_preview_palette() {
        assert!(!include_str!("../../themes/fluent.json").contains("preview_high_contrast"));
    }
}
