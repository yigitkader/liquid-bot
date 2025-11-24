use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Rate limiter - RPC çağrılarını sınırlandırır
pub struct RateLimiter {
    last_request: Mutex<Option<Instant>>,
    min_interval: Duration,
}

impl RateLimiter {
    pub fn new(min_interval_ms: u64) -> Self {
        RateLimiter {
            last_request: Mutex::new(None),
            min_interval: Duration::from_millis(min_interval_ms),
        }
    }

    /// Bir sonraki request için bekleme süresini kontrol et ve gerekirse bekle
    pub async fn wait_if_needed(&self) {
        let mut last = self.last_request.lock().await;
        if let Some(last_time) = *last {
            let elapsed = last_time.elapsed();
            if elapsed < self.min_interval {
                let wait_time = self.min_interval - elapsed;
                drop(last); // Lock'u bırak
                tokio::time::sleep(wait_time).await;
                // Tekrar lock al ve güncelle
                let mut last = self.last_request.lock().await;
                *last = Some(Instant::now());
            } else {
                *last = Some(Instant::now());
            }
        } else {
            *last = Some(Instant::now());
        }
    }

    /// Rate limit kontrolü yapmadan sadece zamanı güncelle
    pub async fn record_request(&self) {
        let mut last = self.last_request.lock().await;
        *last = Some(Instant::now());
    }
}

