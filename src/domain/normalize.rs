use chrono::{DateTime, Utc};

pub fn parse_time_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// Converts an already-observed clock value to the timestamp representation
/// used by the domain. Reading the clock remains an application concern.
pub fn timestamp_ms(time: DateTime<Utc>) -> i64 {
    time.timestamp_millis()
}
