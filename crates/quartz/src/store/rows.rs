//! Persisted rows; integer enum columns use the stable encoded Rust types.

use super::{
    enums::{
        AnnotationKind, ApplicationKind, AvailabilityEndReason, AvailabilityReason, CaptureLevel,
        CategoryTendency, ClockEndReason, DataSource, GapReason, IdleException, IngestOutcome,
        IntervalEndReason, IntervalKind, LinkStatus, NoForegroundReason, RuleSetKind,
        RuleSetSource, RunEndReason, TitleStatus, WebEndReason, WebLevel,
    },
    schema::{
        annotations, applications, availability_intervals, browser_streams, capture_gaps,
        capture_runs, categories, clock_segments, data_day_versions, deletion_barriers, domains,
        enum_labels, ingest_receipts, intervals, rule_set_versions, rule_sets, settings, titles,
        web_intervals, window_instances,
    },
};
use diesel::{
    prelude::{AsChangeset, Insertable, Queryable, Selectable},
    sqlite::Sqlite,
};

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = annotations)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct AnnotationsRow {
    pub id: Option<i64>,
    pub uuid: String,
    pub kind: AnnotationKind,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub category_id: Option<i64>,
    pub label: Option<String>,
    pub note: Option<String>,
    pub created_utc_ms: i64,
    pub updated_utc_ms: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = applications)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct ApplicationsRow {
    pub id: Option<i64>,
    pub identity_key: String,
    pub kind: ApplicationKind,
    pub exe_path: Option<String>,
    pub aumid: Option<String>,
    pub exe_name: Option<String>,
    pub display_name: String,
    pub first_seen_utc_ms: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = availability_intervals)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct AvailabilityIntervalsRow {
    pub id: Option<i64>,
    pub run_id: i64,
    pub clock_segment_id: i64,
    pub reason: AvailabilityReason,
    pub start_utc_ms: i64,
    pub start_mono_ms: i64,
    pub end_utc_ms: Option<i64>,
    pub end_mono_ms: Option<i64>,
    pub confirmed_utc_ms: Option<i64>,
    pub confirmed_mono_ms: Option<i64>,
    pub end_reason: Option<AvailabilityEndReason>,
    pub last_input_utc_ms: Option<i64>,
    pub detected_utc_ms: Option<i64>,
    pub threshold_ms: Option<i64>,
    pub exception: Option<IdleException>,
    pub policy_version: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = browser_streams)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct BrowserStreamsRow {
    pub id: Option<i64>,
    pub browser: String,
    pub profile_instance_id: String,
    pub extension_id: String,
    pub label: Option<String>,
    pub first_seen_utc_ms: i64,
    pub last_seen_utc_ms: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = capture_gaps)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct CaptureGapsRow {
    pub id: Option<i64>,
    pub run_id: i64,
    pub clock_segment_id: i64,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub start_mono_ms: i64,
    pub end_mono_ms: i64,
    pub reason: GapReason,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = capture_runs)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct CaptureRunsRow {
    pub id: Option<i64>,
    pub device_id: String,
    pub win_session_id: i64,
    pub app_version: String,
    pub started_utc_ms: i64,
    pub confirmed_utc_ms: Option<i64>,
    pub ended_utc_ms: Option<i64>,
    pub end_reason: Option<RunEndReason>,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = categories)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct CategoriesRow {
    pub id: Option<i64>,
    pub uuid: String,
    pub parent_id: Option<i64>,
    pub builtin_key: Option<String>,
    pub name: Option<String>,
    pub color: String,
    pub tendency: CategoryTendency,
    pub sort_order: i64,
    pub archived: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = clock_segments)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct ClockSegmentsRow {
    pub id: Option<i64>,
    pub run_id: i64,
    pub start_utc_ms: i64,
    pub start_mono_ms: i64,
    pub confirmed_utc_ms: Option<i64>,
    pub confirmed_mono_ms: Option<i64>,
    pub end_utc_ms: Option<i64>,
    pub end_mono_ms: Option<i64>,
    pub end_reason: Option<ClockEndReason>,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = data_day_versions)]
#[diesel(primary_key(source, utc_date))]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct DataDayVersionsRow {
    pub source: DataSource,
    pub utc_date: i64,
    pub version: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = deletion_barriers)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct DeletionBarriersRow {
    pub id: Option<i64>,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    pub scope: String,
    pub created_utc_ms: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = domains)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct DomainsRow {
    pub id: Option<i64>,
    pub host: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = enum_labels)]
