use anyhow::{Context, Result};
use std::time::Duration;

pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_multiplier: f64,
    pub is_rate_limit: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        RetryConfig {
            max_retries: 3,
            initial_delay_ms: 500,
            max_delay_ms: 8000,
            backoff_multiplier: 2.0,
            is_rate_limit: false,
        }
    }
}

impl RetryConfig {
    pub fn for_rate_limit() -> Self {
        RetryConfig {
            max_retries: 5,
            initial_delay_ms: 1000,
            max_delay_ms: 16000,
            backoff_multiplier: 2.0,
            is_rate_limit: true,
        }
    }

    pub fn for_rpc() -> Self {
        RetryConfig {
            max_retries: 5,
            initial_delay_ms: 500,
            max_delay_ms: 8000,
            backoff_multiplier: 2.0,
            is_rate_limit: false,
        }
    }

    pub fn for_transaction() -> Self {
        RetryConfig {
            max_retries: 3,
            initial_delay_ms: 500,
            max_delay_ms: 2000,
            backoff_multiplier: 1.0,
            is_rate_limit: false,
        }
    }

    fn calculate_delay(&self, attempt: u32) -> Duration {
        let base = if self.is_rate_limit && attempt == 1 {
            self.initial_delay_ms
        } else {
            (self.initial_delay_ms as f64 * self.backoff_multiplier.powi((attempt - 1) as i32)) as u64
        };
        Duration::from_millis(base.min(self.max_delay_ms))
    }
}

pub async fn retry_with_backoff<F, Fut, T>(
    mut operation: F,
    config: RetryConfig,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_error = None;
    
    for attempt in 1..=config.max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = Some(e);
                let error = last_error.as_ref().unwrap();
                
                if attempt >= config.max_retries {
                    break;
                }
                
                if !is_retryable_error(error) {
                    return Err(anyhow::anyhow!("Non-retryable error encountered: {}", error));
                }
                
                let delay = config.calculate_delay(attempt);
                let error_msg = if config.is_rate_limit {
                    format!("Rate limit hit, backing off for {:?} (attempt {}/{})", delay, attempt, config.max_retries)
                } else {
                    format!("Retry attempt {}/{} after {:?}", attempt + 1, config.max_retries + 1, delay)
                };
                
                log::warn!("{}: {}", error_msg, error);
                tokio::time::sleep(delay).await;
            }
        }
    }
    
    Err(last_error.unwrap()).context(format!("Operation failed after {} retries", config.max_retries))
}

pub async fn retry_with_validation<F, Fut, T, V>(
    mut operation: F,
    mut validator: V,
    config: RetryConfig,
    op_name: &str,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
    V: FnMut(&T) -> bool,
{
    for attempt in 1..=config.max_retries {
        match operation().await {
            Ok(result) => {
                if validator(&result) {
                    if attempt > 1 {
                        log::info!("{} succeeded on attempt {}", op_name, attempt);
                    }
                    return Ok(result);
                }
                
                if attempt >= config.max_retries {
                    return Err(anyhow::anyhow!("{} validation failed after {} attempts", op_name, config.max_retries));
                }
                
                let delay = config.calculate_delay(attempt);
                log::warn!("{} validation failed on attempt {}, retrying after {:?}", op_name, attempt, delay);
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                if attempt >= config.max_retries {
                    return Err(e).context(format!("{} failed after {} attempts", op_name, config.max_retries));
                }
                
                if !is_retryable_error(&e) {
                    return Err(e).context(format!("{}: Non-retryable error", op_name));
                }
                
                let delay = config.calculate_delay(attempt);
                log::warn!("{} failed (attempt {}/{}), retrying after {:?}: {}", op_name, attempt, config.max_retries, delay, e);
                tokio::time::sleep(delay).await;
            }
        }
    }
    
    Err(anyhow::anyhow!("{} failed after {} attempts", op_name, config.max_retries))
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
    
    if error_str.contains("accountnotfound")
        || error_str.contains("account not found")
        || error_str.contains("invalidaccount")
        || error_str.contains("invalid account")
        || error_str.contains("invalid pubkey")
        || error_str.contains("pubkey parse error")
        || error_str.contains("failed to parse json")
        || error_str.contains("failed to parse")
        || error_str.contains("validation")
        || error_str.contains("no price impact in response")
        || error_str.contains("negative price impact")
        || error_str.contains("suspiciously high price impact")
    {
        return false;
    }
    
    if error_str.contains("timeout")
        || error_str.contains("network error")
        || error_str.contains("connection")
        || error_str.contains("econnreset")
        || error_str.contains("econnrefused")
        || error_str.contains("connection closed")
        || error_str.contains("connection reset")
        || error_str.contains("http error: 429")
        || error_str.contains("rate limit")
        || error_str.contains("too many requests")
        || error_str.contains("http error: 5")
        || error_str.contains("http error: 503")
        || error_str.contains("http error: 502")
        || error_str.contains("http error: 504")
    {
        return true;
    }
    
    if error_str.contains("http error: 4") {
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

