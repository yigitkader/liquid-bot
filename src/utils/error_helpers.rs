use anyhow::{Context, Result};
use std::time::Duration;

/// Common error handling utilities for consistent error patterns across the codebase

/// Retry a function with exponential backoff
/// 
/// This is a common pattern used throughout the codebase for handling transient errors.
/// 
/// # Parameters
/// - `operation`: The async function to retry
/// - `max_retries`: Maximum number of retry attempts
/// - `initial_delay_ms`: Initial delay in milliseconds before first retry
/// - `max_delay_ms`: Maximum delay between retries (caps exponential growth)
/// 
/// # Returns
/// - `Ok(T)`: Success result from operation
/// - `Err`: Error after all retries exhausted
/// 
/// # Example
/// ```rust
/// let result = retry_with_backoff(
///     || async { rpc.get_account(&pubkey).await },
///     3,
///     1000,
///     30000,
/// ).await?;
/// ```
pub async fn retry_with_backoff<F, Fut, T>(
    mut operation: F,
    max_retries: u32,
    initial_delay_ms: u64,
    max_delay_ms: u64,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut delay_ms = initial_delay_ms;
    
    for attempt in 0..=max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt >= max_retries {
                    return Err(e).context(format!(
                        "Operation failed after {} retries",
                        max_retries + 1
                    ));
                }
                
                // Check if error is retryable
                if !is_retryable_error(&e) {
                    return Err(e).context("Non-retryable error encountered");
                }
                
                log::debug!(
                    "Retry attempt {}/{} after {}ms delay: {}",
                    attempt + 1,
                    max_retries + 1,
                    delay_ms,
                    e
                );
                
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                
                // Exponential backoff with jitter
                delay_ms = (delay_ms * 2).min(max_delay_ms);
            }
        }
    }
    
    unreachable!()
}

/// Check if an error is retryable (transient error that might succeed on retry)
/// 
/// This helps distinguish between:
/// - **Retryable errors**: Network issues, timeouts, rate limits, server errors (5xx)
/// - **Non-retryable errors**: Client errors (4xx except 429), parse errors, validation errors
/// 
/// # Parameters
/// - `error`: The error to check
/// 
/// # Returns
/// - `true`: Error is retryable (network/timeout/server errors)
/// - `false`: Error is not retryable (client/parse/validation errors)
pub fn is_retryable_error(error: &anyhow::Error) -> bool {
    let error_str = error.to_string().to_lowercase();
    
    // Retryable: Server errors, network errors, timeouts, rate limits
    if error_str.contains("timeout")
        || error_str.contains("network error")
        || error_str.contains("connection")
        || error_str.contains("http error: 429") // Rate limit (4xx but retryable)
        || error_str.contains("http error: 5") // 5xx server errors
        || error_str.contains("http error: 503") // Service unavailable
        || error_str.contains("http error: 502") // Bad gateway
        || error_str.contains("http error: 504") // Gateway timeout
    {
        return true;
    }
    
    // Non-retryable: Client errors (except 429), parse errors, missing data
    if error_str.contains("http error: 4") // 4xx client errors (but NOT 429)
        || error_str.contains("failed to parse json")
        || error_str.contains("failed to parse")
        || error_str.contains("invalid")
        || error_str.contains("not found")
        || error_str.contains("validation")
        || error_str.contains("no price impact in response")
        || error_str.contains("negative price impact")
        || error_str.contains("suspiciously high price impact")
    {
        return false;
    }
    
    // Default: retry (conservative approach for unknown errors)
    true
}

/// Safely increment an atomic counter with overflow protection
/// 
/// This is a common pattern used in error tracking to prevent integer overflow.
/// 
/// # Parameters
/// - `counter`: Atomic counter to increment
/// - `max_value`: Maximum safe value (will reset if exceeded)
/// - `reset_to`: Value to reset to if overflow detected
/// 
/// # Returns
/// - `true`: Counter was incremented successfully
/// - `false`: Counter was reset due to overflow protection
pub fn safe_increment_with_overflow_protection(
    counter: &std::sync::atomic::AtomicU32,
    max_value: u32,
    reset_to: u32,
) -> bool {
    let previous = counter.load(std::sync::atomic::Ordering::Relaxed);
    
    if previous >= max_value.saturating_sub(10) {
        // Safety margin: reset before overflow
        log::warn!(
            "Counter near overflow ({}), resetting to {}",
            previous,
            reset_to
        );
        counter.store(reset_to, std::sync::atomic::Ordering::Relaxed);
        return false;
    }
    
    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    true
}

/// Convert error to user-friendly message
/// 
/// Extracts the most relevant error message from an anyhow::Error chain.
pub fn error_to_string(error: &anyhow::Error) -> String {
    // Try to get the root cause first
    if let Some(cause) = error.root_cause().downcast_ref::<std::io::Error>() {
        return format!("IO error: {}", cause);
    }
    
    // Otherwise return the full error chain
    error.to_string()
}

