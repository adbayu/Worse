use super::*;

#[test]
fn interval_adds_seconds() {
    assert_eq!(
        next_run(&ScheduleKind::Interval { seconds: 5 }, 1_000),
        Some(6_000)
    );
}

#[test]
fn daily_is_in_the_future_on_an_allowed_weekday() {
    let now = now_ms();
    let kind = ScheduleKind::Daily {
        minute_of_day: 9 * 60,
        weekdays: vec![3],
    };
    let next = next_run(&kind, now);
    assert!(next.is_some_and(|n| n > now && n - now <= 8 * 86_400_000));
    let weekday = next
        .and_then(|n| Local.timestamp_millis_opt(n).single())
        .map(|t| t.weekday().number_from_monday());
    assert_eq!(weekday, Some(3));
}
