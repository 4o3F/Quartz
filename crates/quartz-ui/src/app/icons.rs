//! Fluent UI System Icons, embedded without the bundled Lucide asset source (11 section 7).

use super::metrics::Density;
use gpui_kit::{AssetSource, SharedString, assets::IconName};
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IconVariant {
    Regular,
    Selected,
}

#[derive(Debug)]
struct IconAsset {
    name: IconName,
    compact: &'static [u8],
    standard: &'static [u8],
    filled_compact: Option<&'static [u8]>,
    filled_standard: Option<&'static [u8]>,
}

macro_rules! filled {
    ($name:ident, $size:literal) => {
        None
    };
    ($name:ident, $size:literal, filled) => {
        Some(include_bytes!(concat!(
            "../../assets/icons/",
            stringify!($name),
            "-",
            $size,
            "-filled.svg"
        )))
    };
}
macro_rules! fluent_icons {
    ($($name:ident $(=> $filled:ident)?),* $(,)?) => {
        const ICONS: &[IconAsset] = &[$(IconAsset {
            name: IconName::$name,
            compact: include_bytes!(concat!("../../assets/icons/", stringify!($name), "-16-regular.svg")),
            standard: include_bytes!(concat!("../../assets/icons/", stringify!($name), "-20-regular.svg")),
            filled_compact: filled!($name, "16" $(, $filled)?),
            filled_standard: filled!($name, "20" $(, $filled)?),
        }),*];
    };
}
fluent_icons! {
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
    Menu,
    Calendar => filled,
    CalendarRange => filled,
    Tag => filled,
    Settings => filled,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Minus,
    Plus,
    Close,
    Clock,
    Check,
    Shield,
    Trash,
    Code,
    Globe,
    MessageCircle,
    Send,
    SquareTerminal,
    FileText,
    Copy,
    TriangleAlert,
    CircleX,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AppAssets;

impl IconAsset {
    fn path(&self, density: Density, variant: IconVariant) -> SharedString {
        let size = match density {
            Density::Compact => "16",
            Density::Standard => "20",
        };
        let style = match variant {
            IconVariant::Regular => "regular",
            IconVariant::Selected => "filled",
        };
        format!("fluent/{:?}-{size}-{style}.svg", self.name).into()
    }

    fn bytes(&self, density: Density, variant: IconVariant) -> Option<&'static [u8]> {
        match variant {
            IconVariant::Regular => Some(match density {
                Density::Compact => self.compact,
                Density::Standard => self.standard,
            }),
            IconVariant::Selected => match density {
                Density::Compact => self.filled_compact,
                Density::Standard => self.filled_standard,
            },
        }
    }
}

impl AppAssets {
    pub(crate) fn progress_track_path() -> SharedString {
        "quartz/ProgressTrack.svg".into()
    }
    pub(crate) fn progress_path() -> SharedString {
        "quartz/ProgressRing.svg".into()
    }
    pub(crate) fn status_path(severity: crate::failure::Severity) -> SharedString {
        match severity {
            crate::failure::Severity::Recoverable => "fluent/TriangleAlert-28-regular.svg",
            crate::failure::Severity::Unrecoverable => "fluent/CircleX-28-regular.svg",
        }
        .into()
    }

