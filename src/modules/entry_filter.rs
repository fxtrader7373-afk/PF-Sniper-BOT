//! Entry Filter — "second wave" timing logic.
//!
//! Skips the unwinnable first-2-block bot races.
//! Realistic assumption: by block 2, the initial front-running bots have
//! already competed and the price has settled to a discoverable range.
//! Entering at block 3-5 captures the organic momentum wave.

use std::time::Duration;
use chrono::Utc;
use tracing::debug;

use crate::core::types::PoolCreationEvent;
use crate::config::FilterConfig;

/// Determines whether a new pool has passed enough time to be in the
/// "second wave" entry zone.
pub struct EntryFilter {
    config: FilterConfig,
}

impl EntryFilter {
    pub fn new(config: FilterConfig) -> Self {
        Self { config }
    }

    /// Returns true if this pool is eligible for entry (passed the delay window)
    pub fn is_eligible(&self, event: &PoolCreationEvent) -> bool {
        let elapsed = Utc::now().signed_duration_since(event.timestamp);
        let elapsed_secs = elapsed.num_seconds() as u64;

        let delay = self.config.entry_delay_seconds;
        let eligible = elapsed_secs >= delay;

        if eligible {
            debug!(
                "Pool {} eligible: {}s elapsed >= {}s delay",
                event.mint, elapsed_secs, delay
            );
        } else {
            debug!(
                "Pool {} waiting: {}s elapsed < {}s delay",
                event.mint, elapsed_secs, delay
            );
        }

        eligible
    }

    /// Returns the seconds remaining until this pool is eligible
    pub fn time_until_eligible(&self, event: &PoolCreationEvent) -> Duration {
        let elapsed = Utc::now().signed_duration_since(event.timestamp);
        let elapsed_secs = elapsed.num_seconds() as u64;

        if elapsed_secs >= self.config.entry_delay_seconds {
            Duration::ZERO
        } else {
            Duration::from_secs(self.config.entry_delay_seconds - elapsed_secs)
        }
    }
}
