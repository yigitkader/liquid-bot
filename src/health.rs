use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{Duration, Instant};

/// System health durumu
#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub is_healthy: bool,
    pub last_successful_poll: Option<Instant>,
    pub last_error: Option<String>,
    pub consecutive_errors: u32,
    pub total_opportunities: u64,
    pub total_transactions: u64,
    pub successful_transactions: u64,
}

impl Default for HealthStatus {
    fn default() -> Self {
        HealthStatus {
            is_healthy: true,
            last_successful_poll: None,
            last_error: None,
            consecutive_errors: 0,
            total_opportunities: 0,
            total_transactions: 0,
            successful_transactions: 0,
        }
    }
}

/// Health check manager - sistem sağlığını takip eder
pub struct HealthManager {
    status: Arc<RwLock<HealthStatus>>,
    max_error_age: Duration,
}

impl HealthManager {
    pub fn new(max_error_age_secs: u64) -> Self {
        HealthManager {
            status: Arc::new(RwLock::new(HealthStatus::default())),
            max_error_age: Duration::from_secs(max_error_age_secs),
        }
    }

    /// Health durumunu al
    pub async fn get_status(&self) -> HealthStatus {
        let status = self.status.read().await;
        status.clone()
    }

    /// Başarılı poll kaydet
    pub async fn record_successful_poll(&self) {
        let mut status = self.status.write().await;
        status.last_successful_poll = Some(Instant::now());
        status.consecutive_errors = 0;
        status.is_healthy = true;
        status.last_error = None;
    }

    /// Hata kaydet
    pub async fn record_error(&self, error: String) {
        let mut status = self.status.write().await;
        status.consecutive_errors += 1;
        status.last_error = Some(error);
        
        // Çok fazla hata varsa unhealthy olarak işaretle
        if status.consecutive_errors >= 10 {
            status.is_healthy = false;
        }
    }

    /// Opportunity kaydet
    pub async fn record_opportunity(&self) {
        let mut status = self.status.write().await;
        status.total_opportunities += 1;
    }

    /// Transaction kaydet
    pub async fn record_transaction(&self, success: bool) {
        let mut status = self.status.write().await;
        status.total_transactions += 1;
        if success {
            status.successful_transactions += 1;
        }
    }

    /// Health check - sistem sağlıklı mı?
    pub async fn check_health(&self) -> bool {
        let status = self.status.read().await;
        
        // Eğer hiç başarılı poll yoksa ve çok eskiyse unhealthy
        if let Some(last_poll) = status.last_successful_poll {
            if last_poll.elapsed() > self.max_error_age {
                return false;
            }
        } else {
            // İlk poll henüz yapılmadıysa bekle
            return true;
        }
        
        status.is_healthy
    }
}

