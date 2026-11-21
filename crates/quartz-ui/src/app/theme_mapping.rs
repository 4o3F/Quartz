//! gpui-component field bindings decoded once from the embedded mapping (11 section 9.2).
use super::tokens::{Palette, PaletteToken};
use gpui_kit::{Hsla, component::Theme};
use serde::Deserialize;
use std::collections::BTreeMap;

macro_rules! component_fields {
    ($($field:ident => $json:literal => $variant:ident),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
        enum ComponentField { $(#[serde(rename=$json)] $variant,)+ }
        impl ComponentField {
            fn write(self, theme: &mut Theme, color: Hsla) { match self { $(Self::$variant => { theme.colors.$field = color; theme.tokens.$field = color.into(); },)+ } }
            #[cfg(test)]
            fn color(self, theme: &Theme) -> Hsla { match self { $(Self::$variant => theme.colors.$field,)+ } }
        }
    };
}
component_fields! {
    background => "background" => Background,
    sidebar => "sidebar.background" => Sidebar,
    foreground => "foreground" => Foreground,
    muted_foreground => "muted.foreground" => MutedForeground,
    border => "border" => Border,
    input => "input.border" => Input,
    primary => "primary.background" => Primary,
    primary_hover => "primary.hover.background" => PrimaryHover,
    primary_active => "primary.active.background" => PrimaryActive,
    primary_foreground => "primary.foreground" => PrimaryForeground,
    secondary => "secondary.background" => Secondary,
    secondary_hover => "secondary.hover.background" => SecondaryHover,
    secondary_active => "secondary.active.background" => SecondaryActive,
    secondary_foreground => "secondary.foreground" => SecondaryForeground,
    list_hover => "list.hover.background" => ListHover,
    list_active => "list.active.background" => ListActive,
    popover => "popover.background" => Popover,
    popover_foreground => "popover.foreground" => PopoverForeground,
    ring => "ring" => Ring,
    danger => "danger.background" => Danger,
    danger_active => "danger.active.background" => DangerActive,
    danger_hover => "danger.hover.background" => DangerHover,
    danger_foreground => "danger.foreground" => DangerForeground,
    success => "success.background" => Success,
    success_foreground => "success.foreground" => SuccessForeground,
    warning => "warning.background" => Warning,
    warning_foreground => "warning.foreground" => WarningForeground,
    title_bar => "title_bar.background" => TitleBar,
    title_bar_border => "title_bar.border" => TitleBarBorder,
    scrollbar => "scrollbar.background" => Scrollbar,
    scrollbar_thumb => "scrollbar.thumb.background" => ScrollbarThumb,
    scrollbar_thumb_hover => "scrollbar.thumb.hover.background" => ScrollbarThumbHover,
    accent => "accent.background" => Accent,
    accent_foreground => "accent.foreground" => AccentForeground,
    muted => "muted.background" => Muted,
    selection => "selection.background" => Selection,
    list => "list.background" => List,
    list_active_border => "list.active.border" => ListActiveBorder,
    list_highlight_foreground => "list.highlight.foreground" => ListHighlightForeground,
    sidebar_foreground => "sidebar.foreground" => SidebarForeground,
    sidebar_accent => "sidebar.accent.background" => SidebarAccent,
    sidebar_accent_foreground => "sidebar.accent.foreground" => SidebarAccentForeground,
    sidebar_primary => "sidebar.primary.background" => SidebarPrimary,
    sidebar_primary_foreground => "sidebar.primary.foreground" => SidebarPrimaryForeground,
    sidebar_border => "sidebar.border" => SidebarBorder,
    link => "link" => Link,
    link_hover => "link.hover" => LinkHover,
    link_active => "link.active" => LinkActive,
    progress_bar => "progress.bar.background" => ProgressBar,
    overlay => "overlay" => Overlay,
    window_border => "window.border" => WindowBorder,
    chart_1 => "chart.1" => Chart1,
    chart_2 => "chart.2" => Chart2,
    chart_3 => "chart.3" => Chart3,
    chart_4 => "chart.4" => Chart4,
    chart_5 => "chart.5" => Chart5,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub(super) struct ComponentMapping(BTreeMap<ComponentField, PaletteToken>);
impl ComponentMapping {
    pub(super) fn apply(&self, palette: &Palette, theme: &mut Theme) {
        for (field, token) in &self.0 {
            field.write(theme, token.color(palette));
        }
    }
    #[cfg(test)]
    pub(super) fn assert_applied(&self, palette: &Palette, theme: &Theme) {
        for (field, token) in &self.0 {
            assert_eq!(field.color(theme), token.color(palette), "{field:?}");
        }
    }
}
