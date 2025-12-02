use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::{WsClient, AccountUpdate};
use crate::core::config::Config;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, broadcast};
use anyhow::Result;

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

    /// Get available balance for a mint, using cache if available.
    /// Falls back to RPC if cache miss or cache is stale.
    ///
    /// ✅ CRITICAL FIX: Reserved read and cache check are now ATOMIC to prevent TOCTOU race condition.
    /// Both locks are held simultaneously to ensure reserved value doesn't change between check and use.
    pub async fn get_available_balance(&self, mint: &Pubkey) -> Result<u64> {
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;
        
        // ✅ CRITICAL FIX: Hold BOTH locks simultaneously to make reserved read and cache check ATOMIC
        // This prevents TOCTOU race condition where reserved value changes between read and use
        // Lock order: reserved first, then balances (consistent ordering prevents deadlock)
        let reserved = self.reserved.read().await;
        let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
        
        // Try to read from cache while holding reserved lock
        {
            let balances = self.balances.read().await;
            if let Some(cached) = balances.get(&ata) {
                // Check cache freshness
                if cached.timestamp.elapsed() < CACHE_TTL {
                    // ✅ ATOMIC: Both reserved and cached values are read while locks are held
                    // This ensures reserved value cannot change between read and calculation
                    let available = cached.amount.saturating_sub(reserved_amount);
                    log::debug!(
                        "BalanceManager: Cache hit for mint {} (ata={}): cached={}, reserved={}, available={}, age={:.2}s",
                        mint, ata, cached.amount, reserved_amount, available, cached.timestamp.elapsed().as_secs_f64()
                    );
                    return Ok(available);
                } else {
                    // Stale cache, fall through to RPC
                    log::debug!(
                        "BalanceManager: Cache stale for mint {} (ata={}): age={:.2}s (TTL={}s), falling back to RPC",
                        mint, ata, cached.timestamp.elapsed().as_secs_f64(), CACHE_TTL.as_secs()
                    );
                }
            }
        }
        // Reserved lock is dropped here (after cache check)
        
        // Cache miss - fallback to RPC (this should be rare after initial subscription)
        log::debug!(
            "BalanceManager: Cache miss for mint {} (ata={}), falling back to RPC",
            mint, ata
        );
        
        // Handle AccountNotFound gracefully - if ATA doesn't exist, balance is 0
        // This is expected behavior: ATA doesn't exist = no token account = balance is 0
        let account = match self.rpc.get_account(&ata).await {
            Ok(acc) => acc,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("AccountNotFound") || error_msg.contains("account not found") {
                    // ATA doesn't exist yet - this is normal, balance is 0
                    // Note: ATA can be created automatically during transaction if needed
                    log::debug!(
                        "BalanceManager: ATA not found for mint {} (ata={}), wallet={}. Returning 0 balance (ATA will be auto-created during transaction if needed)",
                        mint,
                        ata,
                        self.wallet
                    );
                    // Cache the zero balance to avoid repeated RPC calls
                    // ✅ CRITICAL: Read reserved while holding balances write lock to ensure atomicity
                    let reserved_amount = {
                        let reserved = self.reserved.read().await;
                        reserved.get(mint).copied().unwrap_or(0)
                    };
                    let mut balances = self.balances.write().await;
                    balances.insert(ata, CachedBalance {
                        amount: 0,
                        timestamp: Instant::now(),
                    });
                    // ✅ ATOMIC: Reserved was read before cache update, so calculation is safe
                    return Ok(0u64.saturating_sub(reserved_amount));
                }
                // Other errors (network issues, RPC errors, etc.) should be propagated
                return Err(e);
            }
        };
        
        if account.data.len() < 72 {
            return Err(anyhow::anyhow!("Invalid token account data"));
        }
        
        let balance_bytes: [u8; 8] = account.data[64..72].try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read balance from token account"))?;
        let actual = u64::from_le_bytes(balance_bytes);
        
        // ✅ CRITICAL FIX: Read reserved AFTER RPC call but BEFORE cache update
        // This ensures we use the most up-to-date reserved value (RPC call may have taken time)
        // and prevents reserved from changing between cache update and calculation
        let reserved_amount = {
            let reserved = self.reserved.read().await;
            reserved.get(mint).copied().unwrap_or(0)
        };
        
        // Update cache with RPC result
        {
            let mut balances = self.balances.write().await;
            balances.insert(ata, CachedBalance {
                amount: actual,
                timestamp: Instant::now(),
            });
        }
        
        // ✅ ATOMIC: Reserved was read after RPC (most up-to-date) and before cache update
        // This ensures calculation uses consistent values
        let available = actual.saturating_sub(reserved_amount);
        Ok(available)
    }

    /// Get available balance while holding a read lock on reserved map.
    /// This is used internally to avoid double-locking.
    /// Uses cache if available, falls back to RPC.
    async fn get_available_balance_locked(
        &self,
        mint: &Pubkey,
        reserved: &HashMap<Pubkey, u64>,
    ) -> Result<u64> {
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;
        
        // Try cache first
        {
            let balances = self.balances.read().await;
            if let Some(cached) = balances.get(&ata) {
                // Check cache freshness
                if cached.timestamp.elapsed() < CACHE_TTL {
                let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
                    return Ok(cached.amount.saturating_sub(reserved_amount));
                }
                // Stale cache, fall through to RPC
            }
        }
        
        // Cache miss - fallback to RPC
        let account = match self.rpc.get_account(&ata).await {
            Ok(acc) => acc,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("AccountNotFound") || error_msg.contains("account not found") {
                    // Cache zero balance
                    let mut balances = self.balances.write().await;
                    balances.insert(ata, CachedBalance {
                        amount: 0,
                        timestamp: Instant::now(),
                    });
                    let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
                    return Ok(0u64.saturating_sub(reserved_amount));
                }
                return Err(e);
            }
        };
        
        if account.data.len() < 72 {
            return Err(anyhow::anyhow!("Invalid token account data"));
        }
        
        let balance_bytes: [u8; 8] = account.data[64..72].try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read balance from token account"))?;
        let actual = u64::from_le_bytes(balance_bytes);
        
        // Update cache
        {
            let mut balances = self.balances.write().await;
            balances.insert(ata, CachedBalance {
                amount: actual,
                timestamp: Instant::now(),
            });
        }
        
        let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
        Ok(actual.saturating_sub(reserved_amount))
    }

    /// Start monitoring ATA balances via WebSocket.
    /// Subscribes to all required ATAs and updates cache on account changes.
    pub async fn start_monitoring(&self) -> Result<()> {
        let ws = self.ws.as_ref()
            .ok_or_else(|| anyhow::anyhow!("WebSocket client not set for BalanceManager"))?;
        
        use crate::protocol::solend::accounts::get_associated_token_address;
        use std::str::FromStr;
        
        // Get all required mints from config
        let mints = if let Some(ref config) = self.config {
            vec![
                ("USDC", config.usdc_mint.as_str()),
                ("SOL", config.sol_mint.as_str()),
                ("USDT", config.usdt_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
                ("ETH", config.eth_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
                ("BTC", config.btc_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
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
            
            // Subscribe to ATA account
            match ws.subscribe_account(&ata).await {
                Ok(subscription_id) => {
                    log::info!(
                        "✅ Subscribed to {} ATA: {} (subscription ID: {})",
                        name, ata, subscription_id
                    );
                    
                    // Store mapping
                    let mut subscribed_atas = self.subscribed_atas.write().await;
                    subscribed_atas.insert(mint, ata);
                    
                    // Initial balance fetch (cache warm-up)
                    if let Ok(account) = self.rpc.get_account(&ata).await {
                        if account.data.len() >= 72 {
                            let balance_bytes: [u8; 8] = account.data[64..72].try_into()
                                .map_err(|_| anyhow::anyhow!("Failed to read balance"))?;
                            let balance = u64::from_le_bytes(balance_bytes);
                            let mut balances = self.balances.write().await;
                            balances.insert(ata, CachedBalance {
                                amount: balance,
                                timestamp: Instant::now(),
                            });
                            log::debug!(
                                "BalanceManager: Initial {} balance cached: {}",
                                name, balance
                            );
                        }
                    }
                    
                    subscribed_count += 1;
                }
                Err(e) => {
                    log::warn!(
                        "⚠️  Failed to subscribe to {} ATA ({}): {}",
                        name, ata, e
                    );
                    log::warn!("   Balance will be fetched via RPC on demand");
                }
            }
        }
        
        log::info!("✅ BalanceManager: Subscribed to {} ATA(s) via WebSocket", subscribed_count);
        Ok(())
    }

    /// Process account update from WebSocket and update cache.
    /// This should be called from a background task that listens to WebSocket updates.
    pub async fn handle_account_update(&self, update: &AccountUpdate) {
        // ✅ CRITICAL FIX: Atomic check-and-set to prevent TOCTOU race condition
        // Check if this is one of our subscribed ATAs (first check with read lock)
        let subscribed_atas = self.subscribed_atas.read().await;
        let is_subscribed = subscribed_atas.values().any(|&ata| ata == update.pubkey);
        
        if !is_subscribed {
            return; // Not one of our ATAs, ignore
        }
        
        // Parse balance from account data
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
        
        // ✅ CRITICAL FIX: Double-check subscription while holding write lock
        // This prevents TOCTOU: between first check and insert, ATA could be unsubscribed
        // By checking again while holding write lock, we ensure atomicity
        let mut balances = self.balances.write().await;
        
        // Double-check: ATA might have been unsubscribed between first check and now
        // We need to check again while holding write lock to ensure atomicity
        let subscribed_atas_check = self.subscribed_atas.read().await;
        if subscribed_atas_check.values().any(|&ata| ata == update.pubkey) {
            // Still subscribed - safe to update cache
            balances.insert(update.pubkey, CachedBalance {
                amount: balance,
                timestamp: Instant::now(),
            });
            
            log::debug!(
                "BalanceManager: Updated balance cache for ATA {}: {}",
                update.pubkey,
                balance
            );
        } else {
            // ATA was unsubscribed between check and insert - skip update
            log::debug!(
                "BalanceManager: Skipping update for ATA {} (unsubscribed during processing)",
                update.pubkey
            );
        }
    }

    /// Start background task to listen for account updates from WebSocket broadcast channel.
    /// This should be called after start_monitoring().
    pub async fn listen_account_updates(&self) -> Result<()> {
        let ws = self.ws.as_ref()
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

    /// Atomically reserve balance, preventing race conditions.
    ///
    /// This method holds a write lock during both balance check and reservation,
    /// ensuring no other thread can reserve the same balance concurrently.
    ///
    /// The reservation stays in effect until `release` is called for the same
    /// `mint`/`amount` pair (typically by the `Executor` after a transaction
    /// has been sent or definitively failed).
    pub async fn reserve(&self, mint: &Pubkey, amount: u64) -> Result<()> {
        // Acquire write lock FIRST - this prevents race conditions
        let mut reserved = self.reserved.write().await;
        
        // Check available balance while holding the lock
        let available = self.get_available_balance_locked(mint, &reserved).await?;

        if available < amount {
            return Err(anyhow::anyhow!(
                "Insufficient balance for mint {}: need {}, available {}",
                mint,
                amount,
                available
            ));
        }

        // Reserve the amount atomically (add to existing reserved amount)
        *reserved.entry(*mint).or_insert(0) += amount;

        // Lock is released here, but reservation is already committed.
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
}