use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::{AccountUpdate, WsClient};
use crate::core::config::Config;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};

/// Cached balance with timestamp for freshness checking
#[derive(Clone, Debug)]
struct CachedBalance {
    amount: u64,
    timestamp: Instant,
}

/// Cache time-to-live: balances older than this will be considered stale
const CACHE_TTL: Duration = Duration::from_secs(30);

pub struct BalanceManager {
    reserved: Arc<RwLock<HashMap<Pubkey, u64>>>, // mint -> reserved amount
    balances: Arc<RwLock<HashMap<Pubkey, CachedBalance>>>, // ATA pubkey -> cached balance with timestamp
    rpc: Arc<RpcClient>,
    ws: Option<Arc<WsClient>>,
    wallet: Pubkey,
    config: Option<Config>,
    subscribed_atas: Arc<RwLock<HashMap<Pubkey, Pubkey>>>, // mint -> ATA pubkey mapping
}

impl BalanceManager {
    pub fn new(rpc: Arc<RpcClient>, wallet: Pubkey) -> Self {
        BalanceManager {
            reserved: Arc::new(RwLock::new(HashMap::new())),
            balances: Arc::new(RwLock::new(HashMap::new())),
            rpc,
            ws: None,
            wallet,
            config: None,
            subscribed_atas: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn with_config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub fn with_websocket(mut self, ws: Arc<WsClient>) -> Self {
        self.ws = Some(ws);
        self
    }

    pub async fn get_available_balance(&self, mint: &Pubkey) -> Result<u64> {
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;

        {
            let balances = self.balances.read().await;
            if let Some(cached) = balances.get(&ata) {
                if cached.timestamp.elapsed() < CACHE_TTL {
                    // Read reserved amount without holding it across other locks/RPC calls
                    let reserved_amount = {
                        let reserved = self.reserved.read().await;
                        reserved.get(mint).copied().unwrap_or(0)
                    };
                    let available = cached.amount.saturating_sub(reserved_amount);
                    log::debug!(
                        "BalanceManager: Cache hit for mint {} (ata={}): cached={}, reserved={}, available={}, age={:.2}s",
                        mint, ata, cached.amount, reserved_amount, available, cached.timestamp.elapsed().as_secs_f64()
                    );
                    return Ok(available);
                } else {
                    log::debug!(
                        "BalanceManager: Cache stale for mint {} (ata={}): age={:.2}s (TTL={}s), falling back to RPC",
                        mint, ata, cached.timestamp.elapsed().as_secs_f64(), CACHE_TTL.as_secs()
                    );
                }
            }
        }

        log::debug!(
            "BalanceManager: Cache miss for mint {} (ata={}), falling back to RPC",
            mint,
            ata
        );

        let account = match self.rpc.get_account(&ata).await {
            Ok(acc) => acc,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("AccountNotFound") || error_msg.contains("account not found")
                {
                    log::debug!(
                        "BalanceManager: ATA not found for mint {} (ata={}), wallet={}. Returning 0 balance (ATA will be auto-created during transaction if needed)",
                        mint,
                        ata,
                        self.wallet
                    );

                    let mut balances = self.balances.write().await;
                    balances.insert(
                        ata,
                        CachedBalance {
                            amount: 0,
                            timestamp: Instant::now(),
                        },
                    );

                    let reserved_amount = {
                        let reserved = self.reserved.read().await;
                        reserved.get(mint).copied().unwrap_or(0)
                    };

                    return Ok(0u64.saturating_sub(reserved_amount));
                }
                // Other errors (network issues, RPC errors, etc.) should be propagated
                return Err(e);
            }
        };

        if account.data.len() < 72 {
            return Err(anyhow::anyhow!("Invalid token account data"));
        }

        let balance_bytes: [u8; 8] = account.data[64..72]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read balance from token account"))?;
        let actual = u64::from_le_bytes(balance_bytes);

        // Update cache with RPC result (while still holding reserved lock)
        {
            let mut balances = self.balances.write().await;
            balances.insert(
                ata,
                CachedBalance {
                    amount: actual,
                    timestamp: Instant::now(),
                },
            );
        }

        let reserved_amount = {
            let reserved = self.reserved.read().await;
            reserved.get(mint).copied().unwrap_or(0)
        };

        let available = actual.saturating_sub(reserved_amount);
        Ok(available)
    }

    pub async fn get_available_balance_locked(
        &self,
        mint: &Pubkey,
        reserved: &HashMap<Pubkey, u64>,
    ) -> Result<u64> {
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;

        {
            let balances = self.balances.read().await;
            if let Some(cached) = balances.get(&ata) {
                if cached.timestamp.elapsed() < CACHE_TTL {
                    let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
                    return Ok(cached.amount.saturating_sub(reserved_amount));
                }
                // Stale cache, fall through to RPC
            }
        }

        let account = match self.rpc.get_account(&ata).await {
            Ok(acc) => acc,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("AccountNotFound") || error_msg.contains("account not found")
                {
                    // Cache zero balance
                    let mut balances = self.balances.write().await;
                    balances.insert(
                        ata,
                        CachedBalance {
                            amount: 0,
                            timestamp: Instant::now(),
                        },
                    );
                    let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
                    return Ok(0u64.saturating_sub(reserved_amount));
                }
                return Err(e);
            }
        };

        if account.data.len() < 72 {
            return Err(anyhow::anyhow!("Invalid token account data"));
        }

        let balance_bytes: [u8; 8] = account.data[64..72]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read balance from token account"))?;
        let actual = u64::from_le_bytes(balance_bytes);

        // Update cache
        {
            let mut balances = self.balances.write().await;
            balances.insert(
                ata,
                CachedBalance {
                    amount: actual,
                    timestamp: Instant::now(),
                },
            );
        }

        let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
        Ok(actual.saturating_sub(reserved_amount))
    }

