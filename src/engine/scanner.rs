use crate::core::events::{Event, EventBus};
use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::WsClient;
use crate::protocol::Protocol;
use crate::utils::cache::AccountCache;
use anyhow::Result;
use std::sync::Arc;

pub struct Scanner {
    rpc: Arc<RpcClient>,
    ws: Arc<WsClient>,
    protocol: Arc<dyn Protocol>,
    event_bus: EventBus,
    cache: Arc<AccountCache>,
}

impl Scanner {
    pub fn new(
        rpc: Arc<RpcClient>,
        ws: Arc<WsClient>,
        protocol: Arc<dyn Protocol>,
        event_bus: EventBus,
        cache: Arc<AccountCache>,
    ) -> Self {
        Scanner {
            rpc,
            ws,
            protocol,
            event_bus,
            cache,
        }
    }

    pub async fn discover_accounts(&self) -> Result<usize> {
        let program_id = self.protocol.program_id();
        let accounts = self.rpc.get_program_accounts(&program_id).await?;

        log::info!(
            "Scanner: fetched {} program accounts for program {}",
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
                        self.event_bus.publish(Event::AccountUpdated {
                            pubkey: update.pubkey,
                            position,
                        })?;
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
                self.ws.reconnect_with_backoff().await;
            }
        }
    }

    pub async fn run(&self) -> Result<()> {
        self.discover_accounts().await?;
        self.start_monitoring().await
    }
}
