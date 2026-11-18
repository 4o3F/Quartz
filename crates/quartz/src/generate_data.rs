//! Deterministic capture workloads persisted through the production capture writer.

use crate::{
    data_directory::DataDirectory,
    simulation::{SimulationPath, ValidateSimulationError},
    store::{
        self, ReadOnlyStore, Store,
        capture::{CaptureKeys, RunMetadata, WindowsSessionId},
        facts::UTC_DAY_MS,
    },
};
use clap::ValueEnum;
use quartz_domain::capture::{
    self as capture, AppIdentity, AvailabilityEndReason, AvailabilityReason, CaptureGap,
    CaptureLevel, CaptureWrite, CapturedFields, ClockEndReason, IdleException, IdlePolicy,
    IdleRecord, InstanceGeneration, IntervalEndReason, IntervalFact, MonoInstant,
    NoForegroundReason, OpenAvailability, OpenInterval, ProcessId, RowId, RunEndReason, RunId,
    SegmentId, SessionKey, StopReason, TimeRange, Timestamp, Title, WindowHandle, WindowId,
    WindowInstance,
};
use snafu::{ResultExt, Snafu};
use std::{
    io::{self, Write},
    num::TryFromIntError,
    path::Path,
    time::Instant,
};

/// Annual workload size from data model 03 §4.6.
pub(crate) const DEFAULT_DAYS: u32 = 365;
pub(crate) const DEFAULT_SEED: u64 = 1;
/// Data model 03 §4.6 annual workloads report progress every thirty simulated days.
const PROGRESS_DAYS: u32 = 30;
/// Data model 03 §4.6: weekend activity and row/title counts are halved.
const WEEKEND_DIVISOR: usize = 2;
/// Data model 03 §4.6: each run includes one locked hour and one suspended hour.
const STOP_HOURS: i64 = 2;
/// Data model 03 §4.6 assumes ten active hours; weekends use half the workload.
const ACTIVE_HOURS: i64 = 10;
const HOUR_MS: i64 = 3_600_000;
/// Starts one second past 18:00 UTC to force a real writer midnight split.
const START_OFFSET_MS: i64 = 18 * HOUR_MS + 1_000;
/// Stable calendar origin, independent of the date the command is executed.
const EPOCH_MS: i64 = 1_735_689_600_000; // 2025-01-01T00:00:00Z (Wednesday)
const EPOCH_WEEKDAY: u32 = 2;
const WEEK_DAYS: u32 = 7;
const WORK_DAYS: u32 = 5;
/// Data model 03 §4.6: 5% of new titles are reused on subsequent days.
const REUSE_PERCENT: usize = 5;
const PERCENT: usize = 100;
/// One long title per day approaches the 4,096 UTF-16 unit cap (08 §2.2).
const TITLE_CAP: usize = capture::limits::TITLE_UTF16_UNITS;
/// Ordinary UTF-8 title byte lengths; the separate capped title adds a long tail (03 §4.6).
const TITLE_LENGTHS: [usize; 4] = [24, 36, 48, 72];
/// Data model 03 §4.6: dictionary titles average sixty UTF-8 bytes, including the long tail.
const MEAN_TITLE_BYTES: usize = 60;
/// Heavy-profile new-title count from data model 03 §4.6.
const HEAVY_NEW_TITLES: usize = 1_700;
/// Data model 03 §4.6 workload: periodic no-foreground slices preserve the new-title prefix.
const NO_FOREGROUND_PERIOD: usize = 97;
/// Resources 08 §2.2: forty one-second title changes per burst, alternating player and terminal.
const CHURN_BLOCK: usize = 200;
const CHURN_WINDOW: usize = 40;
const CHURN_SLICE_MS: i64 = 1_000;
/// Four application identities exercise switches and terminal/player churn (03 §4.6).
const APP_NAMES: [&str; 4] = ["editor", "browser", "terminal", "player"];
const APP_COUNT: usize = APP_NAMES.len();
const CHURN_APP_COUNT: usize = 2;
/// Availability evidence is included in the 03 §4.6 simulation workload.
const IDLE_OFFSET_MS: i64 = HOUR_MS;
const IDLE_LENGTH_MS: i64 = 20 * 60_000;
const IDLE_THRESHOLD_MS: i64 = 180_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(crate) enum Profile {
    Light,
    #[default]
    Typical,
    Heavy,
}
#[derive(Debug, Clone, Copy)]
struct ProfileParameters {
    intervals: usize,
    web_slices: usize,
    new_titles: usize,
}
/// Data model 03 §4.6 workload parameters before weekend scaling and midnight splits.
const LIGHT_PROFILE: ProfileParameters = ProfileParameters {
    intervals: 600,
    web_slices: 250,
    new_titles: 160,
};
const TYPICAL_PROFILE: ProfileParameters = ProfileParameters {
    intervals: 2_000,
    web_slices: 900,
    new_titles: 550,
};
const HEAVY_PROFILE: ProfileParameters = ProfileParameters {
    intervals: 6_000,
    web_slices: 2_500,
    new_titles: HEAVY_NEW_TITLES,
};
impl Profile {
    fn parameters(self) -> ProfileParameters {
        match self {
            Self::Light => LIGHT_PROFILE,
            Self::Typical => TYPICAL_PROFILE,
            Self::Heavy => HEAVY_PROFILE,
        }
    }
}

