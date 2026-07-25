//! Rotation policies and due-date evaluation.
//!
//! ## Why this is policy-driven rather than a scheduler
//!
//! AWS Secrets Manager rotates server-side: the service itself invokes a Lambda
//! on a schedule, which is what `xv rotate --native` triggers. Neither Azure Key
//! Vault nor the local backend has an equivalent — Key Vault can auto-rotate
//! *keys* and emit near-expiry events for secrets, but nothing in it will
//! regenerate a secret's value on a timer, and a local directory obviously has
//! no scheduler at all.
//!
//! So rotation here is split the only way it honestly can be:
//!
//! - **`xv` owns the policy and the due-date math.** The interval lives with the
//!   secret as the [`TAG_ROTATE_EVERY`] tag, and the last rotation as
//!   [`TAG_ROTATED_AT`]. Both travel with the secret across backends and are
//!   visible to anything else reading its metadata.
//! - **The operator owns the clock.** A cron entry, systemd timer, or CI job
//!   runs `xv rotate --due` and rotation happens. `xv rotate --check` reports
//!   without changing anything and exits non-zero when something is overdue, so
//!   a pipeline can gate on it.
//!
//! This is deliberately *not* described as scheduled rotation: nothing rotates
//! unless something invokes `xv`. What it does provide on Azure and local is the
//! part that was missing — a durable policy, a computed due state, and a
//! single command that rotates everything due.
//!
//! `--native` remains AWS-only and is unaffected; a secret can carry a policy on
//! any backend.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use crate::error::{CrosstacheError, Result};

/// Tag holding the rotation interval, e.g. `90d`. Absent = no policy.
pub const TAG_ROTATE_EVERY: &str = "xv:rotate_every";

/// Tag holding the RFC 3339 timestamp of the last rotation.
pub const TAG_ROTATED_AT: &str = "xv:rotated_at";

/// Largest accepted interval (10 years). Guards against a typo like `9999w`
/// silently meaning "never".
const MAX_INTERVAL_DAYS: i64 = 3650;

// ---------------------------------------------------------------------------
// Interval parsing
// ---------------------------------------------------------------------------

/// Parse a rotation interval such as `30d`, `12h`, `6w`.
///
/// Accepted suffixes: `m` (minutes), `h` (hours), `d` (days), `w` (weeks). A
/// unit is required — a bare `90` is ambiguous between minutes and days, and
/// guessing wrong would either rotate constantly or never.
pub fn parse_interval(input: &str) -> Result<Duration> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(invalid_interval(input, "the interval is empty"));
    }

    let (digits, unit) =
        trimmed.split_at(trimmed.find(|c: char| !c.is_ascii_digit()).ok_or_else(|| {
            invalid_interval(
                input,
                "no unit given — use m (minutes), h (hours), d (days), or w (weeks), e.g. 90d",
            )
        })?);

    if digits.is_empty() {
        return Err(invalid_interval(input, "no number given, e.g. 90d"));
    }
    let value: i64 = digits
        .parse()
        .map_err(|_| invalid_interval(input, "the number is too large"))?;
    if value == 0 {
        return Err(invalid_interval(
            input,
            "the interval must be greater than zero",
        ));
    }

    let duration = match unit {
        "m" => Duration::minutes(value),
        "h" => Duration::hours(value),
        "d" => Duration::days(value),
        "w" => Duration::weeks(value),
        other => {
            return Err(invalid_interval(
                input,
                &format!(
                    "unknown unit '{other}' — use m (minutes), h (hours), d (days), or w (weeks)"
                ),
            ))
        }
    };

    if duration > Duration::days(MAX_INTERVAL_DAYS) {
        return Err(invalid_interval(
            input,
            "the interval exceeds the 10-year maximum",
        ));
    }
    Ok(duration)
}

