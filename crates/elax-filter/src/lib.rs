//! Filter evaluation primitives for structured predicates.

use anyhow::Result;

bitflags::bitflags! {
    /// Placeholder flag set for filters.
    pub struct FilterFlags: u8 {
        const EMPTY = 0b0000_0001;
    }
}

/// Placeholder evaluate function producing empty flags.
pub fn evaluate() -> Result<FilterFlags> {
    Ok(FilterFlags::EMPTY)
}
