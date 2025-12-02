use crate::core::events::{Event, EventBus};
use crate::core::config::Config;
use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::WsClient;
use crate::protocol::Protocol;
use crate::utils::cache::AccountCache;
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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
        let errors = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
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

        let mut parsed = 0usize;
        let mut failed = 0usize;

        for (pubkey, account) in accounts {
            match self.protocol.parse_position(&account).await {
                Some(position) => {
                    parsed += 1;
                    self.cache.insert(pubkey, position.clone()).await;
                    self.event_bus.publish(Event::AccountDiscovered {
                        pubkey,
                        position,
                    })?;
                }
                None => {
                    failed += 1;
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
        use anyhow::Context;
        let program_id = self.protocol.program_id();
        log::info!("Starting WebSocket monitoring for program: {}", program_id);
        
        let subscription_id = self
            .ws
            .subscribe_program(&program_id)
            .await
            .context("Failed to subscribe to program")?;
        log::info!("Subscribed to program {} with subscription ID: {}", program_id, subscription_id);
        
        // Reset errors on successful subscription
        self.reset_errors();
        
        loop {
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
                log::warn!("WebSocket connection lost, attempting reconnect...");
                self.record_error()?; // Critical error - connection lost
                self.ws.reconnect_with_backoff().await;
            }
        }
    }

    pub async fn run(&self) -> Result<()> {
        self.discover_accounts().await?;
        self.start_monitoring().await
    }
}
