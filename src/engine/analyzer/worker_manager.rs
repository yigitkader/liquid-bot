/// Worker management and scaling logic for the analyzer
/// 
/// This module handles:
/// - Dynamic worker scaling based on load
/// - Task queue management
/// - Worker pool coordination

use crate::core::config::Config;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Duration;

pub struct WorkerManager {
    current_workers: Arc<AtomicUsize>,
    max_workers_limit: usize,
    config: Config,
}

impl WorkerManager {
    pub fn new(config: Config) -> Self {
        let initial_workers = config.analyzer_max_workers;
        WorkerManager {
            current_workers: Arc::new(AtomicUsize::new(initial_workers)),
            max_workers_limit: config.analyzer_max_workers_limit,
            config,
        }
    }

    pub fn create_semaphore(&self) -> Arc<Semaphore> {
        Arc::new(Semaphore::new(
            self.current_workers.load(Ordering::Relaxed),
        ))
    }

    pub fn current_workers(&self) -> usize {
        self.current_workers.load(Ordering::Relaxed)
    }

    /// Handle lag event by scaling up workers
    pub fn handle_lag(&self, semaphore: &Arc<Semaphore>, skipped: usize) {
        const MAX_CONCURRENT_VALIDATIONS: usize = 100;
        let current_target = self.current_workers.load(Ordering::Relaxed);

        if current_target < self.max_workers_limit {
            let increase = std::cmp::max(2, current_target / 2);
            let new_target =
                std::cmp::min(current_target + increase, self.max_workers_limit);

            let current_available = semaphore.available_permits();

            let permits_to_add = if current_available < new_target {
                new_target - current_available
            } else {
                0
            };

            let estimated_total_after = current_available + permits_to_add;
            let final_permits_to_add =
                if estimated_total_after > MAX_CONCURRENT_VALIDATIONS {
                    MAX_CONCURRENT_VALIDATIONS.saturating_sub(current_available)
                } else {
                    permits_to_add
                };

            if final_permits_to_add > 0 {
                semaphore.add_permits(final_permits_to_add);
                let actual_new_target = current_target + final_permits_to_add;
                self.current_workers
                    .store(actual_new_target, Ordering::Relaxed);

                if estimated_total_after > MAX_CONCURRENT_VALIDATIONS {
                    log::warn!(
                        "WorkerManager: Analyzer lagged, skipped {} events. Increasing workers: {} -> {} (added {} permits, available: {}, capped at max {})",
                        skipped,
                        current_target,
                        actual_new_target,
                        final_permits_to_add,
                        semaphore.available_permits(),
                        MAX_CONCURRENT_VALIDATIONS
                    );
                } else {
                    log::warn!(
                        "WorkerManager: Analyzer lagged, skipped {} events. Increasing workers: {} -> {} (added {} permits, available: {})",
                        skipped,
                        current_target,
                        actual_new_target,
                        final_permits_to_add,
                        semaphore.available_permits()
                    );
                }
            } else if permits_to_add > 0 {
                log::error!(
                    "WorkerManager: Analyzer lagged {} events, but cannot add more permits (max concurrent: {}, available: {})",
                    skipped,
                    MAX_CONCURRENT_VALIDATIONS,
                    current_available
                );
            } else {
                self.current_workers.store(new_target, Ordering::Relaxed);
                log::warn!(
                    "WorkerManager: Analyzer lagged, skipped {} events. Target updated: {} -> {} (permits already sufficient: available={}, target={})",
                    skipped,
                    current_target,
                    new_target,
                    current_available,
                    new_target
                );
            }
        } else {
            log::error!(
                "WorkerManager: Analyzer lagged {} events, max workers ({}) reached!",
                skipped,
                self.max_workers_limit
            );
        }
    }

    /// Scale down workers if no lag for a while
    pub fn try_scale_down(
        &self,
        semaphore: &Arc<Semaphore>,
        last_lag: Instant,
        last_scale_down: &mut Instant,
    ) {
        const SCALE_DOWN_THRESHOLD: Duration = Duration::from_secs(60);
        const SCALE_DOWN_INTERVAL: Duration = Duration::from_secs(30);

        if last_lag.elapsed() > SCALE_DOWN_THRESHOLD
            && last_scale_down.elapsed() > SCALE_DOWN_INTERVAL
        {
            let current = self.current_workers.load(Ordering::Relaxed);
            let target_workers = self.config.analyzer_max_workers;

            if current > target_workers {
                let decrease = std::cmp::max(1, current / 4);
                let new_workers = std::cmp::max(target_workers, current - decrease);

                self.current_workers.store(new_workers, Ordering::Relaxed);
                *last_scale_down = Instant::now();

                log::info!(
                    "WorkerManager: Scaling down target (no lag for {}s): {} -> {} (available permits: {})",
                    SCALE_DOWN_THRESHOLD.as_secs(),
                    current,
                    new_workers,
                    semaphore.available_permits()
                );
            }
        }
    }

    /// Cleanup completed tasks from the task queue
    pub fn cleanup_tasks<T: 'static>(tasks: &mut JoinSet<T>) -> usize {
        const MAX_CONCURRENT_TASKS: usize = 1000;
        let mut cleaned_count = 0;
        
        while let Some(result) = tasks.try_join_next() {
            cleaned_count += 1;
            if let Err(e) = result {
                log::error!("WorkerManager: Task failed: {}", e);
            }
        }

        if cleaned_count > 0 {
            log::debug!(
                "WorkerManager: Cleaned up {} completed task(s), {} remaining",
                cleaned_count,
                tasks.len()
            );
        }

        if tasks.len() >= MAX_CONCURRENT_TASKS {
            log::warn!(
                "WorkerManager: Task queue still full after cleanup ({} tasks). Tasks may be completing slowly.",
                tasks.len()
            );
        }

        cleaned_count
    }
}