/// Render a duration back into the canonical tag form.
///
/// Chooses the largest unit that divides evenly, so a round-trip of `2w` stays
/// `2w` rather than degrading to `14d`.
pub fn format_interval(d: Duration) -> String {
    let minutes = d.num_minutes();
    if minutes % (60 * 24 * 7) == 0 {
        format!("{}w", minutes / (60 * 24 * 7))
    } else if minutes % (60 * 24) == 0 {
        format!("{}d", minutes / (60 * 24))
    } else if minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn invalid_interval(input: &str, reason: &str) -> CrosstacheError {
    CrosstacheError::InvalidArgument(format!("invalid rotation interval '{input}': {reason}."))
}

// ---------------------------------------------------------------------------
// Due-date evaluation
// ---------------------------------------------------------------------------

/// Where a secret stands relative to its rotation policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationStatus {
    /// No `xv:rotate_every` tag — rotation is not managed for this secret.
    NoPolicy,
    /// Policy present and the next rotation is still in the future.
    Ok {
        /// Time remaining until due.
        due_in: Duration,
        /// When rotation becomes due.
        due_at: DateTime<Utc>,
    },
    /// Policy present and the due date has passed.
    Due {
        /// How long ago it came due.
        overdue_by: Duration,
        /// When rotation became due.
        due_at: DateTime<Utc>,
    },
    /// Policy present but unparseable — treated as a hard error rather than
    /// "no policy", so a typo cannot silently disable rotation.
    Invalid {
        /// The offending tag value.
        value: String,
        /// Why it could not be used.
        reason: String,
    },
}

impl RotationStatus {
    /// Whether this secret should be rotated now.
    pub fn is_due(&self) -> bool {
        matches!(self, Self::Due { .. })
    }

    /// Whether the policy could not be interpreted.
    pub fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid { .. })
    }
}

