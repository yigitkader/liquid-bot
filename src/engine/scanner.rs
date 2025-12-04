use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::WsClient;
use crate::core::config::Config;
use crate::core::events::{Event, EventBus};
use crate::protocol::Protocol;
use crate::utils::cache::AccountCache;
use anyhow::Result;
use futures_util::future::join_all;
use solana_sdk::pubkey::Pubkey;
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
        // If we're near u32::MAX, reset to a safe value below threshold to prevent immediate panic
        let previous_value = self.consecutive_errors.load(Ordering::Relaxed);
        
        if previous_value >= u32::MAX - 10 {
            // Safety margin: reset before overflow to prevent wraparound
            // ✅ CRITICAL FIX: Reset to threshold - 5 instead of threshold to prevent immediate panic
            // This gives buffer for temporary network issues without causing bot restart
            let reset_value = self.config.max_consecutive_errors.saturating_sub(5);
            log::error!(
                "🚨 CRITICAL: Scanner error counter near overflow ({}), resetting to {} (threshold: {}, buffer: 5 errors)",
                previous_value,
                reset_value,
                self.config.max_consecutive_errors
            );
            self.consecutive_errors.store(reset_value, Ordering::Relaxed);
            // Don't check threshold after reset - we're below threshold now
            // This prevents immediate panic on overflow protection
            return Ok(());
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

        // Log parse statistics
        log::info!(
            "Scanner: initial discovery completed. parsed_positions={}, failed_to_parse={}, total_accounts={}",
            parsed,
            failed,
            accounts.len()
        );
        
        // Warn if no positions were parsed
        if parsed == 0 && failed > 0 {
            log::warn!("⚠️  WARNING: No positions were parsed from {} accounts! This may indicate:", failed);
            log::warn!("   1. Data size filter may be incorrect (currently filtering for exactly {} bytes)", OBLIGATION_DATA_SIZE);
            log::warn!("   2. Obligation accounts may have different sizes (expected: 1200-1500 bytes)");
            log::warn!("   3. Account structure may not match expected obligation format");
            log::warn!("   4. All accounts may be empty (no deposits/borrows) or malformed");
            log::warn!("   → Check sample discriminators and pubkeys below for on-chain inspection");
        }
        
        // Log data size distribution
        if !data_size_distribution.is_empty() {
            log::info!("Scanner: Data size distribution of discovered accounts:");
            let mut sorted_sizes: Vec<_> = data_size_distribution.iter().collect();
            sorted_sizes.sort_by(|a, b| b.1.cmp(a.1));
            for (size, count) in sorted_sizes.iter().take(10) {
                log::info!("  - {} bytes: {} accounts", size, count);
            }
            if sorted_sizes.len() > 10 {
                log::info!("  ... and {} more size categories", sorted_sizes.len() - 10);
            }
        }
        
        if !parse_stats.is_empty() {
            log::info!("Scanner: Parse failure breakdown:");
            let mut sorted_stats: Vec<_> = parse_stats.iter().collect();
            sorted_stats.sort_by(|a, b| b.1.cmp(a.1));
            for (reason, count) in sorted_stats.iter().take(10) {
                log::info!("  - {}: {} accounts", reason, count);
            }
            if sorted_stats.len() > 10 {
                log::info!("  ... and {} more categories", sorted_stats.len() - 10);
            }
        }
        
        // Log sample discriminators from failed accounts (helps debug parse issues)
        if !sample_discriminators.is_empty() {
            log::info!("Scanner: Sample discriminators from failed accounts (for debugging account types):");
            for (idx, disc) in sample_discriminators.iter().take(5).enumerate() {
                log::info!("  Sample {}: {:02x?}", idx + 1, disc);
            }
            if !sample_failed_pubkeys.is_empty() {
                log::info!("Scanner: Sample failed account pubkeys (for on-chain inspection):");
                for (idx, pubkey) in sample_failed_pubkeys.iter().take(3).enumerate() {
                    log::info!("  Account {}: {} (check on Solana Explorer)", idx + 1, pubkey);
                }
            }
            if !sample_failed_sizes.is_empty() {
                log::info!("Scanner: Sample failed account data sizes: {:?}", sample_failed_sizes);
            }
        }
        
        // Log health factor distribution
        if !health_factors.is_empty() {
            health_factors.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let count = health_factors.len();
            let p50 = health_factors[count / 2];
            let p25 = health_factors[count / 4];
            let p75 = health_factors[(count * 3) / 4];
            let min = health_factors[0];
            let max = health_factors[count - 1];
            let liquidatable = health_factors.iter().filter(|&&hf| hf < self.config.hf_liquidation_threshold).count();
            
            log::info!(
                "Scanner: Health factor distribution ({} positions): min={:.4}, p25={:.4}, p50={:.4}, p75={:.4}, max={:.4}, liquidatable={} (HF < {})",
                count, min, p25, p50, p75, max, liquidatable, self.config.hf_liquidation_threshold
            );
        }

        Ok(parsed)
    }

    /// Categorize why an account failed to parse
    fn categorize_parse_failure(account: &solana_sdk::account::Account) -> Option<String> {
        let data_len = account.data.len();
        const MIN_OBLIGATION_SIZE: usize = 1200;
        const MAX_OBLIGATION_SIZE: usize = 1500;
        
        if data_len < MIN_OBLIGATION_SIZE {
            return Some(format!("data_too_small ({} < {})", data_len, MIN_OBLIGATION_SIZE));
        }
        
        if data_len > MAX_OBLIGATION_SIZE {
            return Some(format!("data_too_large ({} > {})", data_len, MAX_OBLIGATION_SIZE));
        }
        
        // ✅ FIX: Removed discriminator check - now we try to parse all accounts in size range
        // If parse fails, it's either:
        // 1. Wrong account structure (not an obligation)
        // 2. Parse error (malformed data)
        // 3. Empty position (no deposits/borrows)
        // We can't distinguish without actually parsing, so return generic
        Some("parse_failed_or_empty".to_string())
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
            // ✅ FIX: Periodically check for failed subscriptions and restart if needed
            // This prevents data loss when subscriptions fail during WebSocket reconnect
            if last_failed_subscription_check.elapsed() >= FAILED_SUBSCRIPTION_CHECK_INTERVAL {
                last_failed_subscription_check = Instant::now();
                let failed_subscriptions = self.ws.get_failed_subscriptions().await;
                if !failed_subscriptions.is_empty() {
                    log::warn!(
                        "⚠️  Found {} failed subscription(s) - restarting scanner to restore subscriptions",
                        failed_subscriptions.len()
                    );
                    
                    // Try to restore failed subscriptions
                    let mut restored_count = 0;
                    for info in &failed_subscriptions {
                        let restore_result = match &info.subscription_type {
                            crate::blockchain::ws_client::SubscriptionType::Program(program_id) => {
                                self.ws.subscribe_program(program_id).await
                            }
                            crate::blockchain::ws_client::SubscriptionType::Account(pubkey) => {
                                self.ws.subscribe_account(pubkey).await
                            }
                            crate::blockchain::ws_client::SubscriptionType::Slot => {
                                self.ws.subscribe_slot().await
                            }
                        };
                        
                        match restore_result {
                            Ok(new_id) => {
                                restored_count += 1;
                                log::info!("✅ Restored failed subscription: new_id={}", new_id);
                            }
                            Err(e) => {
                                log::warn!("⚠️  Failed to restore subscription: {}", e);
                            }
                        }
                    }
                    
                    if restored_count == failed_subscriptions.len() {
                        // All subscriptions restored - clear failed list
                        self.ws.clear_failed_subscriptions().await;
                        log::info!("✅ All {} failed subscription(s) restored successfully", restored_count);
                    } else {
                        log::warn!(
                            "⚠️  Only {}/{} subscription(s) restored. {} still failed.",
                            restored_count,
                            failed_subscriptions.len(),
                            failed_subscriptions.len() - restored_count
                        );
                        // Publish event to notify other components
                        if let Err(e) = self.event_bus.publish(Event::SubscriptionLost {
                            count: failed_subscriptions.len() - restored_count,
                        }) {
                            log::error!("Scanner: Failed to publish SubscriptionLost event: {}", e);
                        }
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

                    match self.protocol.parse_position(&update.account, Some(Arc::clone(&self.rpc))).await {
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
                    log::warn!("WebSocket connection lost (listen() returned None), attempting reconnect...");
                    log::debug!("WebSocket: This may indicate network issues, RPC provider problems, or connection timeout");
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
                            log::info!("📡 Scanner: Switching to RPC polling mode (WebSocket unavailable)");
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
                            log::info!("📡 Scanner: Switching to RPC polling mode (WebSocket reconnection failed after 10 attempts)");
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
                            match self.protocol.parse_position(&account, Some(Arc::clone(&self.rpc))).await {
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
                        let error_str = e.to_string().to_lowercase();
                        
                        // ✅ CRITICAL FIX: Rate limit errors (429) are expected on free RPC
                        // Problem: 30 consecutive 429 errors → bot panic (crash)
                        // Solution: Don't count rate limit errors as fatal, apply backoff instead
                        if error_str.contains("429") 
                            || error_str.contains("rate limit")
                            || error_str.contains("too many requests")
                        {
                            // Rate limit: expected for free RPC, don't count as fatal error
                            log::warn!(
                                "Scanner: Rate limit hit (429), applying exponential backoff (not counting as fatal error)"
                            );
                            
                            // Calculate adjusted interval first
                            let base_interval = if self.config.is_free_rpc_endpoint() {
                                let min_interval = Duration::from_millis(120_000); // 2 minutes
                                poll_interval.max(min_interval)
                            } else {
                                poll_interval
                            };
                            
                            // Exponential backoff for rate limits: 2x base interval (capped at 10 minutes)
                            // This gives RPC time to recover from rate limit
                            let backoff_interval = base_interval * 2;
                            let max_backoff = Duration::from_millis(600_000); // 10 minutes max
                            let backoff_interval = backoff_interval.min(max_backoff);
                            
                            log::info!(
                                "Scanner: Rate limit backoff: waiting {:?} before next poll (base: {:?}, 2x backoff)",
                                backoff_interval,
                                base_interval
                            );
                            
                            sleep(backoff_interval).await;
                            continue; // Don't call record_error(), just retry after backoff
                        }
                        
                        // Other errors: count as fatal (network errors, server errors, etc.)
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
