//! Dictionary enrichment and JSON presentation for capture dumps.

use super::super::{
    ReadOnlyStore, enums,
    enums::IntegerCode,
    rows::{CaptureRunsRow, ClockSegmentsRow, TimedPoliciesRow},
    schema::{applications, intervals, titles, window_instances},
};
use super::{
    Fact, FactDetails, FactEnd, FactEndReason, FactKind, FactReason, ReadFactsError, WindowDetails,
};
use diesel::{
    Connection, ExpressionMethods, JoinOnDsl, NullableExpressionMethods, QueryDsl, RunQueryDsl,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// SQLite's guaranteed minimum bind limit, data model 03 §4.5.
const SQLITE_BIND_LIMIT: usize = 999;

#[derive(Debug, diesel::Queryable)]
struct IntervalDetails {
    id: i64,
    display_name: Option<String>,
    exe_path: Option<String>,
    exe_name: Option<String>,
    title: Option<String>,
    hwnd: Option<i64>,
    generation: Option<i64>,
    pid: Option<i64>,
    window_class: Option<String>,
}
impl ReadOnlyStore {
    /// Reads the same bounded SQL scans as the query pipeline inside one WAL snapshot.
    pub(crate) fn dump_range(&mut self, from: i64, to: i64) -> Result<Vec<Fact>, ReadFactsError> {
        self.connection.transaction(|connection| {
            let mut cursor = super::super::query::ScanCursor::initial();
            let mut facts = Vec::new();
            while let Some(batch) = cursor.read_facts(
                connection,
                quartz_domain::facts::QueryRange {
                    start: from,
                    end: to,
                },
            )? {
                facts.extend(batch);
            }
            facts.sort_by(crate::store::facts::Fact::chronological_cmp);
            Ok(facts)
        })
    }
}
impl Fact {
    pub(in crate::store) fn enrich(
        connection: &mut diesel::SqliteConnection,
        facts: &mut [Fact],
    ) -> Result<(), ReadFactsError> {
        let ids: Vec<_> = facts.iter().map(|fact| fact.id).collect();
        let mut details = BTreeMap::new();
        for chunk in ids.chunks(SQLITE_BIND_LIMIT) {
            let rows = intervals::table
                .left_join(applications::table.on(intervals::app_id.eq(applications::id)))
                .left_join(titles::table.on(intervals::title_id.eq(titles::id)))
                .left_join(
                    window_instances::table
                        .on(intervals::window_instance_id.eq(window_instances::id)),
                )
                .filter(intervals::id.eq_any(chunk))
                .select((
                    intervals::id.assume_not_null(),
                    applications::display_name.nullable(),
                    applications::exe_path.nullable(),
                    applications::exe_name.nullable(),
                    titles::text.nullable(),
                    window_instances::hwnd.nullable(),
                    window_instances::generation.nullable(),
                    window_instances::pid.nullable(),
                    window_instances::window_class.nullable(),
                ))
                .load::<IntervalDetails>(connection)?;
            details.extend(rows.into_iter().map(|row| (row.id, row)));
        }
        for fact in facts {
            if let Some(row) = details.remove(&fact.id) {
                fact.window = Some(WindowDetails {
                    display_name: row.display_name,
                    exe_path: row.exe_path,
                    exe_name: row.exe_name,
                    title: row.title,
                    hwnd: row.hwnd,
                    generation: row.generation,
                    pid: row.pid,
                    window_class: row.window_class,
                });
            }
        }

        Ok(())
    }
}
impl CaptureRunsRow {
    pub(in crate::store) fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let mut fact = Fact::new(
            FactKind::Run,
            self.id,
            self.started_utc_ms,
            self.ended_utc_ms,
            self.confirmed_utc_ms,
        )?;
        fact.run_id = self.id;
        fact.end_reason = self.end_reason.map(FactEndReason::Run);
        fact.details = Some(FactDetails::Run {
            device_id: self.device_id,
            win_session_id: self.win_session_id,
            app_version: self.app_version,
        });
        Ok(fact)
    }
}
impl ClockSegmentsRow {
    pub(in crate::store) fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let mut fact = Fact::new(
            FactKind::ClockSegment,
            self.id,
            self.start_utc_ms,
            self.end_utc_ms,
            self.confirmed_utc_ms,
        )?;
        fact.run_id = Some(self.run_id);
        fact.segment_id = self.id;
        fact.start_mono_ms = Some(self.start_mono_ms);
        fact.end_mono_ms = self.end_mono_ms;
        fact.confirmed_mono_ms = self.confirmed_mono_ms;
        fact.duration_ms = Some(fact.monotonic_duration()?);
        fact.end_reason = self.end_reason.map(FactEndReason::Clock);
        Ok(fact)
    }
}
impl TimedPoliciesRow {
    pub(in crate::store) fn into_fact(self) -> Result<Fact, ReadFactsError> {
        let mut fact = Fact::new(
            FactKind::TimedPolicy,
            Some(i64::from(self.kind.code())),
            self.started_utc_ms,
            Some(self.until_utc_ms),
            None,
        )?;
        fact.policy_kind = Some(self.kind);
        fact.privacy_mode = self.privacy_mode;
        fact.details = Some(FactDetails::TimedPolicy {
            application_id: self.application_id,
        });
        Ok(fact)
    }
}
impl Serialize for FactKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl Serialize for FactEnd {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields = serializer.serialize_struct("FactEnd", 2)?;
        fields.serialize_field("end_utc_ms", &self.closed())?;
        fields.serialize_field("confirmed_utc_ms", &self.confirmed())?;
        fields.end()
    }
}