#[derive(Debug, Snafu)]
pub(crate) enum GenerateDataError {
    #[snafu(display("{source}"))]
    Path { source: ValidateSimulationError },
    #[snafu(display("could not create simulation database: {source}"))]
    Open { source: store::OpenStoreError },
    #[snafu(display("could not write simulation capture batch: {source}"))]
    Capture {
        source: store::capture::ExecuteCaptureBatchError,
    },
    #[snafu(display("could not construct simulation time: {source}"))]
    Time { source: capture::AdvanceTimeError },
    #[snafu(display("could not open simulation for validation: {source}"))]
    Read { source: store::OpenStoreError },
    #[snafu(display("{source}"))]
    Check {
        source: store::facts::ReadFactsError,
    },
    #[snafu(display("could not write progress or inspect simulation size: {source}"))]
    Output { source: io::Error },
    #[snafu(display("simulation count is outside its supported integer range: {source}"))]
    Count { source: TryFromIntError },
    #[snafu(display("days must be positive"))]
    Days,
}

/// SplitMix64 constants for the seeded 03 §4.6 workloads.
const RANDOM_INCREMENT: u64 = 0x9e3779b97f4a7c15;
const RANDOM_FIRST_MULTIPLIER: u64 = 0xbf58476d1ce4e5b9;
const RANDOM_SECOND_MULTIPLIER: u64 = 0x94d049bb133111eb;
const RANDOM_FIRST_SHIFT: u32 = 30;
const RANDOM_SECOND_SHIFT: u32 = 27;
const RANDOM_FINAL_SHIFT: u32 = 31;

/// SplitMix64 gives a reproducible stream, including seed zero, without external state.
#[derive(Debug)]
struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(RANDOM_INCREMENT);
        let mut value = self.0;
        value = (value ^ (value >> RANDOM_FIRST_SHIFT)).wrapping_mul(RANDOM_FIRST_MULTIPLIER);
        value = (value ^ (value >> RANDOM_SECOND_SHIFT)).wrapping_mul(RANDOM_SECOND_MULTIPLIER);
        value ^ (value >> RANDOM_FINAL_SHIFT)
    }
    fn index(&mut self, length: usize) -> usize {
        let bound = u64::try_from(length).expect("collection length fits u64 on supported targets");
        usize::try_from(self.next() % bound).expect("remainder is less than the collection length")
    }
}

