//! Date/time parsing utilities for crosstache
//!
//! This module provides functionality to parse various date/time formats
//! including ISO dates, relative durations, and Azure Key Vault timestamps.

use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use crate::error::{CrosstacheError, Result};

/// Parse a date string in various formats:
/// - ISO 8601 dates: "2024-12-31", "2024-12-31T23:59:59", "2024-12-31T23:59:59Z"
/// - Relative durations: "30d", "7d", "1h", "30m", "1y"
pub fn parse_datetime_or_duration(input: &str) -> Result<DateTime<Utc>> {
    let input = input.trim();
    
    // First try to parse as relative duration
    if let Ok(datetime) = parse_relative_duration(input) {
        return Ok(datetime);
    }
    
    // Then try to parse as ISO date/datetime
    parse_iso_datetime(input)
}

/// Parse relative durations like "30d", "7d", "1h", etc.
/// Supported units: y (years), m (months), d (days), h (hours), min (minutes)
pub fn parse_relative_duration(input: &str) -> Result<DateTime<Utc>> {
    let re = Regex::new(r"^(\d+)([ymdhw]|min)$").unwrap();
    
    if let Some(captures) = re.captures(input) {
        let value: i64 = captures[1].parse().map_err(|_| {
            CrosstacheError::invalid_argument(format!("Invalid number in duration: {}", &captures[1]))
        })?;
        
        let unit = &captures[2];
        let now = Utc::now();
        
        let future_time = match unit {
            "y" => {
                // Approximate: 365.25 days per year
                let days = value * 365 + (value / 4); // Account for leap years approximately
                now + Duration::days(days)
            },
            "m" => {
                // Approximate: 30.44 days per month (365.25/12)
                let days = value * 30 + (value * 44) / 100;
                now + Duration::days(days)
            },
            "w" => now + Duration::weeks(value),
            "d" => now + Duration::days(value),
            "h" => now + Duration::hours(value),
            "min" => now + Duration::minutes(value),
            _ => return Err(CrosstacheError::invalid_argument(format!("Unknown duration unit: {}", unit))),
        };
        
        Ok(future_time)
    } else {
        Err(CrosstacheError::invalid_argument(format!(
            "Invalid relative duration format: '{}'. Expected format like '30d', '7d', '1h', '30min', '1y'", 
            input
        )))
    }
}

/// Parse ISO 8601 date/datetime strings
/// Supported formats:
/// - "2024-12-31" (date only, assumes end of day)
/// - "2024-12-31T23:59:59"
/// - "2024-12-31T23:59:59Z"
/// - "2024-12-31T23:59:59+00:00"
pub fn parse_iso_datetime(input: &str) -> Result<DateTime<Utc>> {
    // Try parsing with timezone first
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.with_timezone(&Utc));
    }
    
    // Try parsing date-only format (YYYY-MM-DD)
    if input.len() == 10 && input.chars().nth(4) == Some('-') && input.chars().nth(7) == Some('-') {
        let date_str = format!("{}T23:59:59Z", input); // End of day
        if let Ok(dt) = DateTime::parse_from_rfc3339(&date_str) {
            return Ok(dt.with_timezone(&Utc));
        }
    }
    
    // Try parsing datetime without timezone (assume UTC)
    if input.contains('T') && !input.contains('Z') && !input.contains('+') && !input.contains('-') {
        let utc_str = format!("{}Z", input);
        if let Ok(dt) = DateTime::parse_from_rfc3339(&utc_str) {
            return Ok(dt.with_timezone(&Utc));
        }
    }
    
    Err(CrosstacheError::invalid_argument(format!(
        "Invalid date format: '{}'. Expected ISO 8601 format (YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS, or YYYY-MM-DDTHH:MM:SSZ) or relative duration (30d, 7d, 1h, etc.)", 
        input
    )))
}

/// Check if a secret has expired based on its expiry date
pub fn is_expired(expires_on: Option<DateTime<Utc>>) -> bool {
    match expires_on {
        Some(expiry) => Utc::now() > expiry,
        None => false, // No expiry date means not expired
    }
}

/// Check if a secret will expire within the specified duration
pub fn is_expiring_within(expires_on: Option<DateTime<Utc>>, duration_str: &str) -> Result<bool> {
    match expires_on {
        Some(expiry) => {
            let threshold = parse_relative_duration(duration_str)?;
            Ok(expiry <= threshold)
        },
        None => Ok(false), // No expiry date means not expiring
    }
}

/// Check if a secret is not yet active based on its not-before date
pub fn is_not_yet_active(not_before: Option<DateTime<Utc>>) -> bool {
    match not_before {
        Some(nbf) => Utc::now() < nbf,
        None => false, // No not-before date means active
    }
}

/// Format a DateTime for display
pub fn format_datetime(dt: Option<DateTime<Utc>>) -> String {
    match dt {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        None => "-".to_string(),
    }
}

/// Parse Unix timestamp to DateTime<Utc>
pub fn parse_unix_timestamp(timestamp: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(timestamp, 0)
}

/// Convert DateTime to Unix timestamp
pub fn to_unix_timestamp(dt: DateTime<Utc>) -> i64 {
    dt.timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_parse_relative_duration() {
        let now = Utc::now();
        
        // Test days
        let result = parse_relative_duration("30d").unwrap();
        assert!((result - now).num_days() >= 29 && (result - now).num_days() <= 31);
        
        // Test hours
        let result = parse_relative_duration("24h").unwrap();
        assert!((result - now).num_hours() >= 23 && (result - now).num_hours() <= 25);
        
        // Test minutes
        let result = parse_relative_duration("60min").unwrap();
        assert!((result - now).num_minutes() >= 59 && (result - now).num_minutes() <= 61);
    }

    #[test]
    fn test_parse_iso_datetime() {
        // Test date only
        let result = parse_iso_datetime("2024-12-31").unwrap();
        assert_eq!(result.date_naive().to_string(), "2024-12-31");
        
        // Test with timezone
        let result = parse_iso_datetime("2024-12-31T23:59:59Z").unwrap();
        assert_eq!(result.year(), 2024);
        assert_eq!(result.month(), 12);
        assert_eq!(result.day(), 31);
    }

    #[test]
    fn test_is_expired() {
        let past = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let future = Utc::now() + Duration::days(1);
        
        assert!(is_expired(Some(past)));
        assert!(!is_expired(Some(future)));
        assert!(!is_expired(None));
    }

    #[test]
    fn test_datetime_or_duration() {
        // Test relative duration
        let result = parse_datetime_or_duration("7d").unwrap();
        assert!((result - Utc::now()).num_days() >= 6);
        
        // Test ISO date
        let result = parse_datetime_or_duration("2024-12-31").unwrap();
        assert_eq!(result.year(), 2024);
        
        // Test invalid input
        assert!(parse_datetime_or_duration("invalid").is_err());
    }
}