    pub async fn start_monitoring(&self) -> Result<()> {
        let ws = self
            .ws
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("WebSocket client not set for BalanceManager"))?;

        use crate::protocol::solend::accounts::get_associated_token_address;
        use std::str::FromStr;

        let mints = if let Some(ref config) = self.config {
            vec![
                ("USDC", config.usdc_mint.as_str()),
                ("SOL", config.sol_mint.as_str()),
                (
                    "USDT",
                    config.usdt_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
                ),
                (
                    "ETH",
                    config.eth_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
                ),
                (
                    "BTC",
                    config.btc_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
                ),
            ]
        } else {
            return Err(anyhow::anyhow!("Config not set for BalanceManager"));
        };

        log::info!("🔍 Subscribing to ATA balances via WebSocket...");

        let mut subscribed_count = 0;
        for (name, mint_str) in &mints {
            if mint_str.is_empty() {
                continue;
            }

            let mint = Pubkey::from_str(mint_str)
                .map_err(|e| anyhow::anyhow!("Invalid {} mint: {}", name, e))?;

            let ata = get_associated_token_address(&self.wallet, &mint, self.config.as_ref())
                .map_err(|e| anyhow::anyhow!("Failed to derive ATA for {}: {}", name, e))?;

            match ws.subscribe_account(&ata).await {
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
                                    timestamp: Instant::now(),
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
        let mut balances = self.balances.write().await;
        let subscribed_atas_check = self.subscribed_atas.read().await;
        if subscribed_atas_check
            .values()
            .any(|&ata| ata == update.pubkey)
        {
            balances.insert(
                update.pubkey,
                CachedBalance {
                    amount: balance,
                    timestamp: Instant::now(),
                },
            );

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
        let ws = self
            .ws
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("WebSocket client not set for BalanceManager"))?;

        let mut receiver = ws.subscribe_account_updates();
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

    pub async fn reserve(&self, mint: &Pubkey, amount: u64) -> Result<()> {
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())
            .with_context(|| format!("Failed to derive ATA for mint {}", mint))?;

        // Step 1: Fetch latest balance from RPC (no locks held to avoid blocking)
        let rpc_account_result = self.rpc.get_account(&ata).await;

        // Step 2: Decode balance
        let actual = match rpc_account_result {
            Ok(account) => {
                if account.data.len() < 72 {
                    return Err(anyhow::anyhow!("Invalid token account data"));
                }

                let balance_bytes: [u8; 8] = account.data[64..72]
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Failed to read balance from token account"))?;
                u64::from_le_bytes(balance_bytes)
            }
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("AccountNotFound") || error_msg.contains("account not found")
                {
                    // ATA doesn't exist - balance is 0
                    0
                } else {
                    return Err(e).context("Failed to fetch account balance during reserve");
                }
            }
        };

        // ✅ FIX: Hold BOTH locks simultaneously to prevent race condition
        // Lock order: balances -> reserved (consistent with other methods)
        let mut balances = self.balances.write().await;
        let mut reserved = self.reserved.write().await;

        // Step 3: Calculate new reserved amount BEFORE updating cache
        let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
        let new_reserved = reserved_amount.saturating_add(amount);

        // Step 4: Calculate available balance with NEW reserved amount
        let available = actual.saturating_sub(new_reserved);

        if available < amount {
            return Err(anyhow::anyhow!(
                "Insufficient balance for mint {}: need {}, available {} (balance: {}, reserved: {} -> {})",
                mint,
                amount,
                available,
                actual,
                reserved_amount,
                new_reserved
            ));
        }

        // Step 5: Update reserved FIRST (before cache update)
        reserved.insert(*mint, new_reserved);

        // Step 6: Update cache AFTER reserved is updated (prevents race condition)
        // Other threads reading cache will see correct reserved amount
        balances.insert(
            ata,
            CachedBalance {
                amount: actual,
                timestamp: Instant::now(),
            },
        );

        log::debug!(
            "BalanceManager: Reserved {} for mint {} (balance: {}, reserved: {} -> {}, available: {})",
            amount,
            mint,
            actual,
            reserved_amount,
            new_reserved,
            available
        );

        // Both locks released here atomically
        Ok(())
    }

    /// Release a previously reserved balance.
    ///
    /// This should be called exactly once for each successful `reserve` call
    /// for a given `(mint, amount)` pair.
    pub async fn release(&self, mint: &Pubkey, amount: u64) {
        let mut reserved = self.reserved.write().await;
        if let Some(reserved_amount) = reserved.get_mut(mint) {
            *reserved_amount = reserved_amount.saturating_sub(amount);
            if *reserved_amount == 0 {
                reserved.remove(mint);
            }
        }
    }

    pub async fn stop_monitoring(&self) -> Result<()> {
        let mut subscribed_atas = self.subscribed_atas.write().await;
        let mut balances = self.balances.write().await; // Hold both locks!

        let subscription_count = subscribed_atas.len();

        subscribed_atas.clear();

        balances.clear();

        log::info!(
            "BalanceManager: Stopped monitoring (cleared {} subscriptions and cache)",
            subscription_count
        );
        Ok(())
    }
}
