pub mod balance_cache;
pub mod reservation;
pub mod balance_subscription;

pub use balance_cache::{BalanceCache, CachedBalance, CACHE_TTL};
pub use reservation::ReservationManager;
pub use balance_subscription::BalanceSubscription;

use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::ws_client::WsClient;
use crate::core::config::Config;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::sync::RwLock;

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
pub struct BalanceManager {
    reservation: ReservationManager,
    cache: BalanceCache,
    subscription: Option<BalanceSubscription>,
    rpc: Arc<RpcClient>,
    wallet: Pubkey,
    config: Option<Config>,
}

impl BalanceManager {
    pub fn new(rpc: Arc<RpcClient>, wallet: Pubkey) -> Self {
        BalanceManager {
            reservation: ReservationManager::new(),
            cache: BalanceCache::new(),
            subscription: None,
            rpc,
            wallet,
            config: None,
        }
    }

    pub fn with_config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub fn with_websocket(mut self, ws: Arc<WsClient>) -> Self {
        let config = self.config.clone().expect("Config must be set before WebSocket");
        // Share the same cache instance with BalanceManager
        let subscription = BalanceSubscription::new(
            self.cache.balances(),
            Arc::clone(&self.rpc),
            ws,
            self.wallet,
            config,
        );
        self.subscription = Some(subscription);
        self
    }

    pub async fn get_available_balance(&self, mint: &Pubkey) -> Result<u64> {
        let config = self.config.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Config not set for BalanceManager"))?;
        use std::str::FromStr;
        use crate::protocol::solend::instructions::is_wsol_mint;
        
        let sol_mint = Pubkey::from_str(&config.sol_mint)
            .map_err(|_| anyhow::anyhow!("Failed to parse SOL mint address from config"))?;
        
        if mint == &sol_mint || is_wsol_mint(mint) {
            use crate::protocol::solend::accounts::get_associated_token_address;
            let wsol_ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())
                .context("Failed to derive WSOL ATA")?;
            
            {
                let balances_arc = self.cache.balances();
                let balances = balances_arc.read().await;
                if let Some(cached) = balances.get(&wsol_ata) {
                    if cached.timestamp.elapsed() < CACHE_TTL {
                        let reserved_amount = {
                            let reserved_arc = self.reservation.reserved();
                            let reserved = reserved_arc.read().await;
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
            
            use crate::utils::helpers::read_ata_balance;
            let wsol_balance = match read_ata_balance(&wsol_ata, &self.rpc).await {
                Ok(balance) => balance,
                Err(e) => {
                    return Err(e).context("Failed to fetch WSOL ATA account");
                }
            };
            
            let reserved_amount = {
                let balances_arc = self.cache.balances();
                let _balances = balances_arc.read().await;
                let reserved_arc = self.reservation.reserved();
                let reserved = reserved_arc.read().await;
                reserved.get(mint).copied().unwrap_or(0)
            };
            
            self.cache.insert(wsol_ata, wsol_balance).await;
            
            let available = wsol_balance.saturating_sub(reserved_amount);
            log::debug!(
                "BalanceManager: WSOL balance (RPC): ata={}, balance={}, reserved={}, available={}",
                wsol_ata, wsol_balance, reserved_amount, available
            );
            return Ok(available);
        }
        
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;

        {
            let balances_arc = self.cache.balances();
            let balances = balances_arc.read().await;
            if let Some(cached) = balances.get(&ata) {
                if cached.timestamp.elapsed() < CACHE_TTL {
                    let reserved_amount = {
                        let reserved_arc = self.reservation.reserved();
                        let reserved = reserved_arc.read().await;
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

        use crate::utils::helpers::read_ata_balance;
        let actual = match read_ata_balance(&ata, &self.rpc).await {
            Ok(balance) => balance,
            Err(e) => {
                return Err(e).context("Failed to fetch token account balance");
            }
        };

        let reserved_amount = {
            let balances_arc = self.cache.balances();
            let _balances = balances_arc.read().await;
            let reserved_arc = self.reservation.reserved();
            let reserved = reserved_arc.read().await;
            reserved.get(mint).copied().unwrap_or(0)
        };

        self.cache.insert(ata, actual).await;

        let available = actual.saturating_sub(reserved_amount);
        Ok(available)
    }

    pub async fn reserve(&self, mint: &Pubkey, amount: u64) -> Result<()> {
        let config = self.config.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Config not set for BalanceManager"))?;
        use std::str::FromStr;
        let sol_mint = Pubkey::from_str(&config.sol_mint)
            .map_err(|_| anyhow::anyhow!("Failed to parse SOL mint address from config"))?;
        
        use crate::protocol::solend::instructions::is_wsol_mint;
        let ata = if mint == &sol_mint || is_wsol_mint(mint) {
            use crate::protocol::solend::accounts::get_associated_token_address;
            get_associated_token_address(&self.wallet, mint, self.config.as_ref())
                .with_context(|| format!("Failed to derive WSOL ATA for mint {}", mint))?
        } else {
            use crate::protocol::solend::accounts::get_associated_token_address;
            get_associated_token_address(&self.wallet, mint, self.config.as_ref())
                .with_context(|| format!("Failed to derive ATA for mint {}", mint))?
        };

        let needs_rpc = {
            let balances_arc = self.cache.balances();
            let balances = balances_arc.read().await;
            balances.get(&ata).map_or(true, |c| c.timestamp.elapsed() >= CACHE_TTL)
        };

        if !needs_rpc {
            let balances_arc = self.cache.balances();
            let balances = balances_arc.read().await;
            let reserved_arc = self.reservation.reserved();
            let mut reserved = reserved_arc.write().await;
            
            let actual = balances.get(&ata)
                .map(|c| c.amount)
                .ok_or_else(|| anyhow::anyhow!("Cache entry disappeared for ATA {}", ata))?;
            
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

            return Ok(());
        } else {
            use crate::utils::helpers::read_ata_balance;
            let balance = read_ata_balance(&ata, &self.rpc)
                .await
                .context("Failed to fetch account balance during reserve")?;
            
            let balances_arc = self.cache.balances();
            let _balances = balances_arc.read().await;
            let reserved_arc = self.reservation.reserved();
            let mut reserved = reserved_arc.write().await;
            
            self.cache.insert(ata, balance).await;
            
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

            return Ok(());
        }
    }

    pub async fn release(&self, mint: &Pubkey, amount: u64) {
        self.reservation.release(mint, amount).await;
    }

    // WebSocket subscription methods - delegate to BalanceSubscription
    pub async fn start_monitoring(&self) -> Result<()> {
        self.subscription
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("WebSocket client not set for BalanceManager"))?
            .start_monitoring()
            .await
    }

    pub async fn listen_account_updates(&self) -> Result<()> {
        self.subscription
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("WebSocket client not set for BalanceManager"))?
            .listen_account_updates()
            .await
    }

    pub async fn stop_monitoring(&self) -> Result<()> {
        self.subscription
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("WebSocket client not set for BalanceManager"))?
            .stop_monitoring()
            .await
    }

}
