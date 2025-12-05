use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::{AccountUpdate, WsClient};
use crate::core::config::Config;
use crate::protocol::solend::accounts::get_associated_token_address;
use crate::protocol::solend::instructions::is_wsol_mint;
use crate::strategy::balance_manager::balance_cache::CachedBalance;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// WebSocket subscription manager for balance monitoring
pub struct BalanceSubscription {
    balances: Arc<RwLock<HashMap<Pubkey, CachedBalance>>>,
    rpc: Arc<RpcClient>,
    ws: Arc<WsClient>,
    wallet: Pubkey,
    config: Config,
    subscribed_atas: Arc<RwLock<HashMap<Pubkey, Pubkey>>>, // mint -> ATA pubkey mapping
}

impl BalanceSubscription {
    pub fn new(
        balances: Arc<RwLock<HashMap<Pubkey, CachedBalance>>>,
        rpc: Arc<RpcClient>,
        ws: Arc<WsClient>,
        wallet: Pubkey,
        config: Config,
    ) -> Self {
        Self {
            balances,
            rpc,
            ws,
            wallet,
            config,
            subscribed_atas: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn start_monitoring(&self) -> Result<()> {
        let mints = vec![
            ("USDC", self.config.usdc_mint.as_str()),
            ("SOL", self.config.sol_mint.as_str()),
            (
                "USDT",
                self.config.usdt_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
            ),
            (
                "ETH",
                self.config.eth_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
            ),
            (
                "BTC",
                self.config.btc_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
            ),
        ];

        log::info!("🔍 Subscribing to ATA balances via WebSocket...");

        let mut subscribed_count = 0;
        let sol_mint = Pubkey::from_str(&self.config.sol_mint)
            .map_err(|_| anyhow::anyhow!("Failed to parse SOL mint address from config"))?;

        for (name, mint_str) in &mints {
            if mint_str.is_empty() {
                continue;
            }

            let mint = Pubkey::from_str(mint_str)
                .map_err(|e| anyhow::anyhow!("Invalid {} mint: {}", name, e))
                .context(format!("Failed to parse {} mint address", name))?;

            if mint == sol_mint || is_wsol_mint(&mint) {
                let wsol_ata = get_associated_token_address(&self.wallet, &mint, Some(&self.config))
                    .map_err(|e| anyhow::anyhow!("Failed to derive WSOL ATA for {}: {}", name, e))
                    .context(format!("Failed to derive WSOL ATA for {}", name))?;

                match self.ws.subscribe_account(&wsol_ata).await {
                    Ok(subscription_id) => {
                        log::info!(
                            "✅ Subscribed to {} WSOL ATA: {} (subscription ID: {})",
                            name,
                            wsol_ata,
                            subscription_id
                        );

                        let mut subscribed_atas = self.subscribed_atas.write().await;
                        subscribed_atas.insert(mint, wsol_ata);

                        if let Ok(account) = self.rpc.get_account(&wsol_ata).await {
                            if account.data.len() >= 72 {
                                let balance_bytes: [u8; 8] = account.data[64..72]
                                    .try_into()
                                    .map_err(|_| anyhow::anyhow!("Failed to read balance"))?;
                                let wsol_balance = u64::from_le_bytes(balance_bytes);
                                let mut balances = self.balances.write().await;
                                balances.insert(
                                    wsol_ata,
                                    CachedBalance {
                                        amount: wsol_balance,
                                        timestamp: std::time::Instant::now(),
                                    },
                                );
                                log::debug!(
                                    "BalanceManager: Initial {} WSOL balance cached: {}",
                                    name,
                                    wsol_balance
                                );
                            }
                        }

                        subscribed_count += 1;
                    }
                    Err(e) => {
                        log::warn!("⚠️  Failed to subscribe to {} WSOL ATA ({}): {}", name, wsol_ata, e);
                        log::warn!("   Balance will be fetched via RPC on demand");
                    }
                }
                continue;
            }

            let ata = get_associated_token_address(&self.wallet, &mint, Some(&self.config))
                .map_err(|e| anyhow::anyhow!("Failed to derive ATA for {}: {}", name, e))
                .context(format!("Failed to derive ATA for {}", name))?;

            match self.ws.subscribe_account(&ata).await {
                Ok(subscription_id) => {
                    log::info!(
                        "✅ Subscribed to {} ATA: {} (subscription ID: {})",
                        name,
                        ata,
                        subscription_id
                    );

                    let mut subscribed_atas = self.subscribed_atas.write().await;
                    subscribed_atas.insert(mint, ata);

                    if let Ok(account) = self.rpc.get_account(&ata).await {
                        if account.data.len() >= 72 {
                            let balance_bytes: [u8; 8] = account.data[64..72]
                                .try_into()
                                .map_err(|_| anyhow::anyhow!("Failed to read balance"))?;
                            let balance = u64::from_le_bytes(balance_bytes);
                            let mut balances = self.balances.write().await;
                            balances.insert(
                                ata,
                                CachedBalance {
                                    amount: balance,
                                    timestamp: std::time::Instant::now(),
                                },
                            );
                            log::debug!(
                                "BalanceManager: Initial {} balance cached: {}",
                                name,
                                balance
                            );
                        }
                    }

                    subscribed_count += 1;
                }
                Err(e) => {
                    log::warn!("⚠️  Failed to subscribe to {} ATA ({}): {}", name, ata, e);
                    log::warn!("   Balance will be fetched via RPC on demand");
                }
            }
        }

        log::info!(
            "✅ BalanceManager: Subscribed to {} ATA(s) via WebSocket",
            subscribed_count
        );
        Ok(())
    }

    pub async fn handle_account_update(&self, update: &AccountUpdate) {
        let subscribed_atas = self.subscribed_atas.read().await;
        let is_subscribed = subscribed_atas.values().any(|&ata| ata == update.pubkey);

        if !is_subscribed {
            return;
        }

        if update.account.data.len() < 72 {
            log::warn!(
                "BalanceManager: Invalid account data length for ATA {}: {} bytes",
                update.pubkey,
                update.account.data.len()
            );
            return;
        }

        let balance_bytes: [u8; 8] = match update.account.data[64..72].try_into() {
            Ok(bytes) => bytes,
            Err(_) => {
                log::warn!(
                    "BalanceManager: Failed to read balance bytes for ATA {}",
                    update.pubkey
                );
                return;
            }
        };

        let balance = u64::from_le_bytes(balance_bytes);
        let subscribed_atas_check = self.subscribed_atas.read().await;
        if subscribed_atas_check
            .values()
            .any(|&ata| ata == update.pubkey)
        {
            let mut balances = self.balances.write().await;
            if let Some(cached) = balances.get_mut(&update.pubkey) {
                cached.amount = balance;
                cached.timestamp = std::time::Instant::now();
            } else {
                balances.insert(
                    update.pubkey,
                    CachedBalance {
                        amount: balance,
                        timestamp: std::time::Instant::now(),
                    },
                );
            }

            log::debug!(
                "BalanceManager: Updated balance cache for ATA {}: {}",
                update.pubkey,
                balance
            );
        } else {
            log::debug!(
                "BalanceManager: Skipping update for ATA {} (unsubscribed during processing)",
                update.pubkey
            );
        }
    }

    pub async fn listen_account_updates(&self) -> Result<()> {
        let mut receiver = self.ws.subscribe_account_updates();
        log::info!("🔄 BalanceManager: Starting account update listener...");

        loop {
            match receiver.recv().await {
                Ok(update) => {
                    self.handle_account_update(&update).await;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!(
                        "BalanceManager: Lagged behind by {} account updates (this is OK during high load)",
                        skipped
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    log::error!("BalanceManager: Account update channel closed, listener stopping");
                    break;
                }
            }
        }

        Ok(())
    }

    pub async fn stop_monitoring(&self) -> Result<()> {
        let mut subscribed_atas = self.subscribed_atas.write().await;
        let mut balances_guard = self.balances.write().await;

        let subscription_count = subscribed_atas.len();

        subscribed_atas.clear();
        balances_guard.clear();

        log::info!(
            "BalanceManager: Stopped monitoring (cleared {} subscriptions and cache)",
            subscription_count
        );
        Ok(())
    }

    pub fn subscribed_atas(&self) -> Arc<RwLock<HashMap<Pubkey, Pubkey>>> {
        Arc::clone(&self.subscribed_atas)
    }
}