#[derive(Debug)]
pub(crate) struct GenerateData<'path> {
    path: &'path Path,
    profile: Profile,
    seed: u64,
    days: u32,
}
impl<'path> GenerateData<'path> {
    pub(crate) fn new(path: &'path Path, profile: Profile, seed: u64, days: u32) -> Self {
        Self {
            path,
            profile,
            seed,
            days,
        }
    }
    pub(crate) fn run(&self) -> Result<(), GenerateDataError> {
        let production = DataDirectory::resolve().map_err(|source| GenerateDataError::Path {
            source: ValidateSimulationError::Directory { source },
        })?;
        self.generate(&production, &mut io::stdout().lock())
    }
    pub(crate) fn generate(
        &self,
        production: &DataDirectory,
        output: &mut impl Write,
    ) -> Result<(), GenerateDataError> {
        snafu::ensure!(self.days > 0, DaysSnafu);
        let path = SimulationPath::validate(self.path, production).context(PathSnafu)?;
        path.reserve().context(PathSnafu)?;
        let started = Instant::now();
        let mut store = Store::create_simulation(path.as_path()).context(OpenSnafu)?;
        let metadata = RunMetadata {
            device_id: format!("simulation-{}", self.seed),
            win_session_id: WindowsSessionId::new(1),
            app_version: env!("CARGO_PKG_VERSION").into(),
        };
        let parameters = self.profile.parameters();
        writeln!(
            output,
            concat!(
                "profile={:?} seed={} days={} ",
                "weekday_intervals={} weekday_new_titles={} requested_web_slices={}",
            ),
            self.profile,
            self.seed,
            self.days,
            parameters.intervals,
            parameters.new_titles,
            parameters.web_slices,
        )
        .context(OutputSnafu)?;
        writeln!(
            output,
            concat!(
                "web_intervals skipped: CaptureWrite has no web writer (PendingM5); ",
                "requested multi-web slices={}/workday",
            ),
            parameters.web_slices,
        )
        .context(OutputSnafu)?;
        let mut workload = Workload {
            random: Random(self.seed),
            reusable: Vec::new(),
        };
        let mut writes_count = 0_u64;
        for day in 0..self.days {
            let plan = workload.day(day, self.profile)?;
            let mut keys = CaptureKeys::default();
            store
                .capture_transaction(&mut keys, &metadata, |capture| {
                    capture.execute_writes(RunId::new(u64::from(day) + 1), &plan)
                })
                .context(CaptureSnafu)?;
            writes_count += u64::try_from(plan.len()).context(CountSnafu)?;
            if (day + 1).is_multiple_of(PROGRESS_DAYS) || day + 1 == self.days {
                writeln!(
                    output,
                    "progress days={}/{} elapsed_s={:.3}",
                    day + 1,
                    self.days,
                    started.elapsed().as_secs_f64()
                )
                .context(OutputSnafu)?;
            }
        }
        store.finish_simulation();
        let mut reader = ReadOnlyStore::open_facts(path.as_path()).context(ReadSnafu)?;
        let mut sample_days = vec![0, self.days / 2, self.days - 1];
        sample_days.sort_unstable();
        sample_days.dedup();
        for day in &sample_days {
            let from = EPOCH_MS + i64::from(*day) * UTC_DAY_MS + START_OFFSET_MS;
            reader
                .check_consistency(from, from + ACTIVE_HOURS * HOUR_MS + STOP_HOURS * HOUR_MS)
                .context(CheckSnafu)?;
        }
        writeln!(output, "consistency=ok sampled_days={}", sample_days.len())
            .context(OutputSnafu)?;
        let counts = reader.row_counts().context(CheckSnafu)?;
        for table in counts.tables() {
            writeln!(output, "rows {}={}", table.name, table.rows).context(OutputSnafu)?;
        }
        let bytes = std::fs::metadata(path.as_path())
            .context(OutputSnafu)?
            .len();
        let elapsed = started.elapsed().as_secs_f64();
        let writes_per_second =
            f64::from(u32::try_from(writes_count).context(CountSnafu)?) / elapsed;
        let fact_rows_per_second =
            f64::from(u32::try_from(counts.fact_rows()).context(CountSnafu)?) / elapsed;
        writeln!(
            output,
            concat!(
                "complete elapsed_s={:.3} writes={} writes_per_s={:.0} ",
                "fact_rows_per_s={:.0} database_bytes={}",
            ),
            elapsed, writes_count, writes_per_second, fact_rows_per_second, bytes,
        )
        .context(OutputSnafu)?;
        Ok(())
    }
}
#[derive(Debug)]
struct SyntheticInterval {
    range: TimeRange,
    fact: IntervalFact,
}

