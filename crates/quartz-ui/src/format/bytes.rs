//! Exact base-1024 byte formatting without lossy floating-point conversions.

use super::{
    Formatter,
    message::{Arg, Msg},
};

const BYTES_PER_KIB: u64 = 1024;
const BYTES_PER_MIB: u64 = BYTES_PER_KIB * 1024;
const BYTES_PER_GIB: u64 = BYTES_PER_MIB * 1024;
const BYTES_PER_TIB: u64 = BYTES_PER_GIB * 1024;
const TENTHS_PER_UNIT: u64 = 10;
const PROMOTION_TENTHS: u128 = 1024 * 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ByteUnit {
    Bytes,
    Kilobytes,
    Megabytes,
    Gigabytes,
    Terabytes,
}

impl ByteUnit {
    const fn msg(self) -> Msg {
        match self {
            Self::Bytes => Msg::BytesB,
            Self::Kilobytes => Msg::BytesKb,
            Self::Megabytes => Msg::BytesMb,
            Self::Gigabytes => Msg::BytesGb,
            Self::Terabytes => Msg::BytesTb,
        }
    }

    const fn next(self) -> Option<(Self, u64)> {
        match self {
            Self::Bytes => Some((Self::Kilobytes, BYTES_PER_KIB)),
            Self::Kilobytes => Some((Self::Megabytes, BYTES_PER_MIB)),
            Self::Megabytes => Some((Self::Gigabytes, BYTES_PER_GIB)),
            Self::Gigabytes => Some((Self::Terabytes, BYTES_PER_TIB)),
            Self::Terabytes => None,
        }
    }
}

fn round_tenths(bytes: u64, unit_bytes: u64) -> u128 {
    let unit = u128::from(unit_bytes);
    (u128::from(bytes) * u128::from(TENTHS_PER_UNIT) + unit / 2) / unit
}

fn format_tenths(tenths: u128) -> String {
    let scale = u128::from(TENTHS_PER_UNIT);
    let whole = tenths / scale;
    let frac = tenths % scale;
    if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

impl Formatter {
    /// Formats a byte count using base-1024 units (`B`, `KB`, `MB`, `GB`, `TB`).
    #[must_use]
    pub(crate) fn bytes(self, bytes: u64) -> String {
        if bytes < BYTES_PER_KIB {
            return self.text_with(
                ByteUnit::Bytes.msg(),
                &[Arg::Value.with(&bytes.to_string())],
            );
        }
        let (mut unit, mut divisor) = if bytes >= BYTES_PER_TIB {
            (ByteUnit::Terabytes, BYTES_PER_TIB)
        } else if bytes >= BYTES_PER_GIB {
            (ByteUnit::Gigabytes, BYTES_PER_GIB)
        } else if bytes >= BYTES_PER_MIB {
            (ByteUnit::Megabytes, BYTES_PER_MIB)
        } else {
            (ByteUnit::Kilobytes, BYTES_PER_KIB)
        };
        let mut tenths = round_tenths(bytes, divisor);
        if tenths >= PROMOTION_TENTHS
            && let Some((next_unit, next_divisor)) = unit.next()
        {
            unit = next_unit;
            divisor = next_divisor;
            tenths = round_tenths(bytes, divisor);
        }
        self.text_with(unit.msg(), &[Arg::Value.with(&format_tenths(tenths))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_i18n::Language;

    const ZH: Formatter = Formatter::new(Language::SimplifiedChinese);
    const EN: Formatter = Formatter::new(Language::English);

    #[test]
    fn byte_formatter_formats_exact_units_and_promotes_rounding_boundaries() {
        assert_eq!(EN.bytes(0), "0 B");
        assert_eq!(ZH.bytes(512), "512 B");
        assert_eq!(EN.bytes(1023), "1023 B");
        assert_eq!(EN.bytes(1024), "1 KB");
        assert_eq!(ZH.bytes(1536), "1.5 KB");
        assert_eq!(EN.bytes(BYTES_PER_MIB - 1), "1 MB");
        assert_eq!(EN.bytes(BYTES_PER_MIB), "1 MB");
        assert_eq!(EN.bytes(18_400_000), "17.5 MB");
        assert_eq!(EN.bytes(BYTES_PER_GIB + BYTES_PER_GIB / 4), "1.3 GB");
        assert_eq!(EN.bytes(2 * BYTES_PER_TIB), "2 TB");
        assert_eq!(EN.bytes(u64::MAX), "16777216 TB");
    }
}
