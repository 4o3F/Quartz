//! Version-bound day loading on the pipe runtime; GPUI only sends intents and receives results.
use super::{BRIDGE_CAPACITY, Link, LinkEvent, RequestCoreError};
use quartz_pipe::CallCoreError;
use quartz_protocol::{
    ApplicationRank, DayMetrics, ErrorKind, HourStack, LocalDate, MillisecondsPerPixel,
    PendingAway, QueryDayRequest, QueryPendingAwayRequest, QueryTimelineRequest, QueryVersions,
    TimelineDetail, TimelineSegment, Topic,
};
use std::{future::Future, num::NonZeroU32, pin::Pin, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

/// Entire query sets may restart twice after version invalidation.
pub const MAX_STALE_RESTARTS: usize = 2;
/// A settled timeline view waits this long before querying again.
pub const ZOOM_SETTLE_DELAY: Duration = Duration::from_millis(180);
/// Fast refreshes leave the date bar unchanged.
pub const REFRESH_INDICATOR_DELAY: Duration = Duration::from_millis(300);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct DayGeneration(u64);
impl DayGeneration {
    pub fn accepts(self, generation: Self) -> bool {
        self == generation
    }
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcRange {
    pub start: i64,
    pub end: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayView {
    WholeDay,
    Range(UtcRange),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayRequest {
    pub date: LocalDate,
    pub view: DayView,
    pub physical_width: NonZeroU32,
}
impl DayRequest {
    pub fn ms_per_pixel(&self, range: UtcRange) -> Result<MillisecondsPerPixel, DayFailure> {
        let span = u64::try_from(
            range
                .end
                .checked_sub(range.start)
                .ok_or(DayFailure::InvalidData)?,
        )
        .map_err(|_| DayFailure::InvalidData)?;
        let value = span.div_ceil(u64::from(self.physical_width.get()));
        Ok(MillisecondsPerPixel(
            std::num::NonZeroU64::new(value).ok_or(DayFailure::InvalidData)?,
        ))
    }
    pub fn physical_width(logical: f32, scale: f32) -> Result<NonZeroU32, DayFailure> {
        use num_traits::ToPrimitive as _;
        if !logical.is_finite() || !scale.is_finite() || logical <= 0. || scale <= 0. {
            return Err(DayFailure::InvalidData);
        }
        (logical * scale)
            .round()
            .to_u32()
            .and_then(NonZeroU32::new)
            .ok_or(DayFailure::InvalidData)
    }
}
#[derive(Debug)]
pub enum DayFailure {
    Request { source: RequestCoreError },
    InvalidData,
    StaleExhausted,
}
/// Local failures have a separate namespace from core protocol errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFailureCode {
    Timeout,
    Connection,
    InvalidResponse,
    StaleExhausted,
}
impl LocalFailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection_error",
            Self::InvalidResponse => "invalid_response",
            Self::StaleExhausted => "stale_exhausted",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DayFailureCode {
    Protocol(ErrorKind),
    Local(LocalFailureCode),
}
impl DayFailureCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Protocol(kind) => kind.code(),
            Self::Local(code) => code.as_str(),
        }
    }
}
impl std::fmt::Display for DayFailureCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}
impl DayFailure {
    pub fn is_busy(&self) -> bool {
        matches!(self.code(), DayFailureCode::Protocol(ErrorKind::Busy))
    }
    pub fn code(&self) -> DayFailureCode {
        match self {
            Self::Request {
                source:
                    RequestCoreError::Call {
                        source: CallCoreError::Protocol { kind },
                    },
            } => DayFailureCode::Protocol(kind.clone()),
            Self::Request {
                source: RequestCoreError::Busy,
            } => DayFailureCode::Protocol(ErrorKind::Busy),
            Self::Request {
                source: RequestCoreError::Timeout,
            } => DayFailureCode::Local(LocalFailureCode::Timeout),
            Self::Request { .. } => DayFailureCode::Local(LocalFailureCode::Connection),
            Self::InvalidData => DayFailureCode::Local(LocalFailureCode::InvalidResponse),
            Self::StaleExhausted => DayFailureCode::Local(LocalFailureCode::StaleExhausted),
        }
    }
    pub(super) fn is_stale(&self) -> bool {
        matches!(
            self,
            Self::Request {
                source: RequestCoreError::Call {
                    source: CallCoreError::Protocol {
                        kind: ErrorKind::StaleCursor
                    }
                }
            }
        )
    }
}
impl From<RequestCoreError> for DayFailure {
    fn from(source: RequestCoreError) -> Self {
        Self::Request { source }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct DayData {
    pub date: LocalDate,
    pub bounds: UtcRange,
    pub visible: UtcRange,
    pub metrics: DayMetrics,
    pub hours: Vec<HourStack>,
    pub applications: Vec<ApplicationRank>,
    pub categories: Vec<TimelineSegment>,
    pub raw: Vec<TimelineSegment>,
    pub pending: Vec<PendingAway>,
}
struct TimelinePages {
    segments: Vec<TimelineSegment>,
    versions: QueryVersions,
}
impl Link {
    async fn reconcile_day_subscription(
        &self,
        subscribed: &mut Option<LocalDate>,
        target: LocalDate,
    ) -> Result<(), RequestCoreError> {
        if *subscribed == Some(target) {
            return Ok(());
        }
        if let Some(previous) = *subscribed {
            self.unsubscribe(vec![Topic::Day(previous)]).await?;
            *subscribed = None;
        }
        self.subscribe(vec![Topic::Day(target)]).await?;
        *subscribed = Some(target);
        Ok(())
    }
    async fn query_day_once(&self, request: DayRequest) -> Result<DayData, DayFailure> {
        let mut query = QueryDayRequest {
            local_date: request.date,
            cursor: None,
        };
        let first = self.request(query.clone()).await?;
        let reference_range = first.reference_range;
        let metrics = first.metrics.ok_or(DayFailure::InvalidData)?;
        let hours = first.hours.ok_or(DayFailure::InvalidData)?;
        let bounds = UtcRange {
            start: hours.first().ok_or(DayFailure::InvalidData)?.start_utc_ms,
            end: hours.last().ok_or(DayFailure::InvalidData)?.end_utc_ms,
        };
        let mut applications = first.applications;
        let version = first.versions;
        query.cursor = first.next_cursor;
        while query.cursor.is_some() {
            let page = self.request(query.clone()).await?;
            if page.versions != version {
                return Err(Self::stale_day());
            }
            if page.next_cursor == query.cursor {
                return Err(DayFailure::InvalidData);
            }
            applications.extend(page.applications);
            query.cursor = page.next_cursor;
        }
        let visible = match request.view {
            DayView::WholeDay => bounds,
            DayView::Range(range) => range,
        };
        if visible.start < bounds.start || visible.end > bounds.end {
            return Err(DayFailure::InvalidData);
        }
        let resolution = request.ms_per_pixel(visible)?;
        let categories = self
            .timeline_pages(visible, TimelineDetail::Category, resolution)
            .await?;
        let raw = self
            .timeline_pages(visible, TimelineDetail::Raw, resolution)
            .await?;
        if categories.versions != raw.versions {
            return Err(Self::stale_day());
        }
        let mut pending = Vec::new();
        let mut query = QueryPendingAwayRequest {
            local_date: request.date,
            cursor: None,
        };
        let mut versions = None;
        loop {
            let page = self.request(query.clone()).await?;
            if versions
                .as_ref()
                .is_some_and(|version| *version != page.versions)
            {
                return Err(Self::stale_day());
            }
            if (reference_range.is_none() && page.versions != version)
                || (visible == bounds && page.versions != categories.versions)
            {
                return Err(Self::stale_day());
            }
            versions = Some(page.versions);
            pending.extend(page.ranges);
            if page.next_cursor.is_none() {
                break;
            }
            if page.next_cursor == query.cursor {
                return Err(DayFailure::InvalidData);
            }
            query.cursor = page.next_cursor;
        }
        if reference_range.is_some() {
            let latest = self
                .request(QueryDayRequest {
                    local_date: request.date,
                    cursor: None,
                })
                .await?;
            if latest.versions != version {
                return Err(Self::stale_day());
            }
        }
        Ok(DayData {
            date: request.date,
            bounds,
            visible,
            metrics,
            hours,
            applications,
            categories: categories.segments,
            raw: raw.segments,
            pending,
        })
    }
    pub(super) fn stale_day() -> DayFailure {
        DayFailure::from(RequestCoreError::Call {
            source: CallCoreError::Protocol {
                kind: ErrorKind::StaleCursor,
            },
        })
    }
    async fn timeline_pages(
        &self,
        range: UtcRange,
        detail: TimelineDetail,
        ms_per_pixel: MillisecondsPerPixel,
    ) -> Result<TimelinePages, DayFailure> {
        let mut query = QueryTimelineRequest {
            start_utc_ms: range.start,
            end_utc_ms: range.end,
            detail,
            ms_per_pixel,
            cursor: None,
        };
        let mut segments = Vec::new();
        let mut versions = None;
        loop {
            let page = self.request(query.clone()).await?;
            if versions
                .as_ref()
                .is_some_and(|version| *version != page.versions)
            {
                return Err(Self::stale_day());
            }
            versions = Some(page.versions);
            segments.extend(page.segments);
            if page.next_cursor.is_none() {
                break;
            }
            if page.next_cursor == query.cursor {
                return Err(DayFailure::InvalidData);
            }
            query.cursor = page.next_cursor;
        }
        Ok(TimelinePages {
            segments,
            versions: versions.ok_or(DayFailure::InvalidData)?,
        })
    }
    pub(super) async fn query_day(&self, request: DayRequest) -> Result<DayData, DayFailure> {
        for attempt in 0..=MAX_STALE_RESTARTS {
            match self.query_day_once(request).await {
                Err(error) if error.is_stale() && attempt < MAX_STALE_RESTARTS => continue,
                Err(error) if error.is_stale() => return Err(DayFailure::StaleExhausted),
                result => return result,
            }
        }
        Err(DayFailure::StaleExhausted)
    }
}
#[derive(Debug, Clone, Copy)]
pub enum DayLoad {
    Immediate,
    Settled,
}
pub(super) enum DayCommand {
    Load {
        request: DayRequest,
        generation: DayGeneration,
        mode: DayLoad,
    },
}
pub(super) struct DayLoader {
    sender: mpsc::Sender<DayCommand>,
    changed: watch::Sender<Option<LocalDate>>,
    date: Option<LocalDate>,
}
impl DayLoader {
    pub fn start(link: Link, events: mpsc::Sender<LinkEvent>) -> Self {
        let (sender, receiver) = mpsc::channel(BRIDGE_CAPACITY);
        let (changed, changes) = watch::channel(None);
        tokio::spawn(Self::run(link, events, receiver, changes));
        Self {
            sender,
            changed,
            date: None,
        }
    }
    pub fn send(&mut self, command: DayCommand) -> Result<(), RequestCoreError> {
        let DayCommand::Load { request, .. } = &command;
        let date = request.date;
        // The pipe reader must keep servicing responses while subscription changes await them.
        self.sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => RequestCoreError::Busy,
            mpsc::error::TrySendError::Closed(_) => RequestCoreError::Closed,
        })?;
        self.date = Some(date);
        Ok(())
    }
    pub fn changed(&self, date: LocalDate) {
        if self.date == Some(date) {
            // A watch value retains one invalidation regardless of the event burst size.
            self.changed.send_replace(Some(date));
        }
    }
    async fn run(
        link: Link,
        events: mpsc::Sender<LinkEvent>,
        mut commands: mpsc::Receiver<DayCommand>,
        mut changes: watch::Receiver<Option<LocalDate>>,
    ) {
        type LoadFuture = Pin<Box<dyn Future<Output = Result<DayData, DayFailure>> + Send>>;
        let mut active: Option<LoadFuture> = None;
        let mut latest: Option<DayRequest> = None;
        let mut subscribed: Option<LocalDate> = None;
        let mut generation = DayGeneration::default();
        let mut loading_generation = generation;
        let mut queued = None;
        let mut deadline: Option<Instant> = None;
        loop {
            tokio::select! {
                command = commands.recv() => {
                    match command {
                        None => break,
                        Some(DayCommand::Load { request, generation: next, mode }) => {
                            if next < generation {
                                continue;
                            }
                            if next > generation {
                                // Dropping the request future detaches its reply receiver; the pipe
                                // worker still consumes late responses by their request identities.
                                active = None;
                                queued = None;
                                deadline = None;
                            }
                            generation = next;
                            latest = Some(request);
                            if let Err(source) = link.reconcile_day_subscription(&mut subscribed, request.date).await {
                                if events.send(LinkEvent::DayLoaded {
                                    generation: next,
                                    result: Err(source.into()),
                                }).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                            queued = Some(request);
                            deadline = Some(Instant::now() + match mode {
                                DayLoad::Immediate => Duration::ZERO,
                                DayLoad::Settled => ZOOM_SETTLE_DELAY,
                            });
                        }
                    }
                }
                changed = changes.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let date = *changes.borrow_and_update();
                    if let Some(request) = latest.filter(|request| Some(request.date) == date && subscribed == date) {
                        queued = Some(request);
                        if active.is_none() {
                            deadline = Some(Instant::now());
                        }
                    }
                }
                result = async {
                    match active.as_mut() {
                        Some(future) => future.await,
                        None => std::future::pending().await,
                    }
                } => {
                    active = None;
                    if events.send(LinkEvent::DayLoaded {
                        generation: loading_generation,
                        result,
                    }).await.is_err() {
                        break;
                    }
                    if queued.is_some() && deadline.is_none() {
                        deadline = Some(Instant::now());
                    }
                }
                () = async {
                    match deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                }, if active.is_none() => {
                    deadline = None;
                    if let Some(request) = queued.take() {
                        loading_generation = generation;
                        if events.send(LinkEvent::DayLoading { generation }).await.is_err() {
                            break;
                        }
                        let link = link.clone();
                        active = Some(Box::pin(async move { link.query_day(request).await }));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_codes_preserve_protocol_kinds_and_type_local_failures() {
        struct Case {
            failure: DayFailure,
            expected: DayFailureCode,
            shown: &'static str,
        }
        for case in [
            Case {
                failure: DayFailure::Request {
                    source: RequestCoreError::Timeout,
                },
                expected: DayFailureCode::Local(LocalFailureCode::Timeout),
                shown: "timeout",
            },
            Case {
                failure: DayFailure::Request {
                    source: RequestCoreError::Disconnected,
                },
                expected: DayFailureCode::Local(LocalFailureCode::Connection),
                shown: "connection_error",
            },
            Case {
                failure: DayFailure::InvalidData,
                expected: DayFailureCode::Local(LocalFailureCode::InvalidResponse),
                shown: "invalid_response",
            },
            Case {
                failure: DayFailure::StaleExhausted,
                expected: DayFailureCode::Local(LocalFailureCode::StaleExhausted),
                shown: "stale_exhausted",
            },
            Case {
                failure: DayFailure::Request {
                    source: RequestCoreError::Busy,
                },
                expected: DayFailureCode::Protocol(ErrorKind::Busy),
                shown: "busy",
            },
            Case {
                failure: DayFailure::from(RequestCoreError::Call {
                    source: CallCoreError::Protocol {
                        kind: ErrorKind::InvalidRange,
                    },
                }),
                expected: DayFailureCode::Protocol(ErrorKind::InvalidRange),
                shown: "invalid_range",
            },
        ] {
            assert_eq!(case.failure.code(), case.expected);
            assert_eq!(case.failure.code().as_str(), case.shown);
            assert_eq!(case.failure.code().to_string(), case.shown);
        }
    }
    #[test]
    fn milliseconds_per_pixel_uses_physical_pixels_at_three_scales() {
        struct Case {
            scale: f32,
            width: u32,
            resolution: u64,
        }
        for case in [
            Case {
                scale: 1.,
                width: 1000,
                resolution: 86400,
            },
            Case {
                scale: 1.25,
                width: 1250,
                resolution: 69120,
            },
            Case {
                scale: 1.5,
                width: 1500,
                resolution: 57600,
            },
        ] {
            let width = DayRequest::physical_width(1000., case.scale).expect("physical width");
            assert_eq!(width.get(), case.width);
            let request = DayRequest {
                date: "2026-11-21".parse().expect("date"),
                view: DayView::WholeDay,
                physical_width: width,
            };
            assert_eq!(
                request
                    .ms_per_pixel(UtcRange {
                        start: 0,
                        end: 86_400_000
                    })
                    .expect("resolution")
                    .0
                    .get(),
                case.resolution
            );
        }
        assert!(DayRequest::physical_width(0., 1.).is_err());
        assert!(
            !DayGeneration::default()
                .next()
                .accepts(DayGeneration::default())
        );
    }
}