    pub(crate) fn icon_path(
        name: IconName,
        density: Density,
        variant: IconVariant,
    ) -> SharedString {
        ICONS
            .iter()
            .find(|asset| asset.name == name)
            .expect("used IconName has a Fluent asset")
            .path(density, variant)
    }
}

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        match path {
            "quartz/ProgressTrack.svg" => {
                return Ok(Some(Cow::Borrowed(include_bytes!(
                    "../../assets/icons/ProgressTrack.svg"
                ))));
            }
            "quartz/ProgressRing.svg" => {
                return Ok(Some(Cow::Borrowed(include_bytes!(
                    "../../assets/icons/ProgressRing.svg"
                ))));
            }
            "fluent/TriangleAlert-28-regular.svg" => {
                return Ok(Some(Cow::Borrowed(include_bytes!(
                    "../../assets/icons/TriangleAlert-28-regular.svg"
                ))));
            }
            "fluent/CircleX-28-regular.svg" => {
                return Ok(Some(Cow::Borrowed(include_bytes!(
                    "../../assets/icons/CircleX-28-regular.svg"
                ))));
            }
            _ => {}
        }
        for asset in ICONS {
            // gpui-component's title bar resolves the same IconName paths at compact density.
            if asset.name.path().as_ref() == path {
                return Ok(Some(Cow::Borrowed(asset.compact)));
            }
            for density in [Density::Compact, Density::Standard] {
                for variant in [IconVariant::Regular, IconVariant::Selected] {
                    if asset.path(density, variant).as_ref() == path {
                        return asset
                            .bytes(density, variant)
                            .map(|bytes| Some(Cow::Borrowed(bytes)))
                            .ok_or_else(|| anyhow::anyhow!("missing Fluent icon variant {path}"));
                    }
                }
            }
        }
        Err(anyhow::anyhow!("missing Fluent icon asset {path}"))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = vec![
            Self::progress_path(),
            Self::progress_track_path(),
            Self::status_path(crate::failure::Severity::Recoverable),
            Self::status_path(crate::failure::Severity::Unrecoverable),
        ];
        for asset in ICONS {
            paths.push(asset.name.path());
            for density in [Density::Compact, Density::Standard] {
                for variant in [IconVariant::Regular, IconVariant::Selected] {
                    if asset.bytes(density, variant).is_some() {
                        paths.push(asset.path(density, variant));
                    }
                }
            }
        }
        paths.retain(|entry| entry.starts_with(path));
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn check_source(path: &Path) {
        for entry in fs::read_dir(path).expect("source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                check_source(&path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let source = fs::read_to_string(&path).expect("Rust source");
                for tail in source.split("IconName::").skip(1) {
                    let name: String = tail
                        .chars()
                        .take_while(|character| character.is_ascii_alphanumeric())
                        .collect();
                    if name
                        .chars()
                        .next()
                        .is_some_and(|character| character.is_ascii_uppercase())
                    {
                        assert!(
                            ICONS
                                .iter()
                                .any(|asset| format!("{:?}", asset.name) == name),
                            "{} uses missing Fluent icon {name}",
                            path.display()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn manifest_covers_every_embedded_svg_including_error_glyphs() {
        #[derive(serde::Deserialize)]
        struct Manifest {
            icons: Vec<Entry>,
            custom_icons: Vec<CustomEntry>,
        }
        #[derive(serde::Deserialize)]
        struct Entry {
            name: String,
            size: u16,
            style: String,
            file: String,
        }
        #[derive(serde::Deserialize)]
        struct CustomEntry {
            file: String,
            path: String,
        }
        let manifest: Manifest =
            serde_json::from_str(include_str!("../../assets/icons/manifest.json"))
                .expect("manifest");
        let mut declared = std::collections::BTreeSet::new();
        for entry in manifest.icons {
            let path = format!("fluent/{}-{}-{}.svg", entry.name, entry.size, entry.style);
            let embedded = AppAssets
                .load(&path)
                .expect("registered path")
                .expect("SVG");
            assert!(
                declared.insert(entry.file.clone()),
                "duplicate manifest entry"
            );
            assert_eq!(
                embedded.as_ref(),
                fs::read(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("assets/icons")
                        .join(entry.file)
                )
                .expect("SVG file")
            );
        }
        for entry in manifest.custom_icons {
            let embedded = AppAssets
                .load(&entry.path)
                .expect("custom path")
                .expect("SVG");
            assert!(declared.insert(entry.file.clone()));
            assert_eq!(
                embedded.as_ref(),
                fs::read(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("assets/icons")
                        .join(entry.file)
                )
                .expect("custom SVG")
            );
        }
        let files = fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons"))
            .expect("icons");
        for entry in files {
            let path = entry.expect("entry").path();
            if path.extension().is_some_and(|extension| extension == "svg") {
                assert!(
                    declared.contains(path.file_name().expect("filename").to_str().expect("UTF-8")),
                    "undeclared {path:?}"
                );
            }
        }
    }

    #[test]
    fn every_used_icon_and_registered_variant_has_an_asset() {
        check_source(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        for path in AppAssets.list("").expect("icon paths") {
            assert!(AppAssets.load(&path).expect("embedded SVG").is_some());
        }
        assert!(AppAssets.load("icons/missing.svg").is_err());
        for name in [
            IconName::Calendar,
            IconName::CalendarRange,
            IconName::Tag,
            IconName::Settings,
        ] {
            for density in [Density::Compact, Density::Standard] {
                let path = AppAssets::icon_path(name, density, IconVariant::Selected);
                assert!(
                    AppAssets
                        .load(&path)
                        .expect("selected sidebar icon")
                        .is_some()
                );
            }
        }
    }
}
