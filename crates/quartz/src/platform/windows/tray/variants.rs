//! In-memory grey and private variants derived from the current application icon.

use windows::Win32::{
    Graphics::Gdi::{
        self, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    },
    UI::WindowsAndMessaging::{self, DI_NORMAL, HICON, ICONINFO},
};

/// Standard application icon canvas; Shell scales it for the notification area.
const ICON_SIDE: i32 = 32;
const PIXEL_SIDE: usize = 32;
const PIXEL_COUNT: usize = PIXEL_SIDE * PIXEL_SIDE;
/// A monochrome row is aligned to a 16-bit boundary by CreateBitmap.
const MASK_BYTES: usize = PIXEL_SIDE * (PIXEL_SIDE / 8);
const PIXEL_BITS: u16 = 32;
const COLOR_CHANNELS: u32 = 3;
const OPAQUE_ALPHA: u32 = 0xff00_0000;
const CHANNEL_MASK: u32 = 0xff;
/// Lower-right privacy dot with a white rim, in the 32-pixel canvas.
const DOT_CENTER: i32 = 24;
const DOT_RADIUS: i32 = 6;
const DOT_INNER_RADIUS: i32 = 4;
const DOT_COLOR: u32 = 0xff69_45b8;
const DOT_RIM: u32 = 0xffff_ffff;

