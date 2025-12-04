use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::WsClient;
use crate::core::config::Config;
use crate::core::events::{Event, EventBus};
use crate::protocol::Protocol;
use crate::utils::cache::AccountCache;
use crate::utils::error_tracker::ErrorTracker;
use anyhow::Result;
use futures_util::future::join_all;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::time::Duration;

pub struct Scanner {
    rpc: Arc<RpcClient>,
    ws: Arc<WsClient>,
    protocol: Arc<dyn Protocol>,
    event_bus: EventBus,
    cache: Arc<AccountCache>,
    config: Config,
    error_tracker: ErrorTracker,
}

impl Scanner {
    pub fn new(
        rpc: Arc<RpcClient>,
        ws: Arc<WsClient>,
        protocol: Arc<dyn Protocol>,
        event_bus: EventBus,
        cache: Arc<AccountCache>,
        config: Config,
    ) -> Self {
        let error_tracker = ErrorTracker::new(
            config.max_consecutive_errors,
            "Scanner",
        );
        Scanner {
            rpc,
            ws,
            protocol,
            event_bus,
            cache,
            config,
            error_tracker,
        }
    }

    /// Check if we've exceeded max consecutive errors and panic if so
    fn check_error_threshold(&self) -> Result<()> {
        self.error_tracker.check_error_threshold()
    }

    fn record_error(&self) -> Result<()> {
        self.error_tracker.record_error()
    }

    fn reset_errors(&self) {
        self.error_tracker.reset_errors();
    }

