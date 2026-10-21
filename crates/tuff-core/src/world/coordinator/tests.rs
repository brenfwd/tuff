use super::*;

#[tuff_core::watchdog]
#[test]
fn late_ticks_keep_the_schedule_until_far_behind() {
    let period = std::time::Duration::from_millis(50);
    let due = tokio::time::Instant::now();
    let late = due + std::time::Duration::from_millis(7);
    assert_eq!(next_due(due, late, period), due + period);
    let stalled = due + std::time::Duration::from_secs(3);
    assert_eq!(next_due(due, stalled, period), stalled + period);
}

#[tuff_core::watchdog]
#[test]
fn sleeping_needs_the_rules_share_of_players_and_at_least_one() {
    assert_eq!(sleepers_needed(0, 100), 1);
    assert_eq!(sleepers_needed(3, 100), 3);
    assert_eq!(sleepers_needed(3, 50), 2);
    assert_eq!(sleepers_needed(4, 0), 1);
    assert_eq!(sleepers_needed(10, 101), 11);
}

#[tuff_core::watchdog]
#[test]
fn overload_warns_past_twenty_ticks_behind_and_then_stays_quiet_for_ten_seconds() {
    use std::time::Duration;
    let period = Duration::from_millis(50);
    let due = tokio::time::Instant::now();
    let late = |by: u64| due + Duration::from_millis(by);
    assert_eq!(overloaded_by(due, late(2000), period, None), None);
    assert_eq!(overloaded_by(due, late(2500), period, None), Some(50));
    assert_eq!(overloaded_by(due, late(2500), period, Some(due)), None);
    let later = due + Duration::from_secs(16);
    assert_eq!(
        overloaded_by(
            later,
            later + Duration::from_millis(2500),
            period,
            Some(due)
        ),
        Some(50)
    );
}
