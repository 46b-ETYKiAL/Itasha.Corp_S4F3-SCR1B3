//! The once-per-launch automatic-update mode→action mapping. `off`/`manual`
//! never do automatic network activity; `notify`/`auto` run a check when due.
use super::update_launch_action;
use crate::updater::LaunchKind;
use scribe_core::config::UpdateMode;

const HOUR: u64 = 3_600;

#[test]
fn off_never_checks_even_when_overdue() {
    assert_eq!(
        update_launch_action(UpdateMode::Off, None, 24, 1_000_000),
        None
    );
}

#[test]
fn manual_never_checks_automatically() {
    // `manual` = the user checks via the Settings button; no on-launch network.
    assert_eq!(update_launch_action(UpdateMode::Manual, None, 24, 0), None);
    assert_eq!(
        update_launch_action(UpdateMode::Manual, None, 24, 1_000_000),
        None
    );
}

#[test]
fn notify_checks_on_every_launch_regardless_of_interval_or_manual_check() {
    // Regression for "relaunch shows no update notification": Notify must
    // check on EVERY launch — the interval throttle and a recent manual
    // "Check for updates" (which stamps `last_check_unix`) must NOT suppress
    // it. Notify is a passive, dismissible banner + one light API GET.
    let last = 1_000;
    // 1 minute after a check (e.g. a manual press) → STILL checks.
    assert_eq!(
        update_launch_action(UpdateMode::Notify, Some(last), 24, last + 60),
        Some(LaunchKind::Notify),
    );
    // 1h after → STILL checks (old behaviour suppressed this).
    assert_eq!(
        update_launch_action(UpdateMode::Notify, Some(last), 24, last + HOUR),
        Some(LaunchKind::Notify),
    );
    // Never checked → checks.
    assert_eq!(
        update_launch_action(UpdateMode::Notify, None, 24, 0),
        Some(LaunchKind::Notify),
    );
    // Even a 0-hour interval is irrelevant to Notify.
    assert_eq!(
        update_launch_action(UpdateMode::Notify, Some(last), 0, last),
        Some(LaunchKind::Notify),
    );
}

#[test]
fn auto_checks_when_due_as_auto_kind_and_respects_interval() {
    assert_eq!(
        update_launch_action(UpdateMode::Auto, None, 24, 0),
        Some(LaunchKind::Auto),
    );
    // Auto still respects the interval (not-due → no check).
    let last = 1_000;
    assert_eq!(
        update_launch_action(UpdateMode::Auto, Some(last), 24, last + HOUR),
        None,
    );
}
