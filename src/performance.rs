use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    pub opportunity_detection_latency: Vec<Duration>,
    pub tx_send_latency: Vec<Duration>,
    pub total_latency: Vec<Duration>, // Opportunity detection → TX send
}

impl Default for PerformanceMetrics {
    fn default() -> Self {
        PerformanceMetrics {
            opportunity_detection_latency: Vec::new(),
            tx_send_latency: Vec::new(),
            total_latency: Vec::new(),
        }
    }
}

pub struct PerformanceTracker {
    metrics: Arc<RwLock<PerformanceMetrics>>,
    opportunity_timestamps: Arc<RwLock<std::collections::HashMap<String, Instant>>>,
}

impl PerformanceTracker {
    pub fn new() -> Self {
        PerformanceTracker {
            metrics: Arc::new(RwLock::new(PerformanceMetrics::default())),
            opportunity_timestamps: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    pub async fn record_opportunity_detection(&self, opportunity_id: String) {
        let mut timestamps = self.opportunity_timestamps.write().await;
        timestamps.insert(opportunity_id, Instant::now());
    }

    pub async fn record_tx_send(&self, opportunity_id: String) -> Option<Duration> {
        let mut timestamps = self.opportunity_timestamps.write().await;
        if let Some(detection_time) = timestamps.remove(&opportunity_id) {
            let latency = detection_time.elapsed();
            let mut metrics = self.metrics.write().await;
            metrics.total_latency.push(latency);

            if metrics.total_latency.len() > 100 {
                metrics.total_latency.remove(0);
            }

            if latency > Duration::from_millis(300) {
                log::warn!(
                    "⚠️  High latency detected: {}ms (target: 300ms)",
                    latency.as_millis()
                );
            }

            Some(latency)
        } else {
            None
        }
    }

    pub async fn get_avg_latency(&self) -> Option<Duration> {
        let metrics = self.metrics.read().await;
        if metrics.total_latency.is_empty() {
            return None;
        }

        let total: u128 = metrics.total_latency.iter().map(|d| d.as_millis()).sum();
        let count = metrics.total_latency.len() as u128;

        Some(Duration::from_millis((total / count) as u64))
    }

    pub async fn get_p95_latency(&self) -> Option<Duration> {
        let metrics = self.metrics.read().await;
        if metrics.total_latency.is_empty() {
            return None;
        }

        let mut sorted = metrics.total_latency.clone();
        sorted.sort();
        let p95_index = (sorted.len() as f64 * 0.95) as usize;

        sorted.get(p95_index).copied()
    }

    pub async fn log_metrics(&self) {
        if let Some(avg) = self.get_avg_latency().await {
            if let Some(p95) = self.get_p95_latency().await {
                log::info!(
                    "📊 Performance metrics: avg_latency={}ms, p95_latency={}ms",
                    avg.as_millis(),
                    p95.as_millis()
                );
            }
        }
    }
}

impl Default for PerformanceTracker {
    fn default() -> Self {
        Self::new()
    }
}