#[diesel(primary_key(enum_name, code))]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct EnumLabelsRow {
    pub enum_name: String,
    pub code: i32,
    pub label: String,
    pub deprecated: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = ingest_receipts)]
#[diesel(primary_key(stream_id, epoch, message_seq))]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct IngestReceiptsRow {
    pub stream_id: i64,
    pub epoch: i64,
    pub message_seq: i64,
    pub received_utc_ms: i64,
    pub outcome: IngestOutcome,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = intervals)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct IntervalsRow {
    pub id: Option<i64>,
    pub run_id: i64,
    pub sequence: i64,
    pub clock_segment_id: i64,
    pub kind: IntervalKind,
    pub no_foreground_reason: Option<NoForegroundReason>,
    pub app_id: Option<i64>,
    pub window_instance_id: Option<i64>,
    pub title_id: Option<i64>,
    pub title_status: TitleStatus,
    pub capture_level: CaptureLevel,
    pub privacy_rule_id: Option<String>,
    pub privacy_label_key: Option<String>,
    pub privacy_label: Option<String>,
    pub session_key: i64,
    pub start_utc_ms: i64,
    pub start_mono_ms: i64,
    pub end_utc_ms: Option<i64>,
    pub end_mono_ms: Option<i64>,
    pub confirmed_utc_ms: Option<i64>,
    pub confirmed_mono_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub end_reason: Option<IntervalEndReason>,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = rule_set_versions)]
#[diesel(primary_key(set_id, version))]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct RuleSetVersionsRow {
    pub set_id: i64,
    pub version: i64,
    pub content: String,
    pub content_hash: String,
    pub parent_version: Option<i64>,
    pub note: Option<String>,
    pub created_utc_ms: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = rule_sets)]
#[diesel(treat_none_as_null = true)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct RuleSetsRow {
    pub id: Option<i64>,
    pub key: String,
    pub kind: RuleSetKind,
    pub parent_id: Option<i64>,
    pub source: RuleSetSource,
    pub builtin_revision: Option<i64>,
    pub name: String,
    pub enabled: i32,
    pub deleted_utc_ms: Option<i64>,
    pub sibling_order: i64,
    pub current_version: i64,
    pub chain_id: Option<i64>,
    pub created_utc_ms: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = settings)]
#[diesel(primary_key(key))]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct SettingsRow {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = titles)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct TitlesRow {
    pub id: Option<i64>,
    pub text: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = web_intervals)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct WebIntervalsRow {
    pub id: Option<i64>,
    pub stream_id: i64,
    pub interval_id: i64,
    pub start_utc_ms: i64,
    pub end_utc_ms: Option<i64>,
    pub confirmed_utc_ms: Option<i64>,
    pub end_reason: Option<WebEndReason>,
    pub domain_id: Option<i64>,
    pub tab_title_id: Option<i64>,
    pub tab_title_status: TitleStatus,
    pub tab_id: Option<i64>,
    pub browser_window_id: Option<i64>,
    pub navigation_id: Option<i64>,
    pub audible: i32,
    pub link_status: LinkStatus,
    pub web_level: WebLevel,
    pub privacy_rule_id: Option<String>,
    pub privacy_label_key: Option<String>,
    pub privacy_label: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = window_instances)]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct WindowInstancesRow {
    pub id: Option<i64>,
    pub run_id: i64,
    pub app_id: i64,
    pub pid: i64,
    pub process_created_utc_ms: Option<i64>,
    pub hwnd: i64,
    pub generation: i64,
    pub retired_utc_ms: Option<i64>,
    pub window_class: Option<String>,
    pub first_seen_utc_ms: i64,
    pub last_seen_utc_ms: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = super::schema::timed_policies)]
#[diesel(primary_key(kind))]
#[diesel(check_for_backend(Sqlite))]
pub(super) struct TimedPoliciesRow {
    pub kind: super::enums::TimedPolicyKind,
    pub privacy_mode: Option<super::enums::PrivacyMode>,
    pub application_id: Option<i64>,
    pub started_utc_ms: i64,
    pub until_utc_ms: i64,
}
