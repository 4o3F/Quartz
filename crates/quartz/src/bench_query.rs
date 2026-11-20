//! Fact-range and staged pipeline timings for an independent simulation database.

mod memory;

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
    #[snafu(display("could not run query pipeline: {source}"))]
    Query { source: crate::query::RunQueryError },
    #[snafu(display("could not load simulation rule snapshot: {source}"))]
    Rules {
        source: crate::store::rules::LoadRulesError,
    },
    #[snafu(display("could not build simulation rule snapshot: {source}"))]
    Registry {
        source: quartz_domain::rules::registry::CreateRuleRegistryError,
    },
    #[snafu(display("runs must be positive"))]
    Runs,
    #[cfg(windows)]
    #[snafu(display("could not sample private bytes: {source}"))]
    Memory { source: windows::core::Error },
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
        let reader = ReadOnlyStore::open_facts(path.as_path()).context(OpenSnafu)?;
        self.measure(reader, &mut io::stdout().lock())
    }
    pub(crate) fn measure(
        &self,
        mut reader: ReadOnlyStore,
        output: &mut impl Write,
    ) -> Result<(), BenchQueryError> {
        use snafu::OptionExt;
        snafu::ensure!(self.runs > 0, RunsSnafu);
        let extent = reader.extent().context(ReadSnafu)?.context(EmptySnafu)?;
        let from = extent.from_utc_ms;
        reader = self.measure_pipeline(reader, from, output)?;
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
    fn measure_pipeline(
        &self,
        mut reader: ReadOnlyStore,
        from: i64,
        output: &mut impl Write,
    ) -> Result<ReadOnlyStore, BenchQueryError> {
        use crate::query::{QueryJob, QueryRequest};
        use quartz_protocol::{
            MillisecondsPerPixel, QueryDayRequest, QueryTimelineRequest, TimelineDetail,
        };
        let registry = quartz_domain::rules::registry::RuleRegistry::from_state(
            reader.rule_sets().load().context(RulesSnafu)?,
        )
        .context(RegistrySnafu)?;
        let hash = registry
            .snapshots()
            .load()
            .hashes()
            .classification
            .to_string();
        let local_date = jiff::Timestamp::from_millisecond(from)
            .map_err(crate::query::RunQueryError::from)
            .context(QuerySnafu)?
            .to_zoned_datetime(jiff::tz::TimeZone::system())
            .date();
        let offset = reader
            .query_settings()
            .context(QuerySnafu)?
            .day_start_offset_min;
        let calendar =
            crate::query::calendar::DayCalendar::new(local_date, offset).context(QuerySnafu)?;
        // 60 seconds per pixel gives a 1,440-pixel daily timeline.
        const BENCH_PIXEL_MS: u64 = 60_000;
        writeln!(output, "pipeline local_date={local_date} system_zone={:?} day_start_offset_min={offset} from_utc_ms={} day_to_utc_ms={} ms_per_pixel={BENCH_PIXEL_MS}", jiff::tz::TimeZone::system(), calendar.range.start, calendar.range.end).context(OutputSnafu)?;
        let timeline = |days| {
            QueryRequest::Timeline(QueryTimelineRequest {
                start_utc_ms: calendar.range.start,
                end_utc_ms: calendar.range.start + days * UTC_DAY_MS,
                detail: TimelineDetail::App,
                ms_per_pixel: MillisecondsPerPixel(
                    std::num::NonZeroU64::new(BENCH_PIXEL_MS).expect("positive pixel duration"),
                ),
                cursor: None,
            })
        };
        let queries = [
            QueryRequest::Day(QueryDayRequest {
                local_date,
                cursor: None,
            }),
            timeline(1),
            timeline(WEEK_DAYS),
        ];
        let idle = memory::PrivateBytes::sample()?;
        for query in queries {
            let name = match &query {
                QueryRequest::Day(_) => "pipeline.day",
                QueryRequest::Timeline(query)
                    if query.end_utc_ms - query.start_utc_ms == UTC_DAY_MS =>
                {
                    "pipeline.timeline.day"
                }
                QueryRequest::Timeline(_) => "pipeline.timeline.week",
            };
            let mut samples = Vec::new();
            let mut encoded_bytes = 0;
            let mut stages = StageSamples::default();
            let mut memory = memory::MemoryPeaks::new(idle);
            for _ in 0..self.runs {
                let mut snapshot_reader = Some(reader);
                memory.observe(memory::PrivateBytes::sample()?);
                let started = Instant::now();
                let job = QueryJob::start(&mut snapshot_reader, query.clone(), &hash)
                    .context(QuerySnafu)?;
                let prepare = started.elapsed();
                let mut completed = job.run_measured().context(QuerySnafu)?;
                completed.timings.read += prepare;
                let encode = Instant::now();
                encoded_bytes = serde_json::to_vec(&completed.response)
                    .map_err(crate::query::RunQueryError::from)
                    .context(QuerySnafu)?
                    .len();
                completed.timings.page_encode += encode.elapsed();
                hint::black_box(&completed.response);
                samples.push(started.elapsed());
                stages.push(completed.timings);
                memory.observe(memory::PrivateBytes::sample()?);
                reader = completed.reader;
            }
            samples.sort_unstable();
            let percentile = |percent: usize| {
                samples[(samples.len() * percent).div_ceil(PERCENTILE_BASE) - 1].as_secs_f64()
                    * MILLISECONDS_PER_SECOND
            };
            writeln!(
                output,
                "{name} first_page_bytes={encoded_bytes} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
                percentile(MEDIAN_PERCENT),
                percentile(TAIL_PERCENT),
                samples[samples.len() - 1].as_secs_f64() * MILLISECONDS_PER_SECOND
            )
            .context(OutputSnafu)?;
            stages.write(name, output).context(OutputSnafu)?;
            memory.write(name, output).context(OutputSnafu)?;
        }
        Ok(reader)
    }
}
#[derive(Debug, Default)]
struct StageSamples {
    read: Vec<std::time::Duration>,
    slice: Vec<std::time::Duration>,
    aggregate: Vec<std::time::Duration>,
    page_encode: Vec<std::time::Duration>,
}
impl StageSamples {
    fn push(&mut self, timings: crate::query::timing::QueryTimings) {
        self.read.push(timings.read);
        self.slice.push(timings.slice);
        self.aggregate.push(timings.aggregate);
        self.page_encode.push(timings.page_encode);
    }
    fn write(&self, name: &str, output: &mut impl Write) -> io::Result<()> {
        self.write_stage(name, "read", &self.read, output)?;
        self.write_stage(name, "slice", &self.slice, output)?;
        self.write_stage(name, "aggregate_merge", &self.aggregate, output)?;
        self.write_stage(name, "paging_encode", &self.page_encode, output)
    }
    fn write_stage(
        &self,
        name: &str,
        stage: &str,
        samples: &[std::time::Duration],
        output: &mut impl Write,
    ) -> io::Result<()> {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let percentile = |percent: usize| {
            sorted[(sorted.len() * percent).div_ceil(PERCENTILE_BASE) - 1].as_secs_f64()
                * MILLISECONDS_PER_SECOND
        };
        writeln!(
            output,
            "{name}.{stage} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
            percentile(MEDIAN_PERCENT),
            percentile(TAIL_PERCENT),
            sorted[sorted.len() - 1].as_secs_f64() * MILLISECONDS_PER_SECOND
        )
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
        let reader = ReadOnlyStore::open(&path).expect("read-only database");
        let mut output = Vec::new();
        BenchQuery::new(&path, 2)
            .measure(reader, &mut output)
            .expect("benchmark");
        let output = String::from_utf8(output).expect("UTF-8 output");
        for name in [
            "day from_utc_ms=",
            "week from_utc_ms=",
            "year from_utc_ms=",
            "p50_ms=",
            "p95_ms=",
            "max_ms=",
            ".read p50_ms=",
            ".slice p50_ms=",
            ".aggregate_merge p50_ms=",
            ".paging_encode p50_ms=",
            ".memory ",
        ] {
            assert!(output.contains(name), "missing {name}: {output}");
        }
    }
}
