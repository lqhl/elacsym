//! Time utilities

use chrono::{DateTime, Utc};

/// Get current timestamp
pub fn now_timestamp() -> DateTime<Utc> {
    Utc::now()
}

/// Convert timestamp to Unix epoch milliseconds
pub fn to_millis(dt: DateTime<Utc>) -> i64 {
    dt.timestamp_millis()
}

/// Convert Unix epoch milliseconds to timestamp
pub fn from_millis(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap_or_else(Utc::now)
}