    pub async fn discover_accounts(&self) -> Result<usize> {
        use solana_client::rpc_filter::RpcFilterType;

        let program_id = self.protocol.program_id();
        const OBLIGATION_DATA_SIZE: u64 = 1300;

        log::info!(
            "Scanner: fetching program accounts with data size filter ({} bytes) for program {}",
            OBLIGATION_DATA_SIZE,
            program_id
        );

        let accounts = self
            .rpc
            .get_program_accounts_with_filters(
                &program_id,
                vec![RpcFilterType::DataSize(OBLIGATION_DATA_SIZE)],
            )
            .await?;

        log::info!(
            "Scanner: fetched {} filtered program accounts (obligations only) for program {}",
            accounts.len(),
            program_id
        );

        const BATCH_SIZE: usize = 100;
        let mut parsed = 0usize;
        let mut failed = 0usize;
        let mut parse_stats = std::collections::HashMap::<String, usize>::new();
        let mut health_factors = Vec::<f64>::new();
        let mut sample_discriminators = std::collections::HashSet::<[u8; 8]>::new();
        let mut sample_failed_pubkeys = Vec::<Pubkey>::new();
        let mut data_size_distribution = std::collections::HashMap::<usize, usize>::new();
        let mut sample_failed_sizes = Vec::<usize>::new();
        
        for chunk in accounts.chunks(BATCH_SIZE) {
            let parse_tasks: Vec<_> = chunk
                .iter()
                .map(|(pubkey, account)| {
                    let protocol = Arc::clone(&self.protocol);
                    let pubkey = *pubkey;
                    let account = account.clone();
                    async move {
                        let result = protocol.parse_position(&account, Some(Arc::clone(&self.rpc))).await;
                        let failure_reason = if result.is_none() {
                            Self::categorize_parse_failure(&account)
                        } else {
                            None
                        };
                        (result.map(|pos| (pubkey, pos)), failure_reason, pubkey, account)
                    }
                })
                .collect();

            let results = join_all(parse_tasks).await;

            for (result, failure_reason, pubkey, account) in results {
                match result {
                    Some((pubkey, position)) => {
                        parsed += 1;
                        health_factors.push(position.health_factor);
                        self.cache.insert(pubkey, position.clone()).await;
                        if let Err(e) = self
                            .event_bus
                            .publish(Event::AccountDiscovered { pubkey, position })
                        {
                            log::error!("Scanner: Failed to publish AccountDiscovered: {}", e);
                        }
                    }
                    None => {
                        failed += 1;
                        let data_size = account.data.len();
                        *data_size_distribution.entry(data_size).or_insert(0) += 1;
                        
                        if let Some(reason) = &failure_reason {
                            *parse_stats.entry(reason.clone()).or_insert(0) += 1;
                            
                            // Collect sample discriminators for parse failures (for debugging)
                            if reason == "parse_failed_or_empty" && sample_discriminators.len() < 5 && data_size >= 8 {
                                let disc = [
                                    account.data[0], account.data[1], account.data[2], account.data[3],
                                    account.data[4], account.data[5], account.data[6], account.data[7],
                                ];
                                sample_discriminators.insert(disc);
                                if sample_failed_pubkeys.len() < 3 {
                                    sample_failed_pubkeys.push(pubkey);
                                }
                                if sample_failed_sizes.len() < 5 {
                                    sample_failed_sizes.push(data_size);
                                }
                            }
                        }
                    }
                }
            }
        }

        log::info!("Scanner: initial discovery completed. parsed_positions={}, failed_to_parse={}, total_accounts={}", parsed, failed, accounts.len());
        
        if parsed == 0 && failed > 0 {
            log::warn!("WARNING: No positions were parsed from {} accounts! Data size filter may be incorrect (filtering for {} bytes)", failed, OBLIGATION_DATA_SIZE);
        }
        
        Self::log_distribution("Data size distribution", &data_size_distribution, 10);
        Self::log_distribution("Parse failure breakdown", &parse_stats, 10);
        
        if !sample_discriminators.is_empty() {
            log::info!("Scanner: Sample discriminators from failed accounts: {:?}", sample_discriminators.iter().take(5).collect::<Vec<_>>());
            if !sample_failed_pubkeys.is_empty() {
                log::info!("Scanner: Sample failed account pubkeys: {:?}", sample_failed_pubkeys.iter().take(3).collect::<Vec<_>>());
            }
        }
        
        if !health_factors.is_empty() {
            health_factors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let count = health_factors.len();
            let liquidatable = health_factors.iter().filter(|&&hf| hf < self.config.hf_liquidation_threshold).count();
            log::info!(
                "Scanner: Health factor distribution ({} positions): min={:.4}, p25={:.4}, p50={:.4}, p75={:.4}, max={:.4}, liquidatable={} (HF < {})",
                count, health_factors[0], health_factors[count / 4], health_factors[count / 2], health_factors[(count * 3) / 4], health_factors[count - 1], liquidatable, self.config.hf_liquidation_threshold
            );
        }

        Ok(parsed)
    }

    fn categorize_parse_failure(account: &solana_sdk::account::Account) -> Option<String> {
        let data_len = account.data.len();
        const MIN_OBLIGATION_SIZE: usize = 1200;
        const MAX_OBLIGATION_SIZE: usize = 1500;
        
        if data_len < MIN_OBLIGATION_SIZE {
            Some(format!("data_too_small ({} < {})", data_len, MIN_OBLIGATION_SIZE))
        } else if data_len > MAX_OBLIGATION_SIZE {
            Some(format!("data_too_large ({} > {})", data_len, MAX_OBLIGATION_SIZE))
        } else {
            Some("parse_failed_or_empty".to_string())
        }
    }

    fn log_distribution<T: std::fmt::Display>(name: &str, distribution: &std::collections::HashMap<T, usize>, max_items: usize) {
        if distribution.is_empty() {
            return;
        }
        log::info!("Scanner: {}:", name);
        let mut sorted: Vec<_> = distribution.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        for (key, count) in sorted.iter().take(max_items) {
            log::info!("  - {}: {} accounts", key, count);
        }
        if sorted.len() > max_items {
            log::info!("  ... and {} more categories", sorted.len() - max_items);
        }
    }

