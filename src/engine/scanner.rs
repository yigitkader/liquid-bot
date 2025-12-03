use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::WsClient;
use crate::core::config::Config;
use crate::core::events::{Event, EventBus};
use crate::protocol::Protocol;
use crate::utils::cache::AccountCache;
use anyhow::Result;
use futures_util::future::join_all;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::Duration;

pub struct Scanner {
    rpc: Arc<RpcClient>,
    ws: Arc<WsClient>,
    protocol: Arc<dyn Protocol>,
    event_bus: EventBus,
    cache: Arc<AccountCache>,
    config: Config,
    consecutive_errors: Arc<AtomicU32>,
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
        Scanner {
            rpc,
            ws,
            protocol,
            event_bus,
            cache,
            config,
            consecutive_errors: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Check if we've exceeded max consecutive errors and panic if so
    fn check_error_threshold(&self) -> Result<()> {
        let errors = self.consecutive_errors.load(Ordering::Relaxed);
        if errors >= self.config.max_consecutive_errors {
            log::error!(
                "🚨 CRITICAL: Scanner exceeded max consecutive errors ({} >= {})",
                errors,
                self.config.max_consecutive_errors
            );
            log::error!("🚨 Scanner entering panic mode - shutting down");
            return Err(anyhow::anyhow!(
                "Scanner exceeded max consecutive errors: {} >= {}",
                errors,
                self.config.max_consecutive_errors
            ));
        }
        Ok(())
    }

    fn record_error(&self) -> Result<()> {
        // ✅ FIX: Check for overflow BEFORE incrementing to prevent wraparound
        // If we're near u32::MAX, reset to max_consecutive_errors to keep threshold check working
        let previous_value = self.consecutive_errors.load(Ordering::Relaxed);
        
        if previous_value >= u32::MAX - 10 {
            // Safety margin: reset before overflow to prevent wraparound
            log::error!(
                "🚨 CRITICAL: Scanner error counter near overflow ({}), resetting to max_consecutive_errors ({})",
                previous_value,
                self.config.max_consecutive_errors
            );
            self.consecutive_errors.store(self.config.max_consecutive_errors, Ordering::Relaxed);
            // Still check threshold after reset
            return self.check_error_threshold();
        }

        // Now safely increment
        let errors = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;

        log::warn!(
            "Scanner: consecutive errors: {}/{}",
            errors,
            self.config.max_consecutive_errors
        );
        self.check_error_threshold()
    }

    fn reset_errors(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
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
        for chunk in accounts.chunks(BATCH_SIZE) {
            let parse_tasks: Vec<_> = chunk
                .iter()
                .map(|(pubkey, account)| {
                    let protocol = Arc::clone(&self.protocol);
                    let pubkey = *pubkey;
                    let account = account.clone();
                    async move {
                        protocol
                            .parse_position(&account)
                            .await
                            .map(|pos| (pubkey, pos))
                    }
                })
                .collect();

            let results = join_all(parse_tasks).await;

            for result in results {
                match result {
                    Some((pubkey, position)) => {
                        parsed += 1;
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
                    }
                }
            }
        }

        log::info!(
            "Scanner: initial discovery completed. parsed_positions={}, failed_to_parse={}",
            parsed,
            failed
        );

        Ok(parsed)
    }

    pub async fn start_monitoring(&self) -> Result<()> {
        use solana_client::rpc_filter::RpcFilterType;
        use std::time::Instant;
        use tokio::time::sleep;

        let program_id = self.protocol.program_id();
        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);
        let mut use_websocket = true;
        let mut last_reconnect_attempt = Instant::now();
        const RECONNECT_COOLDOWN: Duration = Duration::from_secs(300); // 5 minutes

        log::info!(
            "Starting monitoring for program: {} (WebSocket mode)",
            program_id
        );

        let mut subscription_id = match self.ws.subscribe_program(&program_id).await {
            Ok(id) => {
                log::info!(
                    "✅ WebSocket subscribed to program {} with subscription ID: {}",
                    program_id,
                    id
                );
                self.reset_errors();
                Some(id)
            }
            Err(e) => {
                log::warn!(
                    "⚠️  WebSocket subscription failed: {}. Falling back to RPC polling mode.",
                    e
                );
                use_websocket = false;
                None
            }
        };

        loop {
            if use_websocket {
                // WebSocket mode: Real-time updates
                if let Some(update) = self.ws.listen().await {
                    log::debug!(
                        "Scanner: WS update received for pubkey={} slot={}",
                        update.pubkey,
                        update.slot
                    );

                    match self.protocol.parse_position(&update.account).await {
                        Some(position) => {
                            self.cache.update(update.pubkey, position.clone()).await;
                            if let Err(e) = self.event_bus.publish(Event::AccountUpdated {
                                pubkey: update.pubkey,
                                position,
                            }) {
                                log::error!("Scanner: Failed to publish AccountUpdated: {}", e);
                                self.record_error()?;
                                continue;
                            }
                            self.reset_errors();
                        }
                        None => {
                            log::debug!(
                                "Scanner: WS account {} could not be parsed into a Position",
                                update.pubkey
                            );
                        }
                    }
                } else {
                    log::warn!("WebSocket connection lost, attempting reconnect...");
                    self.record_error()?;

                    match self.ws.reconnect_with_backoff().await {
                        Ok(()) => {
                            log::info!("✅ WebSocket reconnected, resubscribing...");
                            match self.ws.subscribe_program(&program_id).await {
                                Ok(id) => {
                                    subscription_id = Some(id);
                                    log::info!("✅ WebSocket resubscribed successfully");
                                    self.reset_errors();
                                }
                                Err(e) => {
                                    log::warn!("⚠️  WebSocket resubscription failed: {}. Falling back to RPC polling.", e);
                                    use_websocket = false;
                                    subscription_id = None;
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("❌ WebSocket reconnection failed: {}", e);
                            log::warn!(
                                "⚠️  Falling back to RPC polling mode to continue operation..."
                            );
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
                            match self.protocol.parse_position(&account).await {
                                Some(position) => {
                                    let cached = self.cache.get(&pubkey).await;
                                    let should_publish = if let Some(cached_pos) = &cached {
                                        cached_pos.health_factor != position.health_factor
                                    } else {
                                        true
                                    };

                                    self.cache.update(pubkey, position.clone()).await;

                                    if should_publish {
                                        if let Err(e) = self
                                            .event_bus
                                            .publish(Event::AccountUpdated { pubkey, position })
                                        {
                                            log::error!(
                                                "Scanner: Failed to publish AccountUpdated: {}",
                                                e
                                            );
                                            self.record_error()?;
                                            continue;
                                        }
                                    }
                                }
                                None => {
                                    // Parse failure is not a critical error
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
                                        log::info!("✅ WebSocket reconnected! Switching back to WebSocket mode.");
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
                        log::error!("Scanner: RPC polling failed: {}", e);
                        self.record_error()?;
                    }
                }

                // Wait before next RPC poll
                // ✅ FIX: For free RPC endpoints, use longer interval to avoid rate limits
                // get_program_accounts is expensive (rate limit = 40 req/10s)
                let adjusted_interval = if self.config.is_free_rpc_endpoint() {
                    // Free RPC: minimum 2 minutes to avoid rate limit issues
                    let min_interval = Duration::from_millis(120_000); // 2 minutes
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
