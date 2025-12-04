/// Error tracking utility for components that need to track consecutive errors
/// 
/// This module provides a reusable error tracking mechanism used by Scanner,
/// Executor, and other components to prevent infinite error loops.

use anyhow::Result;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Error tracker for consecutive error counting with overflow protection
pub struct ErrorTracker {
    consecutive_errors: Arc<AtomicU32>,
    max_consecutive_errors: u32,
    component_name: &'static str,
}

impl ErrorTracker {
    /// Create a new error tracker
    pub fn new(max_consecutive_errors: u32, component_name: &'static str) -> Self {
        ErrorTracker {
            consecutive_errors: Arc::new(AtomicU32::new(0)),
            max_consecutive_errors,
            component_name,
        }
    }

    /// Get current error count
    pub fn current_errors(&self) -> u32 {
        self.consecutive_errors.load(Ordering::Relaxed)
    }

    /// Check if error threshold is exceeded
    /// 
    /// Returns an error if threshold is exceeded, which should trigger shutdown.
    pub fn check_error_threshold(&self) -> Result<()> {
        let errors = self.consecutive_errors.load(Ordering::Relaxed);
        if errors >= self.max_consecutive_errors {
            log::error!(
                "CRITICAL: {} exceeded max consecutive errors ({} >= {})",
                self.component_name,
                errors,
                self.max_consecutive_errors
            );
            log::error!("{} entering panic mode - shutting down", self.component_name);
            return Err(anyhow::anyhow!(
                "{} exceeded max consecutive errors: {} >= {}",
                self.component_name,
                errors,
                self.max_consecutive_errors
            ));
        }
        Ok(())
    }

    /// Record an error and check threshold
    /// 
    /// Handles overflow protection automatically.
    /// Returns error if threshold exceeded.
    pub fn record_error(&self) -> Result<()> {
        let previous_value = self.consecutive_errors.load(Ordering::Relaxed);
        
        if previous_value >= u32::MAX - 10 {
            let reset_value = self.max_consecutive_errors.saturating_sub(5);
            log::error!(
                "CRITICAL: {} error counter near overflow ({}), resetting to {} (threshold: {}, buffer: 5 errors)",
                self.component_name,
                previous_value,
                reset_value,
                self.max_consecutive_errors
            );
            self.consecutive_errors.store(reset_value, Ordering::Relaxed);
            return Ok(());
        }

        // Safely increment
        let errors = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
        log::warn!(
            "{}: consecutive errors: {}/{}",
            self.component_name,
            errors,
            self.max_consecutive_errors
        );
        self.check_error_threshold()
    }

    /// Reset error count to zero
    pub fn reset_errors(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }

    /// Get the underlying atomic counter (for advanced use cases)
    pub fn counter(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.consecutive_errors)
    }
}