    pub async fn start_monitoring(&self) -> Result<()> {
        use solana_client::rpc_filter::RpcFilterType;
        use std::time::Instant;
        use tokio::time::sleep;

        let program_id = self.protocol.program_id();
        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);
        let mut use_websocket = true;
        let mut last_reconnect_attempt = Instant::now();
        let mut last_failed_subscription_check = Instant::now();
        const RECONNECT_COOLDOWN: Duration = Duration::from_secs(300); // 5 minutes
        const FAILED_SUBSCRIPTION_CHECK_INTERVAL: Duration = Duration::from_secs(30); // Check every 30 seconds

        log::info!(
            "Starting monitoring for program: {} (WebSocket mode)",
            program_id
        );

        let mut subscription_id = match self.ws.subscribe_program(&program_id).await {
            Ok(id) => {
                log::info!("WebSocket subscribed to program {} with subscription ID: {}", program_id, id);
                self.reset_errors();
                Some(id)
            }
            Err(e) => {
                log::warn!("WebSocket subscription failed: {}. Falling back to RPC polling mode.", e);
                use_websocket = false;
                None
            }
        };

            loop {
            if last_failed_subscription_check.elapsed() >= FAILED_SUBSCRIPTION_CHECK_INTERVAL {
                last_failed_subscription_check = Instant::now();
                let failed_subscriptions = self.ws.get_failed_subscriptions().await;
                if !failed_subscriptions.is_empty() {
                    log::warn!("Found {} failed subscription(s) - restarting scanner to restore subscriptions", failed_subscriptions.len());
                    
                    let mut restored = 0;
                    for info in &failed_subscriptions {
                        let result = match &info.subscription_type {
                            crate::blockchain::ws_client::SubscriptionType::Program(id) => self.ws.subscribe_program(id).await,
                            crate::blockchain::ws_client::SubscriptionType::Account(pk) => self.ws.subscribe_account(pk).await,
                            crate::blockchain::ws_client::SubscriptionType::Slot => self.ws.subscribe_slot().await,
                        };
                        if result.is_ok() {
                            restored += 1;
                        }
                    }
                    
                    if restored == failed_subscriptions.len() {
                        self.ws.clear_failed_subscriptions().await;
                        log::info!("All {} subscription(s) restored", restored);
                    } else {
                        log::warn!("Only {}/{} subscription(s) restored", restored, failed_subscriptions.len());
                        let _ = self.event_bus.publish(Event::SubscriptionLost {
                            count: failed_subscriptions.len() - restored,
                        });
                    }
                }
            }
            
            if use_websocket {
                // WebSocket mode: Real-time updates
                if let Some(update) = self.ws.listen().await {
                    log::debug!(
                        "Scanner: WS update received for pubkey={} slot={}",
                        update.pubkey,
                        update.slot
                    );

                    if let Some(position) = self.protocol.parse_position(&update.account, Some(Arc::clone(&self.rpc))).await {
                        self.cache.update(update.pubkey, position.clone()).await;
                        if self.event_bus.publish(Event::AccountUpdated {
                            pubkey: update.pubkey,
                            position,
                        }).is_err() {
                            self.record_error()?;
                            continue;
                        }
                        self.reset_errors();
                    }
                } else {
                    log::warn!("WebSocket connection lost (listen() returned None), attempting reconnect...");
                    log::debug!("WebSocket: This may indicate network issues, RPC provider problems, or connection timeout");
                    self.record_error()?;

                    match self.ws.reconnect_with_backoff().await {
                        Ok(()) => {
                            match self.ws.subscribe_program(&program_id).await {
                                Ok(id) => {
                                    subscription_id = Some(id);
                                    log::info!("WebSocket reconnected and resubscribed");
                                    self.reset_errors();
                                }
                                Err(e) => {
                                    log::warn!("WebSocket resubscription failed: {}, falling back to RPC polling", e);
                                    use_websocket = false;
                                    subscription_id = None;
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("WebSocket reconnection failed: {}, falling back to RPC polling", e);
                            use_websocket = false;
                            subscription_id = None;
                        }
                    }
                }
            }

            if !use_websocket {
                log::debug!("Scanner: RPC polling mode - fetching program accounts...");

                match self
                    .rpc
                    .get_program_accounts_with_filters(
                        &program_id,
                        vec![RpcFilterType::DataSize(1300)],
                    )
                    .await
                {
                    Ok(accounts) => {
                        log::debug!("Scanner: RPC polling fetched {} accounts", accounts.len());

                        for (pubkey, account) in accounts {
                            if let Some(position) = self.protocol.parse_position(&account, Some(Arc::clone(&self.rpc))).await {
                                let should_publish = self.cache.get(&pubkey).await
                                    .map(|cached| cached.health_factor != position.health_factor)
                                    .unwrap_or(true);
                                self.cache.update(pubkey, position.clone()).await;
                                if should_publish && self.event_bus.publish(Event::AccountUpdated { pubkey, position }).is_err() {
                                    self.record_error()?;
                                    continue;
                                }
                            }
                        }

                        self.reset_errors();

                        if subscription_id.is_none()
                            && last_reconnect_attempt.elapsed() >= RECONNECT_COOLDOWN
                        {
                            last_reconnect_attempt = Instant::now();
                            match self.ws.reconnect_with_backoff().await {
                                Ok(()) => match self.ws.subscribe_program(&program_id).await {
                                    Ok(id) => {
                                        log::info!("WebSocket reconnected! Switching back to WebSocket mode.");
                                        subscription_id = Some(id);
                                        use_websocket = true;
                                        self.reset_errors();
                                    }
                                    Err(e) => {
                                        log::debug!("WebSocket resubscription failed, will retry in 5min: {}", e);
                                    }
                                },
                                Err(e) => {
                                    // WebSocket still unavailable, continue RPC polling
                                    log::debug!(
                                        "WebSocket reconnect failed, will retry in 5min: {}",
                                        e
                                    );
                                }
                            }
                        } else if subscription_id.is_none() {
                            let remaining_cooldown = RECONNECT_COOLDOWN.as_secs()
                                - last_reconnect_attempt.elapsed().as_secs();
                            log::debug!(
                                "WebSocket reconnect cooldown active, {}s remaining",
                                remaining_cooldown
                            );
                        }
                    }
                    Err(e) => {
                        let error_str = e.to_string().to_lowercase();
                        
                        if error_str.contains("429") 
                            || error_str.contains("rate limit")
                            || error_str.contains("too many requests")
                        {
                            log::warn!("Scanner: Rate limit hit (429), applying exponential backoff");
                            
                            let base_interval = if self.config.is_free_rpc_endpoint() {
                                let min_interval = Duration::from_millis(120_000);
                                poll_interval.max(min_interval)
                            } else {
                                poll_interval
                            };
                            
                            let backoff_interval = base_interval * 2;
                            let max_backoff = Duration::from_millis(600_000);
                            let backoff_interval = backoff_interval.min(max_backoff);
                            
                            log::info!("Scanner: Rate limit backoff: waiting {:?} before next poll (base: {:?}, 2x backoff)", backoff_interval, base_interval);
                            sleep(backoff_interval).await;
                            continue;
                        }
                        
                        log::error!("Scanner: RPC polling failed: {}", e);
                        self.record_error()?;
                    }
                }

                let adjusted_interval = if self.config.is_free_rpc_endpoint() {
                    let min_interval = Duration::from_millis(120_000);
                    poll_interval.max(min_interval)
                } else {
                    poll_interval
                };
                sleep(adjusted_interval).await;
            }
        }
    }

    pub async fn run(&self) -> Result<()> {
        self.discover_accounts().await?;
        self.start_monitoring().await
    }
}