/// Evaluate a secret's rotation status from its tags.
///
/// `fallback` is used as the rotation baseline when a policy exists but
/// [`TAG_ROTATED_AT`] does not — normally the secret's `updated_on` or
/// `created_on`. Without it, a policy added to an existing secret would have no
/// reference point and could never come due.
pub fn evaluate(
    tags: &HashMap<String, String>,
    fallback: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> RotationStatus {
    let Some(raw) = tags.get(TAG_ROTATE_EVERY) else {
        return RotationStatus::NoPolicy;
    };

    let interval = match parse_interval(raw) {
        Ok(d) => d,
        Err(e) => {
            return RotationStatus::Invalid {
                value: raw.clone(),
                reason: e.to_string(),
            }
        }
    };

    let last = tags
        .get(TAG_ROTATED_AT)
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .or(fallback);

    let Some(last) = last else {
        // A policy with no baseline at all: treat as due, so it gets rotated
        // once and acquires a timestamp, rather than being skipped forever.
        return RotationStatus::Due {
            overdue_by: Duration::zero(),
            due_at: now,
        };
    };

    let due_at = last + interval;
    if now >= due_at {
        RotationStatus::Due {
            overdue_by: now - due_at,
            due_at,
        }
    } else {
        RotationStatus::Ok {
            due_in: due_at - now,
            due_at,
        }
    }
}

/// Human-friendly, coarse duration for status output ("3 days", "5 hours").
pub fn humanize(d: Duration) -> String {
    let d = if d < Duration::zero() { -d } else { d };
    let days = d.num_days();
    if days >= 1 {
        return format!("{days} day{}", plural(days));
    }
    let hours = d.num_hours();
    if hours >= 1 {
        return format!("{hours} hour{}", plural(hours));
    }
    let minutes = d.num_minutes();
    if minutes >= 1 {
        return format!("{minutes} minute{}", plural(minutes));
    }
    "less than a minute".to_string()
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Tags to write when a rotation completes: refresh the timestamp, and set the
/// interval when the caller supplied a new one.
pub fn rotation_tags(interval: Option<Duration>, now: DateTime<Utc>) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    tags.insert(TAG_ROTATED_AT.to_string(), now.to_rfc3339());
    if let Some(interval) = interval {
        tags.insert(TAG_ROTATE_EVERY.to_string(), format_interval(interval));
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // -- parsing ------------------------------------------------------------

    #[test]
    fn parses_every_unit() {
        assert_eq!(parse_interval("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_interval("12h").unwrap(), Duration::hours(12));
        assert_eq!(parse_interval("90d").unwrap(), Duration::days(90));
        assert_eq!(parse_interval("6w").unwrap(), Duration::weeks(6));
        assert_eq!(parse_interval("  90d  ").unwrap(), Duration::days(90));
    }

    #[test]
    fn rejects_ambiguous_or_malformed_intervals() {
        // A bare number is the important rejection: guessing the unit wrong
        // means either constant rotation or none.
        for bad in ["", "90", "d", "-5d", "90x", "9999w", "0d", "1.5d", "d90"] {
            assert!(parse_interval(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn interval_error_names_the_valid_units() {
        let err = parse_interval("90").unwrap_err().to_string();
        assert!(err.contains("90d"), "{err}");
        assert!(err.contains("weeks"), "{err}");
    }

    #[test]
    fn format_interval_round_trips_and_prefers_large_units() {
        for input in ["30m", "12h", "90d", "2w"] {
            let parsed = parse_interval(input).unwrap();
            assert_eq!(format_interval(parsed), input, "round trip of {input}");
        }
        // 14 days is exactly 2 weeks and should render as such.
        assert_eq!(format_interval(Duration::days(14)), "2w");
        assert_eq!(format_interval(Duration::hours(36)), "36h");
    }

    // -- evaluation ---------------------------------------------------------

    #[test]
    fn no_policy_tag_means_unmanaged() {
        let now = Utc::now();
        assert_eq!(
            evaluate(&tags(&[]), Some(now), now),
            RotationStatus::NoPolicy
        );
        // A stray timestamp without an interval is still unmanaged.
        assert_eq!(
            evaluate(
                &tags(&[(TAG_ROTATED_AT, &now.to_rfc3339())]),
                Some(now),
                now
            ),
            RotationStatus::NoPolicy
        );
    }

    #[test]
    fn not_yet_due_reports_remaining_time() {
        let now = Utc::now();
        let last = now - Duration::days(10);
        let status = evaluate(
            &tags(&[
                (TAG_ROTATE_EVERY, "30d"),
                (TAG_ROTATED_AT, &last.to_rfc3339()),
            ]),
            None,
            now,
        );
        match status {
            RotationStatus::Ok { due_in, due_at } => {
                assert_eq!(due_in.num_days(), 20);
                assert_eq!(due_at, last + Duration::days(30));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        assert!(!status.is_due());
    }

    #[test]
    fn past_due_reports_overdue_amount() {
        let now = Utc::now();
        let last = now - Duration::days(40);
        let status = evaluate(
            &tags(&[
                (TAG_ROTATE_EVERY, "30d"),
                (TAG_ROTATED_AT, &last.to_rfc3339()),
            ]),
            None,
            now,
        );
        match status {
            RotationStatus::Due { overdue_by, .. } => {
                assert_eq!(overdue_by.num_days(), 10);
            }
            other => panic!("expected Due, got {other:?}"),
        }
        assert!(status.is_due());
    }

    #[test]
    fn exactly_at_the_boundary_is_due() {
        let now = Utc::now();
        let last = now - Duration::days(30);
        let status = evaluate(
            &tags(&[
                (TAG_ROTATE_EVERY, "30d"),
                (TAG_ROTATED_AT, &last.to_rfc3339()),
            ]),
            None,
            now,
        );
        assert!(status.is_due(), "the boundary should rotate, not wait");
    }

    #[test]
    fn falls_back_to_the_secrets_own_timestamp() {
        // Policy added to a pre-existing secret that has never been rotated.
        let now = Utc::now();
        let created = now - Duration::days(100);
        let status = evaluate(&tags(&[(TAG_ROTATE_EVERY, "30d")]), Some(created), now);
        assert!(
            status.is_due(),
            "a policy on an old secret must come due, not be skipped: {status:?}"
        );

        let recent = now - Duration::days(1);
        let status = evaluate(&tags(&[(TAG_ROTATE_EVERY, "30d")]), Some(recent), now);
        assert!(!status.is_due());
    }

    #[test]
    fn policy_without_any_baseline_is_due_once() {
        let now = Utc::now();
        let status = evaluate(&tags(&[(TAG_ROTATE_EVERY, "30d")]), None, now);
        assert!(
            status.is_due(),
            "with no baseline it must rotate once to acquire a timestamp"
        );
    }

    #[test]
    fn unparseable_policy_is_invalid_not_unmanaged() {
        // The safety-critical case: a typo must be loud, never a silent
        // "rotation is off".
        let now = Utc::now();
        let status = evaluate(&tags(&[(TAG_ROTATE_EVERY, "ninety days")]), Some(now), now);
        match &status {
            RotationStatus::Invalid { value, .. } => assert_eq!(value, "ninety days"),
            other => panic!("expected Invalid, got {other:?}"),
        }
        assert!(
            !status.is_due(),
            "an invalid policy must not trigger a write"
        );
        assert!(status.is_invalid());
    }

    #[test]
    fn unparseable_timestamp_falls_back_rather_than_failing() {
        // A corrupt timestamp with a valid interval should still evaluate,
        // using the fallback, so rotation is not blocked by bad bookkeeping.
        let now = Utc::now();
        let created = now - Duration::days(100);
        let status = evaluate(
            &tags(&[
                (TAG_ROTATE_EVERY, "30d"),
                (TAG_ROTATED_AT, "not-a-timestamp"),
            ]),
            Some(created),
            now,
        );
        assert!(status.is_due(), "{status:?}");
    }

    #[test]
    fn non_utc_timestamps_are_normalized() {
        let now = Utc::now();
        // Same instant expressed with a +05:00 offset.
        let last = (now - Duration::days(40))
            .with_timezone(&chrono::FixedOffset::east_opt(5 * 3600).unwrap())
            .to_rfc3339();
        let status = evaluate(
            &tags(&[(TAG_ROTATE_EVERY, "30d"), (TAG_ROTATED_AT, &last)]),
            None,
            now,
        );
        match status {
            RotationStatus::Due { overdue_by, .. } => assert_eq!(overdue_by.num_days(), 10),
            other => panic!("offset timestamps must normalize to UTC, got {other:?}"),
        }
    }

    // -- tag emission -------------------------------------------------------

    #[test]
    fn rotation_tags_always_stamp_time_and_optionally_the_interval() {
        let now = Utc::now();
        let only_time = rotation_tags(None, now);
        assert_eq!(only_time.len(), 1);
        assert_eq!(only_time[TAG_ROTATED_AT], now.to_rfc3339());

        let with_policy = rotation_tags(Some(Duration::days(90)), now);
        assert_eq!(with_policy[TAG_ROTATE_EVERY], "90d");
        assert_eq!(with_policy[TAG_ROTATED_AT], now.to_rfc3339());
    }

    #[test]
    fn stamped_tags_evaluate_as_not_due() {
        // A rotation that just happened must not immediately look due again.
        let now = Utc::now();
        let written = rotation_tags(Some(Duration::days(1)), now);
        let status = evaluate(&written, None, now);
        assert!(!status.is_due(), "{status:?}");
    }

    #[test]
    fn humanize_reads_naturally() {
        assert_eq!(humanize(Duration::days(1)), "1 day");
        assert_eq!(humanize(Duration::days(3)), "3 days");
        assert_eq!(humanize(Duration::hours(5)), "5 hours");
        assert_eq!(humanize(Duration::minutes(1)), "1 minute");
        assert_eq!(humanize(Duration::seconds(30)), "less than a minute");
        // Overdue durations arrive negated; the label should still read forward.
        assert_eq!(humanize(-Duration::days(2)), "2 days");
    }
}
