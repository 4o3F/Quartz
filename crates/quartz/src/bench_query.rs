//! Fact-range and staged pipeline timings for an independent simulation database.

use quartz_domain::capture::limits::UTC_DAY_MS;
mod memory;

use crate::{
    simulation::{SimulationPath, ValidateSimulationError},
    store::{self, ReadOnlyStore, facts::RangeRowCounts},
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
    #[snafu(display("could not seed builtin categories"))]
    SeedCategories {
        source: store::categories::SeedCategoriesError,
    },
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
    #[snafu(display("could not prepare isolated benchmark database: {source}"))]
    Temporary { source: io::Error },
    #[snafu(display("could not install benchmark builtin data: {source}"))]
    Builtins {
        source: crate::rules::registry::OpenRegistryShellError,
    },
    #[snafu(display("could not prepare classification workload: {source}"))]
    Workload {
        source: Box<crate::store::facts::PrepareClassificationWorkloadError>,
    },
    #[snafu(display("prepared database has no builtin classification set"))]
    MissingClassification,
    #[snafu(display("runs must be positive"))]
    Runs,
    #[cfg(windows)]
    #[snafu(display("could not sample private bytes: {source}"))]
    Memory { source: windows::core::Error },
    #[snafu(display("could not write benchmark output: {source}"))]
    Output { source: io::Error },
}
/// Selects the measured query pipelines, optionally followed by raw fact scans.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum BenchQueryScope {
    #[default]
    All,
    Pipeline,
}
/// Uses an isolated workload copy, or reads a previously prepared database unchanged.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum BenchQueryInput {
    #[default]
    Isolated,
    Prepared,
}
#[derive(Debug)]
pub(crate) struct BenchQuery<'path> {
    path: &'path Path,
    runs: u32,
    scope: BenchQueryScope,
    input: BenchQueryInput,
}
impl<'path> BenchQuery<'path> {
    pub(crate) fn new(path: &'path Path, runs: u32) -> Self {
        Self {
            path,
            runs,
            scope: BenchQueryScope::All,
            input: BenchQueryInput::Isolated,
        }
    }
    pub(crate) fn with_scope(mut self, scope: BenchQueryScope) -> Self {
        self.scope = scope;
        self
    }
    pub(crate) fn with_input(mut self, input: BenchQueryInput) -> Self {
        self.input = input;
        self
    }
    pub(crate) fn run(&self) -> Result<(), BenchQueryError> {
        let path = SimulationPath::offline(self.path).context(PathSnafu)?;
        if self.input == BenchQueryInput::Prepared {
            let mut reader = ReadOnlyStore::open_facts(path.as_path()).context(OpenSnafu)?;
            let state = reader.rule_sets().load().context(RulesSnafu)?;
            snafu::ensure!(
                state
                    .sets
                    .keys()
                    .any(|key| key.as_str() == "builtin.classification"),
                MissingClassificationSnafu
            );
            return self.measure(reader, &mut io::stdout().lock());
        }
        let temporary = tempfile::Builder::new()
            .prefix("quartz-bench-query-")
            .tempdir()
            .context(TemporarySnafu)?;
        let directory = crate::data_directory::DataDirectory::new(temporary.path());
        let mut original = ReadOnlyStore::open_facts(path.as_path()).context(OpenSnafu)?;
        original
            .copy_simulation(&directory.database())
            .context(OpenSnafu)?;
        let mut store = crate::store::Store::open(&directory).context(OpenSnafu)?;
        store.seed_categories().context(SeedCategoriesSnafu)?;
        crate::rules::registry::RegistryShell::open(
            &mut store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("benchmark UTC origin"),
        )
        .context(BuiltinsSnafu)?;
        let mut output = io::stdout().lock();
        writeln!(
            output,
            "isolated snapshot copy; builtin categories and rules installed before timing; runs={}",
            self.runs
        )
        .context(OutputSnafu)?;
        let mut reader = ReadOnlyStore::open_facts(&directory.database()).context(OpenSnafu)?;
        let from = reader
            .extent()
            .context(ReadSnafu)?
            .ok_or(BenchQueryError::Empty)?
            .from_utc_ms;
        let baseline = memory::PrivateBytes::sample()?;
        reader = self.measure_pipeline(reader, from, baseline, &mut output)?;
        store
            .use_classification_workload()
            .map_err(|source| BenchQueryError::Workload {
                source: Box::new(source),
            })?;
        writeln!(output, "classification workload: editor=code.exe terminal=windowsterminal.exe player=vlc.exe browser=msedge.exe (unclassified)").context(OutputSnafu)?;
        reader = self.measure_pipeline(reader, from, baseline, &mut output)?;
        if self.scope == BenchQueryScope::All {
            self.measure_fact_reads(&mut reader, from, &mut output)?;
        }
        Ok(())
    }
    fn measure(
        &self,
        mut reader: ReadOnlyStore,
        output: &mut impl Write,
    ) -> Result<(), BenchQueryError> {
        use snafu::OptionExt;
        snafu::ensure!(self.runs > 0, RunsSnafu);
        let extent = reader.extent().context(ReadSnafu)?.context(EmptySnafu)?;
        let from = extent.from_utc_ms;
        reader = self.measure_pipeline(reader, from, memory::PrivateBytes::sample()?, output)?;
        match self.scope {
            BenchQueryScope::All => self.measure_fact_reads(&mut reader, from, output),
            BenchQueryScope::Pipeline => Ok(()),
        }
    }
    fn measure_fact_reads(
        &self,
        reader: &mut ReadOnlyStore,
        from: i64,
        output: &mut impl Write,
    ) -> Result<(), BenchQueryError> {
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
        idle: memory::PrivateBytes,
        output: &mut impl Write,
    ) -> Result<ReadOnlyStore, BenchQueryError> {
        use crate::query::{QueryJob, QueryRequest};
        use quartz_protocol::{
            MillisecondsPerPixel, QueryDayRequest, QueryPendingAwayRequest, QueryTimelineRequest,
            TimelineDetail,
        };
        let registry = quartz_domain::rules::registry::RuleRegistry::from_state(
            reader.rule_sets().load().context(RulesSnafu)?,
        )
        .context(RegistrySnafu)?;
        let rules = registry.snapshots().load();
        let local_date = jiff::Timestamp::from_millisecond(from)
            .map_err(crate::query::RunQueryError::from)
            .context(QuerySnafu)?
            .to_zoned_datetime(jiff::tz::TimeZone::system())
            .date();
        let offset = reader
            .query_settings()
            .map_err(crate::query::RunQueryError::from)
            .context(QuerySnafu)?
            .day_start_offset;
        let calendar =
            crate::query::calendar::DayCalendar::new(local_date, offset).context(QuerySnafu)?;
        // 60 seconds per pixel gives a 1,440-pixel daily timeline.
        const BENCH_PIXEL_MS: u64 = 60_000;
        writeln!(output, "pipeline local_date={local_date} system_zone={:?} day_start_offset_min={} from_utc_ms={} day_to_utc_ms={} ms_per_pixel={BENCH_PIXEL_MS}", jiff::tz::TimeZone::system(), offset.minutes(), calendar.range.start, calendar.range.end).context(OutputSnafu)?;
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
            QueryRequest::PendingAway(QueryPendingAwayRequest {
                local_date,
                cursor: None,
            }),
        ];
        snafu::ensure!(self.runs > 0, RunsSnafu);
        for query in queries {
            let name = match &query {
                QueryRequest::Facts(_) => unreachable!("benchmark workloads use derived queries"),
                QueryRequest::Day(_) => "pipeline.day",
                QueryRequest::PendingAway(_) => "pipeline.pending_away",
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
                let job = QueryJob::start(&mut snapshot_reader, query.clone(), rules.clone())
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
    classify: Vec<std::time::Duration>,
    aggregate: Vec<std::time::Duration>,
    page_encode: Vec<std::time::Duration>,
}
impl StageSamples {
    fn push(&mut self, timings: crate::query::timing::QueryTimings) {
        self.read.push(timings.read);
        self.slice.push(timings.slice);
        self.classify.push(timings.classify);
        self.aggregate.push(timings.aggregate);
        self.page_encode.push(timings.page_encode);
    }
    fn write(&self, name: &str, output: &mut impl Write) -> io::Result<()> {
        self.write_stage(name, "read", &self.read, output)?;
        self.write_stage(name, "slice", &self.slice, output)?;
        self.write_stage(name, "classification", &self.classify, output)?;
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
        let original = std::fs::read(&path).expect("original bytes");
        assert!(
            matches!(
                BenchQuery::new(&path, 2)
                    .with_input(BenchQueryInput::Prepared)
                    .with_scope(BenchQueryScope::Pipeline)
                    .run(),
                Err(BenchQueryError::MissingClassification)
            ),
            "prepared inputs must include builtin classification rules"
        );
        let isolated = DataDirectory::new(temporary.path().join("benchmark"));
        std::fs::create_dir(isolated.as_path()).expect("benchmark directory");
        ReadOnlyStore::open(&path)
            .expect("source database")
            .copy_simulation(&isolated.database())
            .expect("copy source");
        let mut store = crate::store::Store::open(&isolated).expect("copied store");
        store.seed_categories().expect("builtin categories");
        let shell = crate::rules::registry::RegistryShell::open(
            &mut store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("builtin data");
        store
            .use_classification_workload()
            .expect("representative application names");
        let mut reader = ReadOnlyStore::open(&isolated.database()).expect("read-only database");
        let extent = reader.extent().expect("extent").expect("capture extent");
        let mut output = Vec::new();
        BenchQuery::new(&path, 2)
            .measure(reader, &mut output)
            .expect("benchmark");
        let mut reader = store.take_query_reader();
        let request = crate::query::QueryRequest::Timeline(quartz_protocol::QueryTimelineRequest {
            start_utc_ms: extent.from_utc_ms,
            end_utc_ms: extent.to_utc_ms,
            detail: quartz_protocol::TimelineDetail::App,
            ms_per_pixel: quartz_protocol::MillisecondsPerPixel(std::num::NonZeroU64::MIN),
            cursor: None,
        });
        let completed =
            crate::query::QueryJob::start(&mut reader, request, shell.snapshots().load())
                .expect("mapped query")
                .run_measured()
                .expect("mapped result");
        let crate::query::QueryResponse::Timeline(response) = completed.response else {
            panic!("timeline");
        };
        assert!(
            response.segments.iter().any(|segment| matches!(
                segment.category_source,
                quartz_protocol::CategorySource::Rule { .. }
            )),
            "mapped workload must execute matching rules"
        );
        assert_eq!(
            std::fs::read(&path).expect("source bytes"),
            original,
            "benchmark preparation preserves its source"
        );
        assert!(
            matches!(
                store.use_classification_workload(),
                Err(
                    crate::store::facts::PrepareClassificationWorkloadError::MissingApplication { .. }
                )
            ),
            "missing simulation identities are explicit errors"
        );
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
            ".classification p50_ms=",
            ".aggregate_merge p50_ms=",
            ".paging_encode p50_ms=",
            ".memory ",
        ] {
            assert!(output.contains(name), "missing {name}: {output}");
        }
    }
}