#[derive(Debug)]
struct Workload {
    random: Random,
    reusable: Vec<String>,
}
impl Workload {
    fn day_divisor(&self, day: u32) -> usize {
        if (day + EPOCH_WEEKDAY) % WEEK_DAYS < WORK_DAYS {
            1
        } else {
            WEEKEND_DIVISOR
        }
    }
    fn boundary(&self, index: usize, count: usize, active: i64) -> i64 {
        let churn_before = |index: usize| {
            index / CHURN_BLOCK * CHURN_WINDOW + (index % CHURN_BLOCK).min(CHURN_WINDOW)
        };
        let short = i64::try_from(churn_before(index)).expect("profile slice counts fit i64");
        let total_short = i64::try_from(churn_before(count)).expect("profile slice counts fit i64");
        let position = i64::try_from(index).expect("profile slice indices fit i64");
        let slices = i64::try_from(count).expect("profile slice counts fit i64");
        short * CHURN_SLICE_MS
            + (position - short) * (active - total_short * CHURN_SLICE_MS) / (slices - total_short)
    }
    fn at(&self, day: u32, offset: i64) -> Result<Timestamp, GenerateDataError> {
        let elapsed = i64::from(day) * UTC_DAY_MS + START_OFFSET_MS + offset;
        let divisor = i64::try_from(self.day_divisor(day)).expect("day divisor is one or two");
        let suspend_start = ACTIVE_HOURS * HOUR_MS / divisor + HOUR_MS;
        let suspended = (offset - suspend_start).clamp(0, HOUR_MS);
        Ok(Timestamp {
            utc: capture::UtcInstant::try_from(EPOCH_MS + elapsed).context(TimeSnafu)?,
            mono: MonoInstant::try_from(elapsed - suspended).context(TimeSnafu)?,
        })
    }
    fn titles(&mut self, day: u32, count: usize) -> Vec<String> {
        let first_title =
            u64::from(day) * u64::try_from(HEAVY_NEW_TITLES).expect("profile title count fits u64");
        let prefixes: Vec<_> = (0..count)
            .map(|index| {
                format!(
                    "文{:x}",
                    first_title + u64::try_from(index).expect("profile title index fits u64")
                )
            })
            .collect();
        let mut lengths: Vec<_> = prefixes
            .iter()
            .enumerate()
            .map(|(index, prefix)| {
                if index == 0 {
                    TITLE_CAP + prefix.len() - prefix.encode_utf16().count()
                } else {
                    TITLE_LENGTHS[self.random.index(TITLE_LENGTHS.len())].max(prefix.len())
                }
            })
            .collect();
        // Balance ordinary padding around the sampled bins while preserving unique prefixes.
        let budget = count * MEAN_TITLE_BYTES;
        let total: usize = lengths.iter().sum();
        if total < budget {
            let mut remaining = budget - total;
            for (index, length) in lengths.iter_mut().enumerate().skip(1) {
                let extra = remaining.div_ceil(count - index);
                *length += extra;
                remaining -= extra;
            }
        } else {
            let mut remaining = total - budget;
            for (length, prefix) in lengths.iter_mut().zip(&prefixes).skip(1) {
                let reduction = remaining.min(*length - prefix.len());
                *length -= reduction;
                remaining -= reduction;
            }
        }
        prefixes
            .into_iter()
            .zip(lengths)
            .map(|(prefix, length)| {
                let padding = length - prefix.len();
                format!("{prefix}{}", "x".repeat(padding))
            })
            .collect()
    }
    fn day(&mut self, day: u32, profile: Profile) -> Result<Vec<CaptureWrite>, GenerateDataError> {
        let parameters = profile.parameters();
        let divisor = self.day_divisor(day);
        let count = parameters.intervals / divisor;
        let new_titles = parameters.new_titles / divisor;
        let active =
            ACTIVE_HOURS * HOUR_MS / i64::try_from(divisor).expect("day divisor is one or two");
        let run = RunId::new(u64::from(day) + 1);
        let segment = SegmentId::from_persisted(1);
        let start = self.at(day, 0)?;
        let mut writes = vec![
            CaptureWrite::OpenRun { run, at: start },
            CaptureWrite::OpenClockSegment { segment, at: start },
        ];
        for (app, name) in APP_NAMES.iter().enumerate() {
            let process = u32::try_from(app + 1).expect("application identities fit u32");
            let window = u64::from(process);
            writes.push(CaptureWrite::ObserveWindow {
                instance: WindowInstance {
                    id: WindowId::from_persisted(window),
                    handle: WindowHandle::new(window),
                    process: ProcessId::new(process),
                    process_created_utc: Some(start.utc),
                    generation: InstanceGeneration::initial(),
                    class: "SimulationWindow".into(),
                    app: AppIdentity::Exe(format!("c:\\simulation\\{name}.exe")),
                    exe_name: Some(format!("{name}.exe")),
                    display_name: (*name).into(),
                    first_seen: start,
                },
                at: start,
            });
        }
        let titles = self.titles(day, new_titles);
        let reusable_before = self.reusable.len();
        let mut intervals = Vec::with_capacity(count);
        let mut gap = None;
        let mut next_row = 1;
        for index in 0..count {
            let from = self.boundary(index, count, active);
            let to = self.boundary(index + 1, count, active);
            let range = TimeRange {
                start: self.at(day, from)?,
                end: self.at(day, to)?,
            };
            // A missing observation replaces exactly one slot, preserving total coverage.
            if day.is_multiple_of(WEEK_DAYS) && index == count - 1 {
                gap = Some(range);
                continue;
            }
            let app = if index % CHURN_BLOCK < CHURN_WINDOW {
                APP_COUNT - 1 - (index / CHURN_BLOCK) % CHURN_APP_COUNT
            } else {
                self.random.index(APP_COUNT)
            };
            let text = if index < new_titles {
                titles[index].clone()
            } else if reusable_before > 0 && index % PERCENT < REUSE_PERCENT {
                self.reusable[self.random.index(reusable_before)].clone()
            } else {
                titles[self.random.index(titles.len())].clone()
            };
            let title = Title::readable(&text);
            let instance = WindowId::from_persisted(
                u64::try_from(app + 1).expect("application identities fit u64"),
            );
            let fact = if index >= new_titles && index.is_multiple_of(NO_FOREGROUND_PERIOD) {
                IntervalFact::NoForeground(NoForegroundReason::Empty)
            } else {
                IntervalFact::Window {
                    instance: Some(instance),
                    fields: CapturedFields {
                        level: CaptureLevel::Full,
                        title: title.text().map(str::to_owned),
                        title_status: title.status(),
                        privacy: None,
                        web: quartz_domain::rules::WebAction::Full,
                    },
                }
            };
            intervals.push(SyntheticInterval { range, fact });
        }
        for (index, interval) in intervals.iter().enumerate() {
            let reason = match intervals.get(index + 1).map(|next| &next.fact) {
                Some(IntervalFact::NoForeground(_)) => IntervalEndReason::NoForeground,
                Some(IntervalFact::Window { instance: next, .. }) => match &interval.fact {
                    IntervalFact::Window { instance, .. } => {
                        if instance == next {
                            IntervalEndReason::Title
                        } else {
                            IntervalEndReason::Switch
                        }
                    }
                    IntervalFact::NoForeground(_) => IntervalEndReason::Switch,
                },
                None if gap.is_some() => IntervalEndReason::CaptureGap,
                None => IntervalEndReason::Locked,
            };
            let pieces = interval.range.split_midnights().context(TimeSnafu)?;
            for (piece_index, piece) in pieces.iter().enumerate() {
                let id = RowId::from_persisted(next_row);
                next_row += 1;
                writes.push(CaptureWrite::OpenInterval(OpenInterval {
                    id,
                    segment,
                    session: SessionKey::from_persisted(
                        u64::try_from(index).expect("profile slice index fits u64") + 1,
                    ),
                    start: piece.start,
                    fact: interval.fact.clone(),
                }));
                writes.push(CaptureWrite::CloseInterval {
                    id,
                    at: piece.end,
                    reason: if piece_index + 1 == pieces.len() {
                        reason
                    } else {
                        IntervalEndReason::MidnightSplit
                    },
                });
            }
        }
        if let Some(range) = gap {
            writes.push(CaptureWrite::RecordGap(CaptureGap {
                segment,
                range,
                reason: capture::GapReason::EventOverload,
            }));
        }
        self.reusable.extend(
            titles
                .into_iter()
                .take(new_titles * REUSE_PERCENT / PERCENT),
        );
        let idle_start = self.at(day, IDLE_OFFSET_MS)?;
        let idle_id = RowId::from_persisted(next_row);
        next_row += 1;
        writes.push(CaptureWrite::OpenAvailability(OpenAvailability {
            id: idle_id,
            segment,
            reason: AvailabilityReason::Idle,
            start: idle_start,
            idle: Some(IdleRecord {
                last_input: self.at(day, IDLE_OFFSET_MS - IDLE_THRESHOLD_MS)?,
                start: idle_start,
                detected: idle_start,
                policy: IdlePolicy::default(),
            }),
        }));
        writes.push(CaptureWrite::CloseAvailability {
            id: idle_id,
            at: self.at(day, IDLE_OFFSET_MS + IDLE_LENGTH_MS)?,
            reason: AvailabilityEndReason::Exception(IdleException::KeepTiming),
        });
        let mut current_segment = segment;
        for (index, reason) in [StopReason::Locked, StopReason::Suspended]
            .into_iter()
            .enumerate()
        {
            let id = RowId::from_persisted(next_row);
            next_row += 1;
            let from = self.at(
                day,
                active + i64::try_from(index).expect("profile slice indices fit i64") * HOUR_MS,
            )?;
            let to = self.at(
                day,
                active
                    + (i64::try_from(index).expect("profile slice indices fit i64") + 1) * HOUR_MS,
            )?;
            writes.push(CaptureWrite::OpenAvailability(OpenAvailability {
                id,
                segment: current_segment,
                reason: AvailabilityReason::Stopped(reason),
                start: from,
                idle: None,
            }));
            writes.push(CaptureWrite::CloseAvailability {
                id,
                at: to,
                reason: AvailabilityEndReason::Resumed,
            });
            if index == 0 && day.is_multiple_of(WEEK_DAYS) {
                writes.push(CaptureWrite::CloseClockSegment {
                    segment: current_segment,
                    at: to,
                    reason: ClockEndReason::TimeChange,
                });
                current_segment = SegmentId::from_persisted(2);
                writes.push(CaptureWrite::OpenClockSegment {
                    segment: current_segment,
                    at: to,
                });
            }
        }
        let end = self.at(day, active + STOP_HOURS * HOUR_MS)?;
        writes.push(CaptureWrite::CloseClockSegment {
            segment: current_segment,
            at: end,
            reason: ClockEndReason::RunEnd,
        });
        writes.push(CaptureWrite::CloseRun {
            run,
            at: end,
            reason: RunEndReason::Shutdown,
        });
        Ok(writes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn fixed_seed_reproduces_rows_and_conserves_duration() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let production = DataDirectory::new(directory.path().join("production"));
        let mut results = Vec::new();
        for name in ["first.db", "second.db"] {
            let path = directory.path().join(name);
            GenerateData::new(&path, Profile::Light, 42, 3)
                .generate(&production, &mut Vec::new())
                .expect("generate workload");
            let mut reader = ReadOnlyStore::open(&path).expect("read generated database");
            let extent = reader.extent().expect("extent").expect("capture run");
            let from = extent.from_utc_ms;
            let to = extent.to_utc_ms;
            reader
                .check_consistency(from, to)
                .expect("duration conservation");
            let rows = reader.dump_range(from, to).expect("facts");
            results.push((
                reader.row_counts().expect("row counts"),
                Sha256::digest(serde_json::to_vec(&rows).expect("canonical facts")).to_vec(),
            ));
        }
        assert_eq!(
            results[0], results[1],
            "same seed must reproduce facts and dictionaries"
        );
    }
    #[test]
    fn profiles_cover_title_counts_long_titles_and_one_second_churn() {
        use std::collections::BTreeSet;
        for (profile, expected_titles) in [
            (Profile::Light, 160),
            (Profile::Typical, 550),
            (Profile::Heavy, 1_700),
        ] {
            let mut workload = Workload {
                random: Random(7),
                reusable: Vec::new(),
            };
            let writes = workload.day(0, profile).expect("profile plan");
            let titles: BTreeSet<_> = writes
                .iter()
                .filter_map(|write| match write {
                    CaptureWrite::OpenInterval(OpenInterval {
                        fact: IntervalFact::Window { fields, .. },
                        ..
                    }) => fields.title.as_deref(),
                    _ => None,
                })
                .collect();
            assert_eq!(
                titles.len(),
                expected_titles,
                "distinct titles for {profile:?}"
            );
            assert_eq!(
                titles.iter().map(|title| title.len()).sum::<usize>(),
                expected_titles * MEAN_TITLE_BYTES,
                "mean UTF-8 title bytes for {profile:?}"
            );
            assert!(
                titles
                    .iter()
                    .any(|title| title.encode_utf16().count() == TITLE_CAP),
                "long title for {profile:?}"
            );
            let short_title_change = writes.windows(2).any(|pair| match (&pair[0], &pair[1]) {
                (
                    CaptureWrite::OpenInterval(row),
                    CaptureWrite::CloseInterval {
                        at,
                        reason: IntervalEndReason::Title,
                        ..
                    },
                ) => {
                    at.utc.get().as_millisecond() - row.start.utc.get().as_millisecond()
                        == CHURN_SLICE_MS
                }
                _ => false,
            });
            assert!(short_title_change, "one-second title churn for {profile:?}");
        }
    }

    #[test]
    fn plans_pass_domain_conservation_checker() {
        let mut workload = Workload {
            random: Random(0),
            reusable: Vec::new(),
        };
        for day in [0, 3, 7] {
            let plan = workload.day(day, Profile::Light).expect("capture plan");
            capture::test_support::assert_conservation(&plan);
        }
    }
    #[test]
    fn generation_refuses_existing_and_production_paths() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let production = DataDirectory::new(temporary.path().join("production"));
        let path = temporary.path().join("exists.db");
        std::fs::write(&path, b"keep").expect("existing file");
        assert!(
            GenerateData::new(&path, Profile::Light, 1, 1)
                .generate(&production, &mut Vec::new())
                .is_err()
        );
        assert_eq!(std::fs::read(&path).expect("preserved file"), b"keep");
        assert!(
            GenerateData::new(&production.database(), Profile::Light, 1, 1)
                .generate(&production, &mut Vec::new())
                .is_err()
        );
    }
}
