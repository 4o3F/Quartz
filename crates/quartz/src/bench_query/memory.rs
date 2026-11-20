//! Endpoint samples of process-private committed bytes, as requested by resources 08 §1.4.

use super::BenchQueryError;

#[derive(Debug, Clone, Copy)]
pub(super) struct PrivateBytes(Option<usize>);
impl PrivateBytes {
    pub(super) fn sample() -> Result<Self, BenchQueryError> {
        #[cfg(windows)]
        {
            use windows::Win32::System::{
                ProcessStatus::{
                    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
                },
                Threading::GetCurrentProcess,
            };
            let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>())
                .expect("process counters size fits DWORD");
            let mut counters = PROCESS_MEMORY_COUNTERS_EX {
                cb: size,
                ..Default::default()
            };
            // SAFETY: The current process handle is live. The output pointer addresses an EX
            // structure whose cb and buffer length include PrivateUsage.
            unsafe {
                GetProcessMemoryInfo(
                    GetCurrentProcess(),
                    (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                    size,
                )
            }
            .map_err(|source| BenchQueryError::Memory { source })?;
            Ok(Self(Some(counters.PrivateUsage)))
        }
        #[cfg(not(windows))]
        {
            Ok(Self(None))
        }
    }
}
#[derive(Debug)]
pub(super) struct MemoryPeaks {
    idle: PrivateBytes,
    peak: PrivateBytes,
}
impl MemoryPeaks {
    pub(super) fn new(idle: PrivateBytes) -> Self {
        Self { idle, peak: idle }
    }
    pub(super) fn observe(&mut self, sample: PrivateBytes) {
        if let Some(sample) = sample.0
            && let Some(peak) = &mut self.peak.0
        {
            *peak = (*peak).max(sample);
        }
    }
    pub(super) fn write(
        &self,
        name: &str,
        output: &mut impl std::io::Write,
    ) -> std::io::Result<()> {
        if let Some(idle) = self.idle.0
            && let Some(peak) = self.peak.0
        {
            writeln!(
                output,
                "{name}.memory idle_private_bytes={idle} peak_private_bytes={peak} peak_private_bytes_delta={}",
                peak - idle
            )
        } else {
            writeln!(output, "{name}.memory peak_private_bytes=not_measured")
        }
    }
}
