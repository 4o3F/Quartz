use super::*;
use quartz_i18n::Language;
use strum::VariantArray;

#[test]
fn every_failure_kind_has_the_approved_severity_and_actions() {
    struct Case {
        kind: FailureKind,
        severity: Severity,
        actions: &'static [FailureAction],
    }
    use FailureAction::{CopyDetails, Exit, RetryConnection, RetryStartCore, StartCore};
    use FailureKind::*;
    use Severity::{Recoverable, Unrecoverable};
    let cases = [
        Case {
            kind: CoreNotRunning,
            severity: Recoverable,
            actions: &[StartCore, RetryConnection],
        },
        Case {
            kind: PipeBusy,
            severity: Recoverable,
            actions: &[RetryConnection],
        },
        Case {
            kind: ConnectTimeout,
            severity: Recoverable,
            actions: &[RetryConnection],
        },
        Case {
            kind: ConnectIo,
            severity: Recoverable,
            actions: &[RetryConnection],
        },
        Case {
            kind: CoreStartFailed,
            severity: Recoverable,
            actions: &[RetryStartCore, Exit],
        },
        Case {
            kind: NewerDatabase,
            severity: Unrecoverable,
            actions: &[CopyDetails, Exit],
        },
        Case {
            kind: MigrationFailed,
            severity: Unrecoverable,
            actions: &[CopyDetails, Exit],
        },
        Case {
            kind: OtherSession,
            severity: Unrecoverable,
            actions: &[CopyDetails, Exit],
        },
        Case {
            kind: VersionMismatch,
            severity: Unrecoverable,
            actions: &[CopyDetails, Exit],
        },
        Case {
            kind: IdentityRejected,
            severity: Unrecoverable,
            actions: &[CopyDetails, Exit],
        },
        Case {
            kind: HandshakeRejected,
            severity: Unrecoverable,
            actions: &[CopyDetails, Exit],
        },
        Case {
            kind: InvalidResponse,
            severity: Unrecoverable,
            actions: &[CopyDetails, Exit],
        },
        Case {
            kind: InterfaceUnavailable,
            severity: Unrecoverable,
            actions: &[CopyDetails, Exit],
        },
    ];
    assert_eq!(
        cases.len(),
        FailureKind::VARIANTS.len(),
        "enumerate every failure kind"
    );
    for kind in FailureKind::VARIANTS {
        let case = cases
            .iter()
            .find(|case| case.kind == *kind)
            .expect("classification case");
        let failure = Failure::new(*kind);
        assert_eq!(failure.severity(), case.severity, "{kind:?}");
        assert_eq!(failure.actions(), case.actions, "{kind:?}");
        for language in [Language::SimplifiedChinese, Language::English] {
            let formatter = Formatter::new(language);
            let presentation = FailurePresentation::new(&failure, formatter);
            assert!(!presentation.title.is_empty());
            assert!(!presentation.message.is_empty());
            assert!(!presentation.copy_text().contains("ui.failure."));
            for action in presentation.actions {
                assert!(!action.label(formatter).contains("ui.failure."));
            }
        }
    }
}

#[test]
fn startup_codes_use_the_same_failure_model_and_keep_exact_backup_details() {
    for code in CoreErrorCode::VARIANTS {
        let failure = Failure::from_startup(CoreError::new(*code, vec![]));
        let expected = match code {
            CoreErrorCode::CoreStartFailed => FailureKind::CoreStartFailed,
            CoreErrorCode::NewerDatabase => FailureKind::NewerDatabase,
            CoreErrorCode::MigrationFailed => FailureKind::MigrationFailed,
            CoreErrorCode::OtherSession => FailureKind::OtherSession,
        };
        assert_eq!(failure.kind(), expected);
    }
    let backup = PathBuf::from("C:/数据/backup with spaces.db");
    let failure = Failure::from_startup(CoreError::new(
        CoreErrorCode::MigrationFailed,
        vec![backup.clone()],
    ));
    for language in [Language::SimplifiedChinese, Language::English] {
        let presentation = FailurePresentation::new(&failure, Formatter::new(language));
        assert!(presentation.details.contains(&DetailLine::Value {
            label: Formatter::new(language).text(Msg::CoreErrorBackups),
            value: backup.display().to_string()
        }));
        assert!(presentation.details.contains(&DetailLine::Command(
            "quartz restore-backup --yes \"C:/数据/backup with spaces.db\"".into()
        )));
        let copied = presentation.copy_text();
        assert!(copied.contains("C:/数据/backup with spaces.db"));
        assert!(copied.contains("quartz restore-backup"));
    }
    let unsafe_path = Failure::from_startup(CoreError::new(
        CoreErrorCode::MigrationFailed,
        vec![PathBuf::from("C:/a\"b.db")],
    ));
    assert!(
        !FailurePresentation::new(&unsafe_path, Formatter::new(Language::English))
            .details
            .iter()
            .any(|line| matches!(line, DetailLine::Command(_)))
    );
}

