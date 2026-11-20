//! Rankings by application, domain and document (07 section 3).

use std::{cmp::Reverse, collections::BTreeMap, time::Duration};

use super::{
    application::AppId,
    category::CategoryId,
    day::Day,
    details::DetailsSubject,
    segment::Segment,
    summary::{CategoryTotal, total},
};

/// Ranking tabs of 07 section 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum RankingKind {
    #[default]
    Apps,
    Domains,
    Documents,
    /// All three single rankings in one list.
    Mixed,
}

/// The rankings that group segments by one key; `Mixed` is their union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SingleRanking {
    Apps,
    Domains,
    Documents,
}

impl SingleRanking {
    const ALL: [Self; 3] = [Self::Apps, Self::Domains, Self::Documents];

    /// The group key of `segment`; `None` when the segment has no such key.
    fn key(self, segment: &Segment) -> Option<RankKey> {
        match self {
            Self::Apps => Some(RankKey::App(segment.app)),
            Self::Domains => segment.domain.clone().map(RankKey::Domain),
            Self::Documents => segment.document.clone().map(RankKey::Document),
        }
    }
}

/// What one ranking row groups by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RankKey {
    App(AppId),
    Domain(String),
    Document(String),
}

impl RankKey {
    /// What the details sidebar shows when the row is clicked; domains and documents have no
    /// details view until the protocol provides their history.
    #[must_use]
    pub(crate) const fn details_subject(&self) -> Option<DetailsSubject> {
        match self {
            Self::App(app) => Some(DetailsSubject::App(*app)),
            Self::Domain(_) | Self::Document(_) => None,
        }
    }
}

/// One ranking row with its per-category parts for the share bar.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RankRow {
    pub(crate) key: RankKey,
    /// Display text of the key.
    pub(crate) name: String,
    pub(crate) total: Duration,
    /// Category with the most time in this row; the last one wins a tie.
    dominant: CategoryId,
    /// Time per category in category order; never empty.
    pub(crate) parts: Vec<CategoryTotal>,
}

impl RankRow {
    /// The category with the most time; the last one wins a tie.
    #[must_use]
    pub(crate) const fn dominant_category(&self) -> CategoryId {
        self.dominant
    }
}

/// The rows of every ranking tab.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub(super) struct Rankings {
    apps: Vec<RankRow>,
    domains: Vec<RankRow>,
    documents: Vec<RankRow>,
    mixed: Vec<RankRow>,
}

impl Rankings {
    pub(super) fn get(&self, kind: RankingKind) -> &[RankRow] {
        match kind {
            RankingKind::Apps => &self.apps,
            RankingKind::Domains => &self.domains,
            RankingKind::Documents => &self.documents,
            RankingKind::Mixed => &self.mixed,
        }
    }
}

impl Day {
    pub(super) fn compute_rankings(&self) -> Rankings {
        Rankings {
            apps: self.rank(RankingKind::Apps),
            domains: self.rank(RankingKind::Domains),
            documents: self.rank(RankingKind::Documents),
            mixed: self.rank(RankingKind::Mixed),
        }
    }

    /// Ranking rows for a tab, longest first, ties by name.
    fn rank(&self, kind: RankingKind) -> Vec<RankRow> {
        let singles: &[SingleRanking] = match kind {
            RankingKind::Apps => &[SingleRanking::Apps],
            RankingKind::Domains => &[SingleRanking::Domains],
            RankingKind::Documents => &[SingleRanking::Documents],
            RankingKind::Mixed => &SingleRanking::ALL,
        };
        let mut rows: Vec<RankRow> = singles
            .iter()
            .flat_map(|single| self.rank_single(*single))
            .collect();
        rows.sort_by(|a, b| (Reverse(a.total), &a.name).cmp(&(Reverse(b.total), &b.name)));
        rows
    }

    fn rank_single(&self, single: SingleRanking) -> Vec<RankRow> {
        let mut groups: BTreeMap<RankKey, Vec<&Segment>> = BTreeMap::new();
        for segment in &self.segments {
            if let Some(key) = single.key(segment) {
                groups.entry(key).or_default().push(segment);
            }
        }
        groups
            .into_iter()
            .map(|(key, segments)| {
                let name = match &key {
                    RankKey::App(app) => self.app(*app).name.clone(),
                    RankKey::Domain(text) | RankKey::Document(text) => text.clone(),
                };
                let parts = self.totals_by_category(segments.iter().map(|segment| CategoryTotal {
                    category: segment.category,
                    total: segment.duration(),
                }));
                let dominant = parts
                    .iter()
                    .max_by_key(|part| part.total)
                    .expect("a ranking row has at least one non-empty segment")
                    .category;
                RankRow {
                    name,
                    total: total(segments.iter().map(|segment| segment.duration())),
                    dominant,
                    parts,
                    key,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        DayParts,
        fixture::{BROWSER, EDITOR, PLAY, WORK, day, parts, segment},
    };

    #[test]
    fn rows_are_longest_first_with_the_dominant_category() {
        let mut docs = segment(3, BROWSER, WORK, (10, 0), (10, 5));
        docs.domain = Some("docs.rs".to_owned());
        let mut video = segment(4, BROWSER, PLAY, (10, 5), (10, 45));
        video.domain = Some("video.example".to_owned());
        let day = day(
            vec![
                segment(1, EDITOR, WORK, (9, 0), (9, 30)),
                segment(2, BROWSER, WORK, (9, 30), (9, 40)),
                docs,
                video,
            ],
            vec![],
        );
        let apps = day.ranking(RankingKind::Apps);
        let keys: Vec<&RankKey> = apps.iter().map(|row| &row.key).collect();
        assert_eq!(keys, [&RankKey::App(BROWSER), &RankKey::App(EDITOR)]);
        let browser = apps.first().expect("browser row");
        assert_eq!(browser.dominant_category(), PLAY);
        assert_eq!(
            browser.key.details_subject(),
            Some(DetailsSubject::App(BROWSER))
        );
        let domains = day.ranking(RankingKind::Domains);
        assert_eq!(domains.len(), 2);
        assert!(
            domains
                .iter()
                .all(|row| row.key.details_subject().is_none())
        );
        assert_eq!(day.ranking(RankingKind::Mixed).len(), 4);
    }

    #[test]
    fn applications_with_the_same_name_stay_separate() {
        let mut parts = parts();
        for app in &mut parts.apps {
            app.name = "same".to_owned();
        }
        let day = Day::new(DayParts {
            segments: vec![
                segment(1, EDITOR, WORK, (9, 0), (9, 30)),
                segment(2, BROWSER, WORK, (9, 30), (10, 0)),
            ],
            ..parts
        })
        .expect("valid day");
        assert_eq!(day.ranking(RankingKind::Apps).len(), 2);
    }
}
