use crate::core::events::{Event, EventBus};
use crate::core::config::Config;
use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::WsClient;
use crate::protocol::Protocol;
use crate::utils::cache::AccountCache;
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use futures_util::future::join_all;
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

    /// Record a critical error and check threshold
    fn record_error(&self) -> Result<()> {
        // CRITICAL FIX: Use checked_add to prevent u32 overflow
        // fetch_add returns the previous value, so we need to add 1 to get the new value
        // If overflow occurs, we should treat it as a critical error
        let previous_value = self.consecutive_errors.fetch_add(1, Ordering::Relaxed);
        let errors = previous_value
            .checked_add(1)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Error counter overflow: consecutive_errors reached u32::MAX ({}). This indicates a critical system issue.",
                    u32::MAX
                )
            })?;
        
        log::warn!(
            "Scanner: consecutive errors: {}/{}",
            errors,
            self.config.max_consecutive_errors
        );
        self.check_error_threshold()
    }

    /// Reset error counter on successful operation
    fn reset_errors(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }

    pub async fn discover_accounts(&self) -> Result<usize> {
        use solana_client::rpc_filter::RpcFilterType;
        
        let program_id = self.protocol.program_id();
        
        // Use data size filter to only fetch Obligation accounts (1300 bytes)
        // This dramatically reduces data transfer and processing time
        // Obligation accounts are typically 1200-1500 bytes, so we filter for 1300 bytes
        const OBLIGATION_DATA_SIZE: u64 = 1300;
        
        log::info!(
            "Scanner: fetching program accounts with data size filter ({} bytes) for program {}",
            OBLIGATION_DATA_SIZE,
            program_id
        );
        
        let accounts = self.rpc
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

        // ✅ CRITICAL FIX: Batch processing with parallel tasks to prevent CPU bottleneck
        // Processing 10,000+ accounts sequentially is too slow
        // Process in batches of 100 accounts in parallel to utilize CPU efficiently
        const BATCH_SIZE: usize = 100;
        let mut parsed = 0usize;
        let mut failed = 0usize;

        // Process accounts in batches
        for chunk in accounts.chunks(BATCH_SIZE) {
            // Create parallel tasks for this batch
            let parse_tasks: Vec<_> = chunk.iter()
                .map(|(pubkey, account)| {
                    let protocol = Arc::clone(&self.protocol);
                    let pubkey = *pubkey;
                    let account = account.clone();
                    async move {
                        protocol.parse_position(&account).await.map(|pos| (pubkey, pos))
                    }
                })
                .collect();

            // Execute all tasks in parallel for this batch
            let results = join_all(parse_tasks).await;
            
            // Process results
            for result in results {
                match result {
                    Some((pubkey, position)) => {
                        parsed += 1;
                        self.cache.insert(pubkey, position.clone()).await;
                        if let Err(e) = self.event_bus.publish(Event::AccountDiscovered {
                            pubkey,
                            position,
                        }) {
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
        use tokio::time::sleep;
        use solana_client::rpc_filter::RpcFilterType;
        use std::time::Instant;
        
        let program_id = self.protocol.program_id();
        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);
        let mut use_websocket = true;
        
        // ✅ CRITICAL: Track last reconnect attempt to prevent reconnect storm
        let mut last_reconnect_attempt = Instant::now();
        const RECONNECT_COOLDOWN: Duration = Duration::from_secs(300); // 5 minutes
        
        log::info!("Starting monitoring for program: {} (WebSocket mode)", program_id);
        
        // Try to start with WebSocket first
        let mut subscription_id = match self.ws.subscribe_program(&program_id).await {
            Ok(id) => {
                log::info!("✅ WebSocket subscribed to program {} with subscription ID: {}", program_id, id);
                self.reset_errors();
                Some(id)
            }
            Err(e) => {
                log::warn!("⚠️  WebSocket subscription failed: {}. Falling back to RPC polling mode.", e);
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
                            self.cache
                                .update(update.pubkey, position.clone())
                                .await;
                            if let Err(e) = self.event_bus.publish(Event::AccountUpdated {
                                pubkey: update.pubkey,
                                position,
                            }) {
                                log::error!("Scanner: Failed to publish AccountUpdated: {}", e);
                                self.record_error()?;
                                continue;
                            }
                            // Reset errors on successful processing
                            self.reset_errors();
                        }
                        None => {
                            log::debug!(
                                "Scanner: WS account {} could not be parsed into a Position",
                                update.pubkey
                            );
                            // Parse failure is not a critical error, don't count it
                        }
                    }
                } else {
                    // WebSocket connection lost
                    log::warn!("WebSocket connection lost, attempting reconnect...");
                    self.record_error()?; // Critical error - connection lost
                    
                    // Try to reconnect
                    match self.ws.reconnect_with_backoff().await {
                        Ok(()) => {
                            log::info!("✅ WebSocket reconnected, resubscribing...");
                            // Resubscribe
                            match self.ws.subscribe_program(&program_id).await {
                                Ok(id) => {
                                    subscription_id = Some(id);
                                    log::info!("✅ WebSocket resubscribed successfully");
                                    self.reset_errors();
                                    // Continue with WebSocket mode
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
                            log::warn!("⚠️  Falling back to RPC polling mode to continue operation...");
                            use_websocket = false;
                            subscription_id = None;
                            // Continue to RPC polling mode below
                        }
                    }
                }
            }
            
            if !use_websocket {
                // RPC Polling mode: Fallback when WebSocket fails
                // This ensures bot continues operating even if WebSocket is unavailable
                log::debug!("Scanner: RPC polling mode - fetching program accounts...");
                
                match self.rpc
                    .get_program_accounts_with_filters(
                        &program_id,
                        vec![RpcFilterType::DataSize(1300)], // Obligation accounts
                    )
                    .await
                {
                    Ok(accounts) => {
                        log::debug!("Scanner: RPC polling fetched {} accounts", accounts.len());
                        
                        for (pubkey, account) in accounts {
                            match self.protocol.parse_position(&account).await {
                                Some(position) => {
                                    // Check if position changed (avoid duplicate events)
                                    let cached = self.cache.get(&pubkey).await;
                                    let should_publish = if let Some(cached_pos) = cached {
                                        // Only publish if position changed significantly
                                        cached_pos.health_factor != position.health_factor
                                    } else {
                                        // New position
                                        true
                                    };
                                    
                                    if should_publish {
                                        self.cache.update(pubkey, position.clone()).await;
                                        if let Err(e) = self.event_bus.publish(Event::AccountUpdated {
                                            pubkey,
                                            position,
                                        }) {
                                            log::error!("Scanner: Failed to publish AccountUpdated: {}", e);
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
                        
                        // Reset errors on successful RPC polling
                        self.reset_errors();
                        
                        // ✅ CRITICAL: Try to reconnect WebSocket only if cooldown expired
                        // This prevents reconnect storm when network is down
                        if subscription_id.is_none() 
                            && last_reconnect_attempt.elapsed() >= RECONNECT_COOLDOWN 
                        {
                            last_reconnect_attempt = Instant::now();
                            match self.ws.reconnect_with_backoff().await {
                                Ok(()) => {
                                    match self.ws.subscribe_program(&program_id).await {
                                        Ok(id) => {
                                            log::info!("✅ WebSocket reconnected! Switching back to WebSocket mode.");
                                            subscription_id = Some(id);
                                            use_websocket = true;
                                            self.reset_errors();
                                        }
                                        Err(e) => {
                                            log::debug!("WebSocket resubscription failed, will retry in 5min: {}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    // WebSocket still unavailable, continue RPC polling
                                    log::debug!("WebSocket reconnect failed, will retry in 5min: {}", e);
                                }
                            }
                        } else if subscription_id.is_none() {
                            let remaining_cooldown = RECONNECT_COOLDOWN.as_secs() - last_reconnect_attempt.elapsed().as_secs();
                            log::debug!("WebSocket reconnect cooldown active, {}s remaining", remaining_cooldown);
                        }
                    }
                    Err(e) => {
                        log::error!("Scanner: RPC polling failed: {}", e);
                        self.record_error()?;
                    }
                }
                
                // Wait before next RPC poll
                sleep(poll_interval).await;
            }
        }
    }

    pub async fn run(&self) -> Result<()> {
        self.discover_accounts().await?;
        self.start_monitoring().await
    }
}
