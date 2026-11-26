//! Temporary query fixtures; SQL and database mutation stay inside store.

use super::*;
use crate::query::calendar::DayCalendar;
use crate::store::schema::data_day_versions;
use crate::store::versions::{UtcDay, VersionKey, VersionSource};
use crate::store::{
    enums::{
        ApplicationKind, AvailabilityEndReason, ClockEndReason, DataSource, IntervalEndReason,
        IntervalKind, RunEndReason, TitleStatus,
    },
    rows::{ApplicationsRow, CaptureRunsRow, TitlesRow},
};
use crate::{
    data_directory::DataDirectory,
    query::{QueryJob, QueryRequest, QueryResponse, RunQueryError},
};
use quartz_protocol::{
    MillisecondsPerPixel, QueryDayRequest, QueryTimelineRequest, TimelineDetail,
};

pub(crate) const FIXTURE_DATE: &str = "2025-01-02";
pub(super) const FACT_MS: i64 = 1000;
const FIXTURE_TITLE: &str = "private title fixture";
pub(crate) struct QueryFixture {
    pub store: Store,
    pub start: i64,
    pub count: i64,
    pub(crate) directory: tempfile::TempDir,
}
impl QueryFixture {
    pub(crate) fn new(count: i64) -> Self {
        let directory = tempfile::tempdir().expect("query fixture directory");
        let mut store =
            Store::open(&DataDirectory::new(directory.path())).expect("query fixture store");
        let calendar = DayCalendar::new(
            FIXTURE_DATE.parse().expect("fixture date"),
            crate::query::calendar::DayStartOffset::MIDNIGHT,
        )
        .expect("fixture day");
        let start = calendar.range.start;
        store
            .writer
            .immediate_transaction::<_, diesel::result::Error, _>(|connection| {
                diesel::insert_into(capture_runs::table)
                    .values(CaptureRunsRow {
                        id: Some(1),
                        device_id: "query-test".into(),
                        win_session_id: 1,
                        app_version: "test".into(),
                        started_utc_ms: start,
                        confirmed_utc_ms: None,
                        ended_utc_ms: Some(calendar.range.end),
                        end_reason: Some(RunEndReason::Shutdown),
                    })
                    .execute(connection)?;
                diesel::insert_into(clock_segments::table)
                    .values(ClockSegmentsRow {
                        id: Some(1),
                        run_id: 1,
                        start_utc_ms: start,
                        start_mono_ms: 0,
                        confirmed_utc_ms: None,
                        confirmed_mono_ms: None,
                        end_utc_ms: Some(calendar.range.end),
                        end_mono_ms: Some(calendar.range.end - start),
                        end_reason: Some(ClockEndReason::RunEnd),
                    })
                    .execute(connection)?;
                diesel::insert_into(titles::table)
                    .values(TitlesRow {
                        id: Some(1),
                        text: FIXTURE_TITLE.into(),
                    })
                    .execute(connection)?;
                for id in 1..=count {
                    Self::insert_fact(connection, start, id)?;
                }
                Ok(())
            })
            .expect("fixture facts");
        store.seed_categories().expect("builtin categories");
        crate::rules::registry::RegistryShell::open(
            &mut store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("initial builtin rules");
        Self {
            store,
            start,
            count,
            directory,
        }
    }
    fn insert_fact(
        connection: &mut diesel::SqliteConnection,
        start: i64,
        id: i64,
    ) -> Result<(), diesel::result::Error> {
        diesel::insert_into(applications::table)
            .values(ApplicationsRow {
                id: Some(id),
                identity_key: format!("exe:c:/fixture/app-{id}.exe"),
                kind: ApplicationKind::Exe,
                exe_path: Some(format!("c:/fixture/app-{id}.exe")),
                aumid: None,
                exe_name: Some(format!("app-{id}.exe")),
                display_name: format!("Application {id}"),
                first_seen_utc_ms: start,
            })
            .execute(connection)?;
        let mono = (id - 1) * FACT_MS;
        diesel::insert_into(intervals::table)
            .values(IntervalsRow {
                id: Some(id),
                run_id: 1,
                sequence: id,
                clock_segment_id: 1,
                kind: IntervalKind::Window,
                no_foreground_reason: None,
                app_id: Some(id),
                window_instance_id: None,
                title_id: Some(1),
                title_status: TitleStatus::Ok,
                capture_level: enums::CaptureLevel::Full,
                privacy_rule_id: None,
                privacy_label_key: None,
                privacy_label: None,
                session_key: id,
                start_utc_ms: start + mono,
                start_mono_ms: mono,
                end_utc_ms: Some(start + mono + FACT_MS),
                end_mono_ms: Some(mono + FACT_MS),
                confirmed_utc_ms: None,
                confirmed_mono_ms: None,
                duration_ms: Some(FACT_MS),
                end_reason: Some(IntervalEndReason::Switch),
            })
            .execute(connection)?;
        Ok(())
    }
    pub(crate) fn request_day(&self) -> QueryRequest {
        QueryRequest::Day(QueryDayRequest {
            local_date: FIXTURE_DATE.parse().expect("fixture date"),
            cursor: None,
        })
    }
    pub(crate) fn request_timeline(&self) -> QueryRequest {
        QueryRequest::Timeline(QueryTimelineRequest {
            start_utc_ms: self.start,
            end_utc_ms: self.start + (self.count + 1) * FACT_MS,
            detail: TimelineDetail::Raw,
            ms_per_pixel: MillisecondsPerPixel(std::num::NonZeroU64::MIN),
            cursor: None,
        })
    }
    pub(crate) fn rules(&mut self) -> std::sync::Arc<quartz_domain::rules::registry::RuleSnapshot> {
        crate::rules::registry::RegistryShell::open(
            &mut self.store,
            quartz_domain::capture::UtcInstant::try_from(0).expect("UTC"),
        )
        .expect("rules")
        .snapshots()
        .load()
    }
    pub(crate) fn start(&mut self, request: QueryRequest) -> QueryJob {
        let mut reader = self.store.take_query_reader();
        QueryJob::start(&mut reader, request, self.rules()).expect("start snapshot query")
    }
    pub(crate) fn run(&mut self, request: QueryRequest) -> Result<QueryResponse, RunQueryError> {
        let mut reader = self.store.take_query_reader();
        match QueryJob::start(&mut reader, request, self.rules()) {
            Ok(mut job) => {
                let result = loop {
                    match job.advance() {
                        Ok(Some(response)) => break Ok(response),
                        Ok(None) => {}
                        Err(error) => break Err(error),
                    }
                };
                self.store.return_query_reader(job.finish()?);
                result
            }
            Err(error) => {
                self.store
                    .return_query_reader(reader.expect("reader returned after rejected query"));
                Err(error)
            }
        }
    }
    pub(crate) fn append_fact(&mut self) {
        self.count += 1;
        let id = self.count;
        self.store
            .writer
            .immediate_transaction::<_, diesel::result::Error, _>(|connection| {
                Self::insert_fact(connection, self.start, id)
            })
            .expect("append fixture fact");
        self.bump_fact_version();
    }
    pub(crate) fn bump_fact_version(&mut self) {
        let day = self.start.div_euclid(UTC_DAY_MS);
        let version = diesel::insert_into(data_day_versions::table)
            .values((
                data_day_versions::source.eq(DataSource::Intervals),
                data_day_versions::utc_date.eq(day),
                data_day_versions::version.eq(1_i64),
            ))
            .on_conflict((data_day_versions::source, data_day_versions::utc_date))
            .do_update()
            .set(data_day_versions::version.eq(data_day_versions::version + 1_i64))
            .returning(data_day_versions::version)
            .get_result(&mut self.store.writer)
            .expect("bump fact version");
        self.store.pending_effects.days.insert(
            VersionKey {
                source: VersionSource(DataSource::Intervals.code()),
                day: UtcDay(day),
            },
            version,
        );
    }
    pub(crate) fn bump_metadata(&mut self) {
        let mut settings = crate::store::settings::Settings::new(&mut self.store.writer);
        let value = settings.metadata_version().expect("metadata query") + 1;
        settings
            .put_metadata_version(value)
            .expect("metadata update");
        self.store.pending_effects.metadata = Some(value);
    }
    pub(crate) fn mask_idle(&mut self, start: i64, end: i64) {
        diesel::insert_into(availability_intervals::table)
            .values(AvailabilityIntervalsRow {
                id: None,
                run_id: 1,
                clock_segment_id: 1,
                reason: enums::AvailabilityReason::Idle,
                start_utc_ms: self.start + start,
                start_mono_ms: start,
                end_utc_ms: Some(self.start + end),
                end_mono_ms: Some(end),
                confirmed_utc_ms: None,
                confirmed_mono_ms: None,
                end_reason: Some(AvailabilityEndReason::Resumed),
                last_input_utc_ms: None,
                detected_utc_ms: None,
                threshold_ms: None,
                exception: None,
                policy_version: 1,
            })
            .execute(&mut self.store.writer)
            .expect("idle mask");
    }
    pub(crate) fn mask_locked(&mut self, start: i64, end: i64) {
        self.mask_idle(start, end);
        diesel::update(
            availability_intervals::table
                .filter(availability_intervals::start_utc_ms.eq(self.start + start)),
        )
        .set(availability_intervals::reason.eq(enums::AvailabilityReason::Locked))
        .execute(&mut self.store.writer)
        .expect("locked mask");
    }
    pub(crate) fn annotate(
        &mut self,
        id: u32,
        kind: quartz_protocol::AnnotationKind,
        start: i64,
        end: i64,
        category: Option<quartz_protocol::BuiltinCategory>,
    ) {
        self.store
            .create_annotations(
                quartz_protocol::AnnotationsCreateRequest {
                    entities: vec![quartz_protocol::CreateAnnotation {
                        uuid: quartz_protocol::AnnotationId(
                            format!("abcdef00-0000-4000-8000-{id:012x}")
                                .parse()
                                .expect("annotation UUID"),
                        ),
                        kind,
                        values: quartz_protocol::AnnotationValues {
                            start_utc_ms: self.start + start,
                            end_utc_ms: self.start + end,
                            category: category
                                .map(|key| quartz_protocol::AnnotationCategory::Builtin { key }),
                            label_preset: None,
                            label: (kind == quartz_protocol::AnnotationKind::Note)
                                .then(|| "fixture".into()),
                            note: None,
                        },
                    }],
                },
                quartz_domain::capture::UtcInstant::try_from(self.start).expect("UTC"),
            )
            .expect("annotation write");
    }
    pub(crate) fn open_last_fact(&mut self, confirmed_ms: i64) {
        diesel::update(intervals::table.filter(intervals::id.eq(self.count)))
            .set((
                intervals::end_utc_ms.eq(None::<i64>),
                intervals::end_mono_ms.eq(None::<i64>),
                intervals::confirmed_utc_ms.eq(Some(self.start + confirmed_ms)),
                intervals::confirmed_mono_ms.eq(Some(confirmed_ms)),
                intervals::duration_ms.eq(None::<i64>),
            ))
            .execute(&mut self.store.writer)
            .expect("open confirmed row");
    }
    pub(crate) fn collapse_foreground_times(&mut self) {
        diesel::update(intervals::table)
            .set((
                intervals::start_utc_ms.eq(self.start),
                intervals::end_utc_ms.eq(Some(self.start + FACT_MS)),
                intervals::start_mono_ms.eq(0_i64),
                intervals::end_mono_ms.eq(Some(FACT_MS)),
            ))
            .execute(&mut self.store.writer)
            .expect("same timestamp foreground facts");
    }
    pub(crate) fn hide_titles(&mut self) {
        self.redact_titles(enums::CaptureLevel::TitleHidden);
    }
    pub(crate) fn redact_titles(&mut self, level: enums::CaptureLevel) {
        diesel::update(intervals::table)
            .set(intervals::capture_level.eq(level))
            .execute(&mut self.store.writer)
            .expect("fixture capture level");
    }
}
