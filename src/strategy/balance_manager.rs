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
/// ✅ FIX: Increased from 30s to 60s to reduce unnecessary RPC calls
/// WebSocket updates should keep cache fresh, but longer TTL provides buffer
const CACHE_TTL: Duration = Duration::from_secs(60);

/// BalanceManager manages token balances and reservations.
///
/// # CRITICAL: Lock Ordering to Prevent Deadlocks
///
/// When acquiring multiple locks, ALWAYS acquire them in this order:
/// 1. `balances` (read or write)
/// 2. `reserved` (read or write)
///
/// This order MUST be consistent across ALL functions to prevent deadlocks.
/// If you need both locks, acquire `balances` first, then `reserved`.
///
/// Example:
/// ```rust
/// // ✅ CORRECT: balances -> reserved
/// let balances = self.balances.read().await;
/// let reserved = self.reserved.read().await;
///
/// // ❌ WRONG: reserved -> balances (DEADLOCK RISK)
/// let reserved = self.reserved.read().await;
/// let balances = self.balances.read().await;
/// ```
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
    // CRITICAL: Lock Ordering Rules
    //
    // When acquiring multiple locks, ALWAYS acquire them in this order:
    // 1. balances (read or write)
    // 2. reserved (read or write)
    //
    // This order MUST be consistent across ALL functions to prevent deadlocks.
    // If you need both locks, acquire balances first, then reserved.
    //
    // Functions that only need one lock are safe (e.g., release() only locks reserved).

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
        // ✅ FIX: Solend uses WSOL (Wrapped SOL), not native SOL
        // Problem: Previous code read native SOL balance, but Solend requires WSOL in ATA
        // Solution: For SOL/WSOL, read WSOL ATA balance instead of native SOL balance
        //   Native SOL must be wrapped to WSOL before use in Solend protocol
        let config = self.config.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Config not set for BalanceManager"))?;
        use std::str::FromStr;
        use crate::protocol::solend::instructions::is_wsol_mint;
        
        // Check if this is WSOL (Solend uses WSOL for SOL)
        // Note: config.sol_mint should be WSOL mint address
        let sol_mint = Pubkey::from_str(&config.sol_mint)
            .map_err(|_| anyhow::anyhow!("Failed to parse SOL mint address from config"))?;
        
        if mint == &sol_mint || is_wsol_mint(mint) {
            // WSOL: read from WSOL ATA (same as other SPL tokens)
            use crate::protocol::solend::accounts::get_associated_token_address;
            let wsol_ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())
                .context("Failed to derive WSOL ATA")?;
            
            // Check cache first
            {
                let balances = self.balances.read().await;
                if let Some(cached) = balances.get(&wsol_ata) {
                    if cached.timestamp.elapsed() < CACHE_TTL {
                        let reserved_amount = {
                            let reserved = self.reserved.read().await;
                            reserved.get(mint).copied().unwrap_or(0)
                        };
                        let available = cached.amount.saturating_sub(reserved_amount);
                        log::debug!(
                            "BalanceManager: WSOL balance (cached): ata={}, balance={}, reserved={}, available={}",
                            wsol_ata, cached.amount, reserved_amount, available
                        );
                        return Ok(available);
                    }
                }
            }
            
            // Cache miss or stale - fetch from RPC using helper function
            // ✅ FIX: Use helper function to read ATA balance (Problems.md recommendation)
            use crate::utils::helpers::read_ata_balance;
            let wsol_balance = match read_ata_balance(&wsol_ata, &self.rpc).await {
                Ok(balance) => balance,
                Err(e) => {
                    // RPC error (not AccountNotFound) - propagate error
                    return Err(e).context("Failed to fetch WSOL ATA account");
                }
            };
            
            // Update cache
            // ✅ DEADLOCK PREVENTION: Lock order balances -> reserved (consistent order)
            let reserved_amount = {
                // Acquire both locks in correct order: balances -> reserved
                let _balances = self.balances.read().await; // Acquire first to maintain lock order
                let reserved = self.reserved.read().await;
                reserved.get(mint).copied().unwrap_or(0)
            };
            
            // Update cache after releasing locks (short-lived write lock)
            {
                let mut balances = self.balances.write().await;
                balances.insert(
                    wsol_ata,
                    CachedBalance {
                        amount: wsol_balance,
                        timestamp: Instant::now(),
                    },
                );
            }
            
            let available = wsol_balance.saturating_sub(reserved_amount);
            log::debug!(
                "BalanceManager: WSOL balance (RPC): ata={}, balance={}, reserved={}, available={}",
                wsol_ata, wsol_balance, reserved_amount, available
            );
            return Ok(available);
        }
        
        // SPL tokens: use ATA
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;

        {
            // ✅ DEADLOCK PREVENTION: Lock order balances -> reserved (consistent with reserve())
            let balances = self.balances.read().await;
            if let Some(cached) = balances.get(&ata) {
                if cached.timestamp.elapsed() < CACHE_TTL {
                    // Read reserved amount without holding it across other locks/RPC calls
                    // Lock order: balances (already held) -> reserved (consistent order)
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

        // ✅ FIX: Use helper function to read ATA balance (Problems.md recommendation)
        use crate::utils::helpers::read_ata_balance;
        let actual = match read_ata_balance(&ata, &self.rpc).await {
            Ok(balance) => balance,
            Err(e) => {
                // RPC error (not AccountNotFound) - propagate error
                return Err(e).context("Failed to fetch token account balance");
            }
        };

        // Update cache with RPC result
        // ✅ DEADLOCK PREVENTION: Lock order balances -> reserved (consistent with reserve())
        // Must acquire both locks in consistent order, even if balances lock is released between
        let reserved_amount = {
            // Lock order: balances -> reserved (ALWAYS in this order to prevent deadlock)
            // Acquire balances lock first (even if we don't need to modify it here)
            let _balances = self.balances.read().await;
            let reserved = self.reserved.read().await;
            reserved.get(mint).copied().unwrap_or(0)
        };

        // Update cache after releasing locks (short-lived write lock)
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

        let available = actual.saturating_sub(reserved_amount);
        Ok(available)
    }

    pub async fn get_available_balance_locked(
        &self,
        mint: &Pubkey,
        reserved: &HashMap<Pubkey, u64>,
    ) -> Result<u64> {
        // ✅ FIX: Solend uses WSOL (Wrapped SOL), not native SOL
        // For SOL/WSOL, read WSOL ATA balance instead of native SOL balance
        let config = self.config.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Config not set for BalanceManager"))?;
        use std::str::FromStr;
        use crate::protocol::solend::instructions::is_wsol_mint;
        
        let sol_mint = Pubkey::from_str(&config.sol_mint)
            .map_err(|_| anyhow::anyhow!("Failed to parse SOL mint address from config"))?;
        
        if mint == &sol_mint || is_wsol_mint(mint) {
            // WSOL: read from WSOL ATA
            use crate::protocol::solend::accounts::get_associated_token_address;
            let wsol_ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())
                .context("Failed to derive WSOL ATA")?;
            
            // Check cache first
            {
                let balances = self.balances.read().await;
                if let Some(cached) = balances.get(&wsol_ata) {
                    if cached.timestamp.elapsed() < CACHE_TTL {
                        let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
                        return Ok(cached.amount.saturating_sub(reserved_amount));
                    }
                }
            }
            
            // Cache miss or stale - fetch from RPC using helper function
            // ✅ FIX: Use helper function to read ATA balance (Problems.md recommendation)
            use crate::utils::helpers::read_ata_balance;
            let wsol_balance = match read_ata_balance(&wsol_ata, &self.rpc).await {
                Ok(balance) => balance,
                Err(e) => {
                    // RPC error (not AccountNotFound) - propagate error
                    return Err(e).context("Failed to fetch WSOL ATA account");
                }
            };
            
            // Update cache
            {
                let mut balances = self.balances.write().await;
                balances.insert(
                    wsol_ata,
                    CachedBalance {
                        amount: wsol_balance,
                        timestamp: Instant::now(),
                    },
                );
            }
            
            let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
            return Ok(wsol_balance.saturating_sub(reserved_amount));
        }
        
        // SPL tokens: use ATA
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;

        {
            // ✅ DEADLOCK PREVENTION: Lock order balances -> reserved (consistent with reserve())
            // Note: reserved is passed as parameter (already locked by caller), so we only lock balances
            let balances = self.balances.read().await;
            if let Some(cached) = balances.get(&ata) {
                if cached.timestamp.elapsed() < CACHE_TTL {
                    let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
                    return Ok(cached.amount.saturating_sub(reserved_amount));
                }
                // Stale cache, fall through to RPC
            }
        }

        // ✅ FIX: Use helper function to read ATA balance (Problems.md recommendation)
        use crate::utils::helpers::read_ata_balance;
        let actual = match read_ata_balance(&ata, &self.rpc).await {
            Ok(balance) => balance,
            Err(e) => {
                // RPC error (not AccountNotFound) - propagate error
                return Err(e).context("Failed to fetch token account balance");
            }
        };

        // Update cache
        // ✅ DEADLOCK PREVENTION: Lock order balances -> reserved (consistent with reserve())
        // Note: reserved is passed as parameter (already locked by caller), so we only lock balances
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
        // ✅ FIX: Read SOL mint from config (Solend uses native SOL, not wrapped SOL)
        let sol_mint = Pubkey::from_str(&self.config.as_ref().unwrap().sol_mint)
            .map_err(|_| anyhow::anyhow!("Failed to parse SOL mint address from config"))?;
        
        for (name, mint_str) in &mints {
            if mint_str.is_empty() {
                continue;
            }

            let mint = Pubkey::from_str(mint_str)
                .map_err(|e| anyhow::anyhow!("Invalid {} mint: {}", name, e))?;

            // ✅ FIX: Solend uses WSOL, subscribe to WSOL ATA instead of native SOL wallet
            use crate::protocol::solend::instructions::is_wsol_mint;
            if mint == sol_mint || is_wsol_mint(&mint) {
                // WSOL: subscribe to WSOL ATA (same as other SPL tokens)
                let wsol_ata = get_associated_token_address(&self.wallet, &mint, self.config.as_ref())
                    .map_err(|e| anyhow::anyhow!("Failed to derive WSOL ATA for {}: {}", name, e))?;
                
                match ws.subscribe_account(&wsol_ata).await {
                    Ok(subscription_id) => {
                        log::info!(
                            "✅ Subscribed to {} WSOL ATA: {} (subscription ID: {})",
                            name,
                            wsol_ata,
                            subscription_id
                        );

                        let mut subscribed_atas = self.subscribed_atas.write().await;
                        subscribed_atas.insert(mint, wsol_ata);

                        // Read initial WSOL balance from ATA
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
                                        timestamp: Instant::now(),
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

            // SPL tokens: use ATA
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

        // ✅ FIX: WSOL updates come from WSOL ATA, not wallet account
        // WSOL balance is in token account data, same as other SPL tokens
        // (No special handling needed for wallet account - WSOL uses ATA)

        // SPL tokens: balance is in token account data
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
        // ✅ FIX: SOL is native, not an SPL token - read balance from wallet account, not ATA
        // Solend protocol uses native SOL directly, not wrapped SOL
        let config = self.config.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Config not set for BalanceManager"))?;
        use std::str::FromStr;
        let sol_mint = Pubkey::from_str(&config.sol_mint)
            .map_err(|_| anyhow::anyhow!("Failed to parse SOL mint address from config"))?;
        
        // ✅ FIX: WSOL uses ATA, same as other SPL tokens
        use crate::protocol::solend::instructions::is_wsol_mint;
        let ata = if mint == &sol_mint || is_wsol_mint(mint) {
            // WSOL: derive WSOL ATA
            use crate::protocol::solend::accounts::get_associated_token_address;
            get_associated_token_address(&self.wallet, mint, self.config.as_ref())
                .with_context(|| format!("Failed to derive WSOL ATA for mint {}", mint))?
        } else {
            // SPL tokens: derive ATA
            use crate::protocol::solend::accounts::get_associated_token_address;
            get_associated_token_address(&self.wallet, mint, self.config.as_ref())
                .with_context(|| format!("Failed to derive ATA for mint {}", mint))?
        };

        // ✅ CRITICAL FIX: Race condition prevention
        // Problem: Previous implementation had race condition between balance read and reserve:
        //   - Thread A reads balance X (no locks)
        //   - Thread B reads balance X (no locks)  
        //   - Thread A reserves using X
        //   - Thread B reserves using X
        //   - Both pass check even if total reserved exceeds actual balance
        // Solution: 
        //   1. Quick check cache without locks (optimization)
        //   2. Acquire BOTH locks in consistent order (balances -> reserved)
        //   3. Re-read balance from cache (or fetch via RPC if stale) WHILE holding locks
        //   4. Check and reserve atomically
        // This ensures balance check and reserve update are atomic - no other thread
        // can modify balance or reserved amounts between our read and reserve.
        //
        // ✅ DEADLOCK PREVENTION: Lock order MUST be consistent across all functions
        // Lock order: balances -> reserved (ALWAYS in this order to prevent deadlock)
        
        // Step 1: Quick check cache without locks (optimization - not critical for correctness)
        let needs_rpc = {
            let balances = self.balances.read().await;
            balances.get(&ata).map_or(true, |c| c.timestamp.elapsed() >= CACHE_TTL)
        };

        // Step 2: Acquire BOTH locks in consistent order and read balance
        if !needs_rpc {
            // Cache is fresh - acquire locks and read from cache
            let balances = self.balances.read().await;
            let mut reserved = self.reserved.write().await;
            
            let actual = balances.get(&ata)
                .map(|c| c.amount)
                .ok_or_else(|| anyhow::anyhow!("Cache entry disappeared for ATA {}", ata))?;
            
            // Check and reserve atomically (we're holding both locks)
            let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
            let new_reserved = reserved_amount.saturating_add(amount);
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

            // Update reserved
            reserved.insert(*mint, new_reserved);

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
            return Ok(());
        } else {
            // Cache is stale or missing - need RPC call
            // ⚠️ CRITICAL: We must drop locks before RPC call to avoid blocking other threads
            // But we'll re-acquire them in the same order after RPC call
            
            // RPC call WITHOUT locks (to avoid blocking)
            use crate::utils::helpers::read_ata_balance;
            let balance = read_ata_balance(&ata, &self.rpc)
                .await
                .context("Failed to fetch account balance during reserve")?;
            
            // Re-acquire locks in SAME order (balances -> reserved)
            let mut balances = self.balances.write().await; // Write lock to update cache
            let mut reserved = self.reserved.write().await;
            
            // Update cache
            balances.insert(
                ata,
                CachedBalance {
                    amount: balance,
                    timestamp: Instant::now(),
                },
            );
            
            // Check and reserve atomically (we're holding both locks)
            let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
            let new_reserved = reserved_amount.saturating_add(amount);
            let available = balance.saturating_sub(new_reserved);

            if available < amount {
                return Err(anyhow::anyhow!(
                    "Insufficient balance for mint {}: need {}, available {} (balance: {}, reserved: {} -> {})",
                    mint,
                    amount,
                    available,
                    balance,
                    reserved_amount,
                    new_reserved
                ));
            }

            // Update reserved
            reserved.insert(*mint, new_reserved);

            log::debug!(
                "BalanceManager: Reserved {} for mint {} (balance: {}, reserved: {} -> {}, available: {})",
                amount,
                mint,
                balance,
                reserved_amount,
                new_reserved,
                available
            );

            // Both locks released here atomically
            return Ok(());
        }
    }

    /// Internal helper: Reserve balance assuming we already know the actual balance
    /// This ensures consistent lock ordering: balances -> reserved
    async fn reserve_with_balance(&self, mint: &Pubkey, amount: u64, actual: u64) -> Result<()> {
        // ✅ DEADLOCK PREVENTION: Lock order MUST be consistent
        // Lock order: balances -> reserved (ALWAYS in this order)
        // Note: Using read lock for balances since we don't modify it (cache already updated)
        //       but we still acquire it first to maintain consistent lock ordering
        let _balances = self.balances.read().await; // Read-only, but acquired first for lock order
        let mut reserved = self.reserved.write().await;

        // Calculate new reserved amount
        let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
        let new_reserved = reserved_amount.saturating_add(amount);

        // Calculate available balance with NEW reserved amount
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

        // Update reserved
        reserved.insert(*mint, new_reserved);

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
    ///
    /// ✅ DEADLOCK PREVENTION: This function only locks `reserved`, not `balances`.
    /// This is safe because it doesn't need to access balances cache.
    /// If you need both locks, use the order: balances -> reserved (consistent with reserve())
    pub async fn release(&self, mint: &Pubkey, amount: u64) {
        // Only lock reserved (no balances lock needed, so no deadlock risk)
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