impl Serialize for FactEndReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl Serialize for FactReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

#[derive(Serialize)]
#[serde(remote = "Fact")]
struct SerializedFact {
    pub kind: FactKind,
    pub id: i64,
    pub run_id: Option<i64>,
    pub segment_id: Option<i64>,
    pub start_utc_ms: i64,
    #[serde(flatten)]
    pub end: FactEnd,
    pub start_mono_ms: Option<i64>,
    pub end_mono_ms: Option<i64>,
    pub confirmed_mono_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub end_reason: Option<FactEndReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<FactReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_foreground_reason: Option<enums::NoForegroundReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_level: Option<enums::CaptureLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_status: Option<enums::TitleStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<enums::IdleException>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_kind: Option<enums::TimedPolicyKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacy_mode: Option<enums::PrivacyMode>,
}

impl Serialize for Fact {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct DumpFact<'fact> {
            #[serde(flatten, with = "SerializedFact")]
            fact: &'fact Fact,
            #[serde(flatten)]
            fields: BTreeMap<&'static str, Value>,
        }
        let mut fields = BTreeMap::new();
        match &self.details {
            Some(FactDetails::Interval {
                sequence,
                session_key,
                app_id,
                window_instance_id,
                privacy_rule_id,
                privacy_label_key,
                privacy_label,
            }) => {
                fields.insert("sequence", json!(sequence));
                fields.insert("session_key", json!(session_key));
                fields.insert("app_id", json!(app_id));
                fields.insert("window_instance_id", json!(window_instance_id));
                fields.insert("privacy_rule_id", json!(privacy_rule_id));
                fields.insert("privacy_label_key", json!(privacy_label_key));
                fields.insert("privacy_label", json!(privacy_label));
            }
            Some(FactDetails::Availability {
                last_input_utc_ms,
                detected_utc_ms,
                threshold_ms,
                policy_version,
            }) => {
                fields.insert("last_input_utc_ms", json!(last_input_utc_ms));
                fields.insert("detected_utc_ms", json!(detected_utc_ms));
                fields.insert("threshold_ms", json!(threshold_ms));
                fields.insert("policy_version", json!(policy_version));
            }
            Some(FactDetails::Run {
                device_id,
                win_session_id,
                app_version,
            }) => {
                fields.insert("device_id", json!(device_id));
                fields.insert("win_session_id", json!(win_session_id));
                fields.insert("app_version", json!(app_version));
            }
            Some(FactDetails::TimedPolicy { application_id }) => {
                fields.insert("application_id", json!(application_id));
            }
            None => {}
        }
        if let Some(window) = &self.window {
            fields.insert("app_display_name", json!(window.display_name));
            fields.insert("exe", json!(window.exe_path));
            fields.insert("exe_name", json!(window.exe_name));
            fields.insert("hwnd", json!(window.hwnd));
            fields.insert("window_generation", json!(window.generation));
            fields.insert("pid", json!(window.pid));
            fields.insert("window_class", json!(window.window_class));
            if let Some(title) = &window.title {
                fields.insert("title", json!(title));
            }
        }
        DumpFact { fact: self, fields }.serialize(serializer)
    }
}
