use super::IdleClock;

#[tuff_core::watchdog]
#[test]
fn pauses_after_the_empty_seconds_and_resumes_when_someone_joins() {
    let mut idle = IdleClock::default();
    for _ in 0..39 {
        idle.count(true);
    }
    assert!(!idle.paused(2));
    idle.count(true);
    assert!(idle.paused(2));
    idle.count(false);
    assert!(!idle.paused(2));
}

#[tuff_core::watchdog]
#[test]
fn zero_pauses_at_once_and_negative_never_pauses() {
    let mut idle = IdleClock::default();
    assert!(idle.paused(0));
    for _ in 0..100_000 {
        idle.count(true);
    }
    assert!(!idle.paused(-1));
}