#[test]
fn offline_and_version_mismatch_have_exact_text_in_both_languages() {
    for language in [Language::SimplifiedChinese, Language::English] {
        let formatter = Formatter::new(language);
        let offline =
            FailurePresentation::new(&Failure::new(FailureKind::CoreNotRunning), formatter);
        let expected = match language {
            Language::SimplifiedChinese => "Quartz 核心未运行",
            Language::English => "Quartz core is not running",
            #[cfg(debug_assertions)]
            Language::PseudoEnglish => unreachable!("production languages only"),
        };
        assert_eq!(offline.title, expected);
        let mismatch = Failure::from_connection(&CallCoreError::Protocol {
            kind: ErrorKind::ProtoMismatch {
                supported: ProtoRange::new(
                    quartz_protocol::ProtoVersion(2),
                    quartz_protocol::ProtoVersion(3),
                )
                .expect("range"),
                core_version: CoreVersion("2.0".into()),
            },
        });
        let presentation = FailurePresentation::new(&mismatch, formatter);
        let expected = match language {
            Language::SimplifiedChinese => "核心与界面版本不一致",
            Language::English => "Core and window versions don’t match",
            #[cfg(debug_assertions)]
            Language::PseudoEnglish => unreachable!("production languages only"),
        };
        assert_eq!(presentation.title, expected);
        assert!(presentation.copy_text().contains("2.0"));
        assert!(presentation.copy_text().contains("2–3"));
    }
}

#[cfg(debug_assertions)]
#[test]
fn pseudo_locale_uses_the_same_screen_model() {
    for kind in FailureKind::VARIANTS {
        let formatter = Formatter::new(Language::PseudoEnglish);
        let presentation = FailurePresentation::new(&Failure::new(*kind), formatter);
        assert!(presentation.title.starts_with('['));
        assert!(presentation.message.starts_with('['));
        for action in presentation.actions {
            assert!(action.label(formatter).starts_with('['));
        }
    }
}

#[test]
fn expansion_and_copy_placement_cover_every_kind() {
    use DetailsExpansion::{Collapsed, Expanded};
    use FailureKind::*;
    struct ExpansionCase {
        kind: FailureKind,
        expansion: DetailsExpansion,
    }
    let cases = [
        ExpansionCase {
            kind: CoreNotRunning,
            expansion: Collapsed,
        },
        ExpansionCase {
            kind: PipeBusy,
            expansion: Collapsed,
        },
        ExpansionCase {
            kind: ConnectTimeout,
            expansion: Collapsed,
        },
        ExpansionCase {
            kind: ConnectIo,
            expansion: Collapsed,
        },
        ExpansionCase {
            kind: CoreStartFailed,
            expansion: Expanded,
        },
        ExpansionCase {
            kind: MigrationFailed,
            expansion: Expanded,
        },
        ExpansionCase {
            kind: NewerDatabase,
            expansion: Expanded,
        },
        ExpansionCase {
            kind: VersionMismatch,
            expansion: Expanded,
        },
        ExpansionCase {
            kind: OtherSession,
            expansion: Collapsed,
        },
        ExpansionCase {
            kind: IdentityRejected,
            expansion: Collapsed,
        },
        ExpansionCase {
            kind: HandshakeRejected,
            expansion: Collapsed,
        },
        ExpansionCase {
            kind: InvalidResponse,
            expansion: Collapsed,
        },
        ExpansionCase {
            kind: InterfaceUnavailable,
            expansion: Collapsed,
        },
    ];
    assert_eq!(cases.len(), FailureKind::VARIANTS.len());
    for &kind in FailureKind::VARIANTS {
        let expected = cases
            .iter()
            .find(|case| case.kind == kind)
            .expect("every kind");
        assert_eq!(kind.details_expansion(), expected.expansion, "{kind:?}");
        let main_copies = kind
            .actions()
            .iter()
            .filter(|action| **action == FailureAction::CopyDetails)
            .count();
        match kind.copy_placement() {
            CopyPlacement::PrimaryAction => {
                assert_eq!(main_copies, 1);
                assert_eq!(kind.actions().first(), Some(&FailureAction::CopyDetails));
            }
            CopyPlacement::DetailsFooter => assert_eq!(main_copies, 0),
        }
    }
}

#[test]
fn copied_diagnostics_contain_the_message_code_values_and_exact_command_without_titles() {
    let path = PathBuf::from(r"C:\Users\someone\a very long directory\升级前备份.db");
    let failure = Failure::from_startup(CoreError::new(CoreErrorCode::MigrationFailed, vec![path]));
    for &language in Language::ALL {
        let mut presentation = FailurePresentation::new(&failure, Formatter::new(language));
        presentation.title = "DO NOT COPY A WINDOW TITLE".into();
        let text = presentation.copy_text();
        assert!(text.starts_with(&presentation.message));
        assert!(text.contains("migration_failed"));
        assert!(text.contains(r"C:\Users\someone\a very long directory\升级前备份.db"));
        assert!(text.contains(
            r#"quartz restore-backup --yes "C:\Users\someone\a very long directory\升级前备份.db""#
        ));
        assert!(!text.contains("DO NOT COPY A WINDOW TITLE"));
        assert!(!text.contains("<br>"));
    }
}
