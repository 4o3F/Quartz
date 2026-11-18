//! Fact-range read timings for an independent simulation database.

use crate::{
    simulation::{SimulationPath, ValidateSimulationError},
    store::{
        self, ReadOnlyStore,
        facts::{RangeRowCounts, UTC_DAY_MS},
    },
};
use snafu::{ResultExt, Snafu};
use std::{
    hint,
    io::{self, Write},
    path::Path,
    time::Instant,
};

/// Resources 08 §2.1: day/week/year reads each execute 100 times.
pub(crate) const DEFAULT_RUNS: u32 = 100;
/// Resources 08 §2.1 compares day, seven-day week, and 365-day year reads.
const WEEK_DAYS: i64 = 7;
const YEAR_DAYS: i64 = 365;
/// Nearest-rank percentiles required by resources 08 §2.1.
const PERCENTILE_BASE: usize = 100;
const MEDIAN_PERCENT: usize = 50;
const TAIL_PERCENT: usize = 95;
/// Resource measurements 08 §2.1 express query durations in milliseconds.
const MILLISECONDS_PER_SECOND: f64 = 1_000.0;

#[derive(Debug, Snafu)]
pub(crate) enum BenchQueryError {
    #[snafu(display("{source}"))]
    Path { source: ValidateSimulationError },
    #[snafu(display("could not open simulation read-only: {source}"))]
    Open { source: store::OpenStoreError },
    #[snafu(display("{source}"))]
    Read {
        source: store::facts::ReadFactsError,
    },
    #[snafu(display("simulation database has no closed runs"))]
    Empty,
    #[snafu(display("runs must be positive"))]
    Runs,
    #[snafu(display("could not write benchmark output: {source}"))]
    Output { source: io::Error },
}
#[derive(Debug)]
pub(crate) struct BenchQuery<'path> {
    path: &'path Path,
    runs: u32,
}
impl<'path> BenchQuery<'path> {
    pub(crate) fn new(path: &'path Path, runs: u32) -> Self {
        Self { path, runs }
    }
    pub(crate) fn run(&self) -> Result<(), BenchQueryError> {
        let path = SimulationPath::offline(self.path).context(PathSnafu)?;
        let mut reader = ReadOnlyStore::open_facts(path.as_path()).context(OpenSnafu)?;
        self.measure(&mut reader, &mut io::stdout().lock())
    }
    pub(crate) fn measure(
        &self,
        reader: &mut ReadOnlyStore,
        output: &mut impl Write,
    ) -> Result<(), BenchQueryError> {
        use snafu::OptionExt;
        snafu::ensure!(self.runs > 0, RunsSnafu);
        let extent = reader.extent().context(ReadSnafu)?.context(EmptySnafu)?;
        let from = extent.from_utc_ms;
        writeln!(
            output,
            concat!(
                "fact range reads; foreground + availability + gaps; runs={}; ",
                "nearest-rank percentiles; includes first read"
            ),
            self.runs,
        )
        .context(OutputSnafu)?;
        for (name, days) in [("day", 1), ("week", WEEK_DAYS), ("year", YEAR_DAYS)] {
            let to = from.saturating_add(days * UTC_DAY_MS);
            let mut samples = Vec::with_capacity(
                usize::try_from(self.runs).expect("run count fits usize on supported targets"),
            );
            let mut counts = RangeRowCounts::default();
            for _ in 0..self.runs {
                let start = Instant::now();
                let rows = reader.fact_range(from, to).context(ReadSnafu)?;
                counts = rows.counts();
                hint::black_box(&rows);
                samples.push(start.elapsed());
            }
            samples.sort_unstable();
            let percentile = |percent: usize| {
                samples[(samples.len() * percent).div_ceil(PERCENTILE_BASE) - 1].as_secs_f64()
                    * MILLISECONDS_PER_SECOND
            };
            let maximum = samples[samples.len() - 1].as_secs_f64() * MILLISECONDS_PER_SECOND;
            writeln!(
                output,
                concat!(
                    "{} from_utc_ms={} to_utc_ms={} rows={} foreground={} availability={} gaps={} ",
                    "p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
                ),
                name,
                from,
                to,
                counts.total(),
                counts.foreground,
                counts.availability,
                counts.gaps,
                percentile(MEDIAN_PERCENT),
                percentile(TAIL_PERCENT),
                maximum,
            )
            .context(OutputSnafu)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        data_directory::DataDirectory,
        generate_data::{GenerateData, Profile},
    };
    #[test]
    fn benchmarks_tiny_capture_database() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("tiny.db");
        GenerateData::new(&path, Profile::Light, 1, 1)
            .generate(
                &DataDirectory::new(temporary.path().join("production")),
                &mut Vec::new(),
            )
            .expect("generate");
        let mut reader = ReadOnlyStore::open(&path).expect("read-only database");
        let mut output = Vec::new();
        BenchQuery::new(&path, 2)
            .measure(&mut reader, &mut output)
            .expect("benchmark");
        let output = String::from_utf8(output).expect("UTF-8 output");
        for name in [
            "day from_utc_ms=",
            "week from_utc_ms=",
            "year from_utc_ms=",
            "p50_ms=",
            "p95_ms=",
            "max_ms=",
        ] {
            assert!(output.contains(name), "missing {name}: {output}");
        }
    }
}