#[derive(Debug, snafu::Snafu)]
pub(in crate::platform::windows) enum GenerateTrayIconsError {
    #[snafu(display("could not generate tray icon pixels or handles: {source}"))]
    Windows { source: windows::core::Error },
    #[snafu(display("tray icon GDI operation failed: {operation}"))]
    Gdi { operation: GdiCall },
}
impl From<windows::core::Error> for GenerateTrayIconsError {
    fn from(source: windows::core::Error) -> Self {
        Self::Windows { source }
    }
}
/// These GDI calls report failure without a documented extended last-error value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub(in crate::platform::windows) enum GdiCall {
    CreateCompatibleDC,
    GdiFlush,
    SelectObject,
    CreateBitmap,
}
impl GdiCall {
    fn check(self, outcome: GdiOutcome) -> Result<(), GenerateTrayIconsError> {
        match outcome {
            GdiOutcome::Succeeded => Ok(()),
            GdiOutcome::Failed => Err(GenerateTrayIconsError::Gdi { operation: self }),
        }
    }
}
enum GdiOutcome {
    Succeeded,
    Failed,
}
impl GdiOutcome {
    fn from_success(succeeded: bool) -> Self {
        if succeeded {
            Self::Succeeded
        } else {
            Self::Failed
        }
    }
}
struct OwnedIcon(HICON);
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        // SAFETY: Only icons created by CreateIconIndirect are owned here; Shell has copied them.
        let _ = unsafe { WindowsAndMessaging::DestroyIcon(self.0) };
    }
}
pub(super) struct IconVariants {
    grey: OwnedIcon,
    private: OwnedIcon,
}
impl IconVariants {
    pub(super) fn new(source: HICON) -> Result<Self, GenerateTrayIconsError> {
        let mut canvas = Canvas::new()?;
        // SAFETY: The memory DC contains a writable 32-bit DIB; source remains shared and live.
        unsafe {
            WindowsAndMessaging::DrawIconEx(
                canvas.dc, 0, 0, source, ICON_SIDE, ICON_SIDE, 0, None, DI_NORMAL,
            )
        }?;
        GdiCall::GdiFlush.check(GdiOutcome::from_success(
            // SAFETY: Complete DrawIconEx's GDI writes before reading the DIB memory.
            unsafe { Gdi::GdiFlush() }.as_bool(),
        ))?;
        // SAFETY: CreateDIBSection allocated PIXEL_COUNT aligned u32 pixels for this canvas.
        let original = unsafe { std::slice::from_raw_parts(canvas.pixels, PIXEL_COUNT) }.to_vec();
        let has_alpha = original.iter().any(|pixel| pixel & OPAQUE_ALPHA != 0);
        let mut pixels = original;
        if !has_alpha {
            for pixel in &mut pixels {
                if *pixel != 0 {
                    *pixel |= OPAQUE_ALPHA;
                }
            }
        }
        let grey = canvas.icon(&Self::grey_pixels(&pixels))?;
        let private = canvas.icon(&Self::private_pixels(&pixels))?;
        Ok(Self { grey, private })
    }
    pub(super) fn grey(&self) -> HICON {
        self.grey.0
    }
    pub(super) fn private(&self) -> HICON {
        self.private.0
    }
    fn grey_pixels(pixels: &[u32]) -> Vec<u32> {
        pixels
            .iter()
            .map(|pixel| {
                let value = ((pixel & CHANNEL_MASK)
                    + ((pixel >> 8) & CHANNEL_MASK)
                    + ((pixel >> 16) & CHANNEL_MASK))
                    / COLOR_CHANNELS;
                (pixel & OPAQUE_ALPHA) | value | (value << 8) | (value << 16)
            })
            .collect()
    }
    fn private_pixels(pixels: &[u32]) -> Vec<u32> {
        let mut marked = pixels.to_vec();
        for y in 0..ICON_SIDE {
            for x in 0..ICON_SIDE {
                let distance = (x - DOT_CENTER).pow(2) + (y - DOT_CENTER).pow(2);
                if distance <= DOT_RADIUS.pow(2) {
                    let index =
                        usize::try_from(y * ICON_SIDE + x).expect("positive canvas coordinate");
                    marked[index] = if distance <= DOT_INNER_RADIUS.pow(2) {
                        DOT_COLOR
                    } else {
                        DOT_RIM
                    };
                }
            }
        }
        marked
    }
}
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    mask: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut u32,
}
impl Canvas {
    fn new() -> Result<Self, GenerateTrayIconsError> {
        // SAFETY: A compatible memory DC requires no borrowed window resources.
        let dc = unsafe { Gdi::CreateCompatibleDC(None) };
        GdiCall::CreateCompatibleDC.check(GdiOutcome::from_success(!dc.is_invalid()))?;
        let mut canvas = Self {
            dc,
            bitmap: HBITMAP::default(),
            mask: HBITMAP::default(),
            previous: HGDIOBJ::default(),
            pixels: std::ptr::null_mut(),
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: super::super::structure_size::<BITMAPINFOHEADER>(),
                biWidth: ICON_SIDE,
                biHeight: -ICON_SIDE,
                biPlanes: 1,
                biBitCount: PIXEL_BITS,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = std::ptr::null_mut();
        // SAFETY: info defines a top-down 32-bit canvas; pixels receives its allocation.
        canvas.bitmap = unsafe {
            Gdi::CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0)
        }?;
        canvas.pixels = pixels.cast();
        // SAFETY: Both DC and bitmap are uniquely owned and remain live until restored in Drop.
        canvas.previous = unsafe { Gdi::SelectObject(dc, canvas.bitmap.into()) };
        GdiCall::SelectObject.check(GdiOutcome::from_success(!canvas.previous.is_invalid()))?;
        // SAFETY: Zeroed monochrome mask is opaque; per-pixel alpha controls the icon transparency.
        let mask = [0_u8; MASK_BYTES];
        // SAFETY: mask supplies the complete monochrome bitmap and is copied synchronously.
        canvas.mask =
            unsafe { Gdi::CreateBitmap(ICON_SIDE, ICON_SIDE, 1, 1, Some(mask.as_ptr().cast())) };
        GdiCall::CreateBitmap.check(GdiOutcome::from_success(!canvas.mask.is_invalid()))?;
        // SAFETY: The allocated canvas contains exactly PIXEL_COUNT writable pixels.
        unsafe { std::ptr::write_bytes(canvas.pixels, 0, PIXEL_COUNT) };
        Ok(canvas)
    }
    fn icon(&mut self, pixels: &[u32]) -> Result<OwnedIcon, GenerateTrayIconsError> {
        // SAFETY: Both slices have PIXEL_COUNT pixels and do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.pixels, PIXEL_COUNT) };
        // SAFETY: GDI requires the source bitmaps to be unselected while copying the icon.
        unsafe { Gdi::SelectObject(self.dc, self.previous) };
        let info = ICONINFO {
            fIcon: windows::core::BOOL::from(true),
            hbmMask: self.mask,
            hbmColor: self.bitmap,
            ..Default::default()
        };
        // SAFETY: CreateIconIndirect copies the live color and mask bitmaps into a new owned icon.
        let created = unsafe { WindowsAndMessaging::CreateIconIndirect(&info) };
        // SAFETY: Restore our bitmap for the next variant; both handles remain live.
        unsafe { Gdi::SelectObject(self.dc, self.bitmap.into()) };
        Ok(OwnedIcon(created?))
    }
}
impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: Restore the DC's original object before deleting our uniquely owned bitmaps and DC.
        unsafe {
            if !self.previous.is_invalid() {
                let _ = Gdi::SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = Gdi::DeleteObject(self.bitmap.into());
            }
            if !self.mask.is_invalid() {
                let _ = Gdi::DeleteObject(self.mask.into());
            }
            let _ = Gdi::DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_gdi_calls_identify_operation_without_an_extended_error_source() {
        use std::error::Error as _;
        for operation in [
            GdiCall::CreateCompatibleDC,
            GdiCall::GdiFlush,
            GdiCall::SelectObject,
            GdiCall::CreateBitmap,
        ] {
            let error = operation
                .check(GdiOutcome::Failed)
                .expect_err("injected GDI failure");
            assert!(error.source().is_none());
            assert!(
                matches!(error, GenerateTrayIconsError::Gdi { operation: reported } if reported == operation)
            );
            operation
                .check(GdiOutcome::Succeeded)
                .expect("injected success");
        }
    }
    #[test]
    fn generated_variants_preserve_alpha_and_add_a_visible_privacy_marker() {
        let original = vec![0xff12_3456; PIXEL_COUNT];
        let grey = IconVariants::grey_pixels(&original);
        assert_eq!(grey[0], 0xff34_3434);
        let private = IconVariants::private_pixels(&original);
        assert_eq!(private[0], original[0]);
        let center = usize::try_from(DOT_CENTER).expect("positive coordinate");
        assert_eq!(private[center * PIXEL_SIDE + center], DOT_COLOR);
        assert_eq!(IconVariants::grey_pixels(&[0]), [0]);
    }
    #[test]
    fn shared_application_icon_produces_two_owned_native_icons() {
        // SAFETY: IDI_APPLICATION is a shared system resource, retained by Windows.
        let source =
            unsafe { WindowsAndMessaging::LoadIconW(None, WindowsAndMessaging::IDI_APPLICATION) }
                .expect("system application icon");
        let icons = IconVariants::new(source).expect("generated variants");
        assert!(!icons.grey().is_invalid());
        assert!(!icons.private().is_invalid());
        assert_ne!(icons.grey(), icons.private());
        assert_ne!(icons.grey(), source);
    }
}
