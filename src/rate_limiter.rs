use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{sleep, Duration};

pub struct RateLimiter {
    last_request_nanos: AtomicU64,
    min_interval_nanos: u64,
}

impl RateLimiter {
    pub fn new(min_interval_ms: u64) -> Self {
        let min_interval_nanos = min_interval_ms * 1_000_000;
        RateLimiter {
            last_request_nanos: AtomicU64::new(0),
            min_interval_nanos,
        }
    }

    fn now_nanos() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }

    pub async fn wait_if_needed(&self) {
        let now_nanos = Self::now_nanos();
        let last_nanos = self.last_request_nanos.load(Ordering::Acquire);
        let elapsed_nanos = now_nanos.saturating_sub(last_nanos);

        if elapsed_nanos < self.min_interval_nanos {
            let wait_nanos = self.min_interval_nanos - elapsed_nanos;
            let wait_duration = Duration::from_nanos(wait_nanos);
            sleep(wait_duration).await;

            let new_now_nanos = Self::now_nanos();
            self.last_request_nanos
                .store(new_now_nanos, Ordering::Release);
        } else {
            self.last_request_nanos.store(now_nanos, Ordering::Release);
        }
    }

    pub async fn record_request(&self) {
        let now_nanos = Self::now_nanos();
        self.last_request_nanos.store(now_nanos, Ordering::Release);
    }
}
