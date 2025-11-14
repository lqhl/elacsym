//! ID generation utilities

use uuid::Uuid;

/// Generate a unique ID
pub fn generate_id() -> String {
    Uuid::new_v4().to_string()
}

/// Generate a short ID (first 8 characters)
pub fn generate_short_id() -> String {
    let id = Uuid::new_v4().to_string();
    id.chars().take(8).collect()
}
