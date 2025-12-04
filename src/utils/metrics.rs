use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct Metrics {
    opportunities_found: AtomicU64,
    opportunities_approved: AtomicU64,
    opportunities_rejected: AtomicU64,
    transactions_sent: AtomicU64,
    transactions_successful: AtomicU64,
    total_profit_usd: Arc<RwLock<f64>>,
    latency: Arc<RwLock<Vec<Duration>>>,
}

impl Metrics {
    pub fn new() -> Self {
        Metrics {
            opportunities_found: AtomicU64::new(0),
            opportunities_approved: AtomicU64::new(0),
            opportunities_rejected: AtomicU64::new(0),
            transactions_sent: AtomicU64::new(0),
            transactions_successful: AtomicU64::new(0),
            total_profit_usd: Arc::new(RwLock::new(0.0)),
            latency: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn record_opportunity(&self) {
        self.opportunities_found.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_opportunity_approved(&self) {
        self.opportunities_approved.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_opportunity_rejected(&self) {
        self.opportunities_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn record_transaction(&self, success: bool, profit: f64) {
        self.transactions_sent.fetch_add(1, Ordering::Relaxed);
        if success {
            self.transactions_successful.fetch_add(1, Ordering::Relaxed);
        }
        *self.total_profit_usd.write().await += profit;
    }

    pub async fn record_latency(&self, duration: Duration) {
        self.latency.write().await.push(duration);
    }

    pub async fn get_summary(&self) -> MetricsSummary {
        let opportunities = self.opportunities_found.load(Ordering::Relaxed);
        let opportunities_approved = self.opportunities_approved.load(Ordering::Relaxed);
        let opportunities_rejected = self.opportunities_rejected.load(Ordering::Relaxed);
        let tx_sent = self.transactions_sent.load(Ordering::Relaxed);
        let tx_success = self.transactions_successful.load(Ordering::Relaxed);
        let success_rate = if tx_sent > 0 {
            tx_success as f64 / tx_sent as f64
        } else {
            0.0
        };
        let total_profit = *self.total_profit_usd.read().await;

        let latencies = self.latency.read().await;
        let avg_latency_ms = if !latencies.is_empty() {
            let sum: u64 = latencies.iter().map(|d| d.as_millis() as u64).sum();
            sum / latencies.len() as u64
        } else {
            0
        };

        let p95_latency_ms = if !latencies.is_empty() {
            let mut sorted = latencies
                .iter()
                .map(|d| d.as_millis() as u64)
                .collect::<Vec<_>>();
            sorted.sort();
            let index = (sorted.len() as f64 * 0.95) as usize;
            sorted
                .get(index.min(sorted.len() - 1))
                .copied()
                .unwrap_or(0)
        } else {
            0
        };

        MetricsSummary {
            opportunities,
            opportunities_approved,
            opportunities_rejected,
            tx_sent,
            tx_success,
            success_rate,
            total_profit,
            avg_latency_ms,
            p95_latency_ms,
        }
    }
}

pub struct MetricsSummary {
    pub opportunities: u64,
    pub opportunities_approved: u64,
    pub opportunities_rejected: u64,
    pub tx_sent: u64,
    pub tx_success: u64,
    pub success_rate: f64,
    pub total_profit: f64,
    pub avg_latency_ms: u64,
    pub p95_latency_ms: u64,
}
