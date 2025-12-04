use anyhow::{Context, Result};
use std::time::Duration;

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
                    return Err(e).context(format!("Operation failed after {} retries", max_retries + 1));
                }
                
                if !is_retryable_error(&e) {
                    return Err(e).context("Non-retryable error encountered");
                }
                
                log::debug!("Retry attempt {}/{} after {}ms delay: {}", attempt + 1, max_retries + 1, delay_ms, e);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(max_delay_ms);
            }
        }
    }
    
    unreachable!()
}

pub async fn retry_with_linear_backoff<F, Fut, T>(
    mut operation: F,
    max_retries: u32,
    base_delay_ms: u64,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    for attempt in 0..=max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt >= max_retries {
                    return Err(e);
                }
                let delay_ms = base_delay_ms * (attempt + 1) as u64;
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }
    }
    unreachable!()
}

pub fn is_retryable_error(error: &anyhow::Error) -> bool {
    let error_str = error.to_string().to_lowercase();
    
    if error_str.contains("timeout")
        || error_str.contains("network error")
        || error_str.contains("connection")
        || error_str.contains("http error: 429")
        || error_str.contains("http error: 5")
        || error_str.contains("http error: 503")
        || error_str.contains("http error: 502")
        || error_str.contains("http error: 504")
    {
        return true;
    }
    
    if error_str.contains("http error: 4")
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
    
    true
}

pub fn safe_increment_with_overflow_protection(
    counter: &std::sync::atomic::AtomicU32,
    max_value: u32,
    reset_to: u32,
) -> bool {
    let previous = counter.load(std::sync::atomic::Ordering::Relaxed);
    
    if previous >= max_value.saturating_sub(10) {
        log::warn!("Counter near overflow ({}), resetting to {}", previous, reset_to);
        counter.store(reset_to, std::sync::atomic::Ordering::Relaxed);
        return false;
    }
    
    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    true
}

pub fn error_to_string(error: &anyhow::Error) -> String {
    if let Some(cause) = error.root_cause().downcast_ref::<std::io::Error>() {
        return format!("IO error: {}", cause);
    }
    error.to_string()
}

