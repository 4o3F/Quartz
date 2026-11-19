//! Diagnostic identifiers constructed from the generated Diesel schema.

use super::schema;
use diesel::{Column, Table};
use std::fmt;

macro_rules! table_names {
    ($($variant:ident => $table:ident),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum TableName { $($variant),+ }

        impl fmt::Display for TableName {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self { $(Self::$variant => stringify!($table)),+ })
            }
        }

        $(impl NamedTable for schema::$table::table {
            const NAME: TableName = TableName::$variant;
        })+
    };
}

pub(crate) trait NamedTable: Table + Copy {
    const NAME: TableName;
}

table_names! {
    Applications => applications,
    AvailabilityIntervals => availability_intervals,
    CaptureGaps => capture_gaps,
    CaptureRuns => capture_runs,
    ClockSegments => clock_segments,
    DataDayVersions => data_day_versions,
    Intervals => intervals,
    Settings => settings,
    TimedPolicies => timed_policies,
    Titles => titles,
    WindowInstances => window_instances,
    WebIntervals => web_intervals,
    IngestReceipts => ingest_receipts,
    Categories => categories,
    RuleSets => rule_sets,
    Annotations => annotations,
}

impl TableName {
    pub(crate) const fn of<T: NamedTable>(_: T) -> Self {
        T::NAME
    }
}

pub(crate) trait SchemaColumn: Column + Copy {
    const TABLE: TableName;
}
impl<C: Column + Copy> SchemaColumn for C
where
    C::Table: NamedTable,
{
    const TABLE: TableName = <C::Table as NamedTable>::NAME;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColumnName(&'static str);
impl ColumnName {
    pub(crate) const fn of<C: SchemaColumn>(_: C) -> Self {
        Self(C::NAME)
    }
}
impl fmt::Display for ColumnName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnumName {
    Column {
        table: TableName,
        column: ColumnName,
    },
    SharedColumn(ColumnName),
}
impl EnumName {
    pub(super) const fn column<C: SchemaColumn>(column: C) -> Self {
        Self::Column {
            table: C::TABLE,
            column: ColumnName::of(column),
        }
    }
}
impl fmt::Display for EnumName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Column { table, column } => write!(f, "{table}.{column}"),
            Self::SharedColumn(column) => column.fmt(f),
        }
    }
}
