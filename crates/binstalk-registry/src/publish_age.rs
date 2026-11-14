//! Cargo's `min-publish-age` dependency cooldown.

use std::time::Duration;

use compact_str::CompactString;
use jiff::{SignedDuration, Timestamp};
use thiserror::Error as ThisError;

#[derive(Debug, ThisError)]
#[error(
    "invalid min-publish-age `{0}`, expected `0` or `N seconds/minutes/hours/days/weeks/months`"
)]
pub struct InvalidMinPublishAge(CompactString);

/// Parse a min-publish-age using the same syntax as Cargo's
/// `registry.global-min-publish-age`:
///
/// - `"0"` disables the cooldown and returns [`Duration::ZERO`]
/// - an integer followed by an optional space and one of `second(s)`,
///   `minute(s)`, `hour(s)`, `day(s)`, `week(s)` or `month(s)`,
///   where a month is 30.436875 days.
pub fn parse_min_publish_age(s: &str) -> Result<Duration, InvalidMinPublishAge> {
    if s == "0" {
        return Ok(Duration::ZERO);
    }

    let err = || InvalidMinPublishAge(s.into());

    let unit_start = s.find(|c: char| !c.is_ascii_digit()).ok_or_else(err)?;
    let (count, unit) = s.split_at(unit_start);
    let unit = unit.strip_prefix(' ').unwrap_or(unit);

    let count: u64 = count.parse().map_err(|_| err())?;
    let factor = match unit {
        "second" | "seconds" => 1,
        "minute" | "minutes" => 60,
        "hour" | "hours" => 60 * 60,
        "day" | "days" => 24 * 60 * 60,
        "week" | "weeks" => 7 * 24 * 60 * 60,
        "month" | "months" => 2_629_746,
        _ => return Err(err()),
    };

    count
        .checked_mul(factor)
        .map(Duration::from_secs)
        .ok_or_else(err)
}

/// Return the latest publish time that is at least `min_age` old at `now`.
///
/// Returns `None` if `min_age` is zero, i.e. the cooldown is disabled.
pub(crate) fn publish_cutoff(now: Timestamp, min_age: Duration) -> Option<Timestamp> {
    if min_age.is_zero() {
        return None;
    }

    // Like Cargo, consider everything too new if the cutoff
    // underflows the range of `Timestamp`.
    Some(
        SignedDuration::try_from(min_age)
            .ok()
            .and_then(|min_age| now.checked_sub(min_age).ok())
            .unwrap_or(Timestamp::MIN),
    )
}

/// Return true if a version published at `pubtime` is newer than `cutoff`.
///
/// Versions without `pubtime` are never too new, as Cargo does.
/// A `pubtime` that cannot be parsed is treated as too new, since Cargo
/// would reject such an index entry.
pub(crate) fn is_too_new(pubtime: Option<&str>, cutoff: Timestamp) -> bool {
    pubtime.is_some_and(|pubtime| {
        pubtime
            .parse::<Timestamp>()
            .map_or(true, |pubtime| pubtime > cutoff)
    })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_min_publish_age() {
        let secs = |secs| Ok(Duration::from_secs(secs));
        let parse = |s| parse_min_publish_age(s).map_err(|_| ());

        assert_eq!(parse("0"), secs(0));
        assert_eq!(parse("0 days"), secs(0));
        assert_eq!(parse("1second"), secs(1));
        assert_eq!(parse("30 seconds"), secs(30));
        assert_eq!(parse("5 minutes"), secs(5 * 60));
        assert_eq!(parse("2 hours"), secs(2 * 60 * 60));
        assert_eq!(parse("1 day"), secs(24 * 60 * 60));
        assert_eq!(parse("7 days"), secs(7 * 24 * 60 * 60));
        assert_eq!(parse("2 weeks"), secs(14 * 24 * 60 * 60));
        assert_eq!(parse("1 month"), secs(2_629_746));

        for invalid in [
            "",
            "7",
            "days",
            "-1 days",
            "1.5 days",
            "7  days",
            "7 Days",
            " 7 days",
            "7 days ",
            "7 fortnights",
            "18446744073709551615 months",
        ] {
            assert!(parse(invalid).is_err(), "{invalid:?} should be rejected");
        }
    }

    #[test]
    fn test_publish_cutoff() {
        let now: Timestamp = "2026-11-14T00:00:00Z".parse().unwrap();

        assert_eq!(publish_cutoff(now, Duration::ZERO), None);
        assert_eq!(
            publish_cutoff(now, Duration::from_secs(7 * 24 * 60 * 60)),
            Some("2026-11-07T00:00:00Z".parse().unwrap())
        );
        assert_eq!(
            publish_cutoff(now, Duration::from_secs(u64::MAX)),
            Some(Timestamp::MIN)
        );
    }

    #[test]
    fn test_is_too_new() {
        let cutoff: Timestamp = "2026-11-07T00:00:00Z".parse().unwrap();

        assert!(!is_too_new(None, cutoff));
        assert!(!is_too_new(Some("2026-11-06T23:59:59Z"), cutoff));
        assert!(!is_too_new(Some("2026-11-07T00:00:00Z"), cutoff));
        assert!(is_too_new(Some("2026-11-07T00:00:01Z"), cutoff));
        // crates.io API's `created_at` format
        assert!(!is_too_new(
            Some("2026-11-06T12:00:00.123456+00:00"),
            cutoff
        ));
        assert!(is_too_new(Some("2026-11-07T12:00:00.123456+00:00"), cutoff));
        assert!(is_too_new(Some("not a timestamp"), cutoff));
    }
}
