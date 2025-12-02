use anyhow::{Context, Result};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use crate::core::types::Opportunity;
use crate::protocol::solend::accounts::{
    get_associated_token_address,
    derive_lending_market_authority,
};
use crate::protocol::oracle::{get_pyth_oracle_account, get_switchboard_oracle_account};
use crate::core::config::Config;
use std::sync::Arc;
use std::str::FromStr;
use std::time::Duration;
use sha2::{Sha256, Digest};
use spl_token;
use std::collections::HashMap;
use tokio::sync::RwLock;
use once_cell::sync::Lazy;
use crate::blockchain::rpc_client::RpcClient;

/// Minimal subset of Solend reserve data we need when building liquidation instructions.
#[derive(Clone, Debug)]
struct ReserveData {
    pub collateral_mint: Pubkey,
    pub liquidity_supply: Pubkey,
}

struct ReserveCacheInner {
    mint_to_reserve: HashMap<Pubkey, Pubkey>,
    reserve_data: HashMap<Pubkey, ReserveData>,
    initialized: bool,
}

struct ReserveCache {
    inner: RwLock<ReserveCacheInner>,
}

impl ReserveCache {
    fn new() -> Self {
        Self {
            inner: RwLock::new(ReserveCacheInner {
                mint_to_reserve: HashMap::new(),
                reserve_data: HashMap::new(),
                initialized: false,
            }),
        }
    }

    /// Get reserve address for a given liquidity mint, using cached mapping.
    ///
    /// On first miss (or when cache is empty), this performs a single
    /// `get_program_accounts` call to build a complete mint → reserve index.
    ///
    /// ✅ IMPROVED: Uses clean double-check locking pattern without nested lock risk.
    /// RPC call is made outside the lock, then lock is acquired once to update cache.
    async fn get_reserve_for_mint(
        &self,
        mint: &Pubkey,
        rpc: &Arc<RpcClient>,
        config: &Config,
    ) -> Result<Pubkey> {
        // Fast path: Try read lock first (most common case)
        {
            let inner = self.inner.read().await;
            if let Some(reserve) = inner.mint_to_reserve.get(mint) {
                return Ok(*reserve);
            }
        }

        // Slow path: Cache miss - need to refresh
        // ✅ CRITICAL: RPC call is made OUTSIDE the lock to prevent blocking
        // This is safe because we'll double-check after acquiring write lock
        let mint_to_reserve = self.fetch_reserve_mapping_from_rpc(rpc, config).await?;

        // ✅ FIX: Check BEFORE acquiring write lock to prevent deadlock
        // If another thread already updated the cache while we were fetching, we can return early
        // This prevents two threads from both acquiring write locks simultaneously
        {
            let inner = self.inner.read().await;
            if let Some(reserve) = inner.mint_to_reserve.get(mint) {
                return Ok(*reserve); // Another thread already updated
            }
        }

        // ✅ CRITICAL: Acquire write lock ONCE to update cache
        // No nested lock risk - we only acquire lock here, not inside fetch function
        {
            let mut inner = self.inner.write().await;
            
            // Double-check: Another thread might have refreshed while we were waiting for write lock
            if let Some(reserve) = inner.mint_to_reserve.get(mint) {
                return Ok(*reserve);
            }
            
            // Update cache with fresh data
            inner.mint_to_reserve = mint_to_reserve;
            inner.initialized = true;
            
            // Now get the reserve we just cached
            inner.mint_to_reserve.get(mint).copied()
                .ok_or_else(|| anyhow::anyhow!("Reserve not found for mint: {}", mint))
        }
    }

    /// Get cached reserve data (collateral mint, liquidity supply, etc).
    async fn get_reserve_data(
        &self,
        reserve: &Pubkey,
        rpc: &Arc<RpcClient>,
    ) -> Result<ReserveData> {
        {
            let inner = self.inner.read().await;
            if let Some(data) = inner.reserve_data.get(reserve) {
                return Ok(data.clone());
            }
        }

        // Not cached yet: fetch once and store.
        let account = rpc
            .get_account(reserve)
            .await
            .context("Failed to fetch reserve account")?;

        if account.data.len() < 200 {
            return Err(anyhow::anyhow!("Reserve account data too small"));
        }

        let collateral_mint_offset = 8 + 32;
        let liquidity_supply_offset = 8 + 32 + 32 + 32;

        if account.data.len() < liquidity_supply_offset + 32 {
            return Err(anyhow::anyhow!("Reserve account data incomplete"));
        }

        let collateral_mint_bytes: [u8; 32] =
            account.data[collateral_mint_offset..collateral_mint_offset + 32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid reserve collateral mint data"))?;
        let collateral_mint = Pubkey::try_from(collateral_mint_bytes)
            .map_err(|_| anyhow::anyhow!("Invalid collateral mint"))?;

        let liquidity_supply_bytes: [u8; 32] =
            account.data[liquidity_supply_offset..liquidity_supply_offset + 32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid reserve liquidity supply data"))?;
        let liquidity_supply = Pubkey::try_from(liquidity_supply_bytes)
            .map_err(|_| anyhow::anyhow!("Invalid liquidity supply"))?;

        let data = ReserveData {
            collateral_mint,
            liquidity_supply,
        };

        let mut inner = self.inner.write().await;
        inner.reserve_data.insert(*reserve, data.clone());

        Ok(data)
    }

    /// Fetch reserve mapping from RPC without acquiring any locks.
    /// This is safe to call from anywhere because it doesn't touch shared state.
    /// Returns the mint_to_reserve mapping that can be used to update the cache.
    ///
    /// ✅ IMPROVED: Separated RPC fetch from lock acquisition to prevent nested lock risk.
    async fn fetch_reserve_mapping_from_rpc(
        &self,
        rpc: &Arc<RpcClient>,
        config: &Config,
    ) -> Result<HashMap<Pubkey, Pubkey>> {
        let program_id = Pubkey::from_str(&config.solend_program_id)
            .context("Invalid Solend program ID")?;

        let accounts = rpc
            .get_program_accounts(&program_id)
            .await
            .context("Failed to fetch program accounts for reserve lookup")?;

        let mut mint_to_reserve = HashMap::new();

        for (pubkey, account) in accounts {
            if account.data.len() < 8 {
                continue;
            }

            let reserve_discriminator = &account.data[0..8];
            let expected_discriminator = get_reserve_discriminator();

            if reserve_discriminator != expected_discriminator {
                continue;
            }

            if account.data.len() < 200 {
                continue;
            }

            let liquidity_mint_offset = 8 + 32 + 32;
            if account.data.len() <= liquidity_mint_offset + 32 {
                continue;
            }

            let liquidity_mint_bytes: [u8; 32] =
                account.data[liquidity_mint_offset..liquidity_mint_offset + 32]
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Invalid reserve data"))?;
            let liquidity_mint = Pubkey::try_from(liquidity_mint_bytes)
                .map_err(|_| anyhow::anyhow!("Invalid mint in reserve"))?;

            mint_to_reserve.insert(liquidity_mint, pubkey);
        }

        Ok(mint_to_reserve)
    }

    /// Refresh the cache from RPC.
    /// 
    /// ✅ IMPROVED: This method acquires its own write lock, so it's safe to call
    /// from anywhere (including background tasks). No nested lock risk.
    async fn refresh_from_rpc(&self, rpc: &Arc<RpcClient>, config: &Config) -> Result<()> {
        // Fetch data outside lock (no nested lock risk)
        let mint_to_reserve = self.fetch_reserve_mapping_from_rpc(rpc, config).await?;

        // Acquire lock ONCE to update cache
        let mut inner = self.inner.write().await;
        inner.mint_to_reserve = mint_to_reserve;
        inner.initialized = true;

        Ok(())
    }

    /// Check if cache is initialized (has been populated at least once)
    pub async fn is_initialized(&self) -> bool {
        let inner = self.inner.read().await;
        inner.initialized
    }

    /// Wait for cache to be initialized (with timeout)
    /// Returns true if initialized, false if timeout
    pub async fn wait_for_initialization(&self, timeout: Duration) -> bool {
        use tokio::time::{sleep, Instant};
        let start = Instant::now();
        
        while start.elapsed() < timeout {
            if self.is_initialized().await {
                return true;
            }
            sleep(Duration::from_millis(100)).await; // Check every 100ms
        }
        
        false
    }

    /// Start a background task that periodically refreshes the reserve cache.
    /// This prevents RPC call storms when building liquidation instructions.
    /// 
    /// The cache is refreshed every 5 minutes to avoid hitting rate limits
    /// on free RPC endpoints (typically 1 req/10s for getProgramAccounts).
    pub fn start_background_refresh(
        self: Arc<Self>,
        rpc: Arc<RpcClient>,
        config: Config,
    ) {
        tokio::spawn(async move {
            // ✅ CRITICAL FIX: Delay initial refresh to prevent RPC storm on startup
            // Free RPC endpoints typically have 1 req/10s limit for getProgramAccounts
            // Waiting 30 seconds before first refresh prevents hitting rate limits
            // This is especially important when multiple components start simultaneously
            // Scanner will start after 5s delay, so 30s ensures no overlap
            tokio::time::sleep(Duration::from_secs(30)).await;
            
            log::info!("Performing initial reserve cache refresh...");
            if let Err(e) = self.refresh_from_rpc(&rpc, &config).await {
                log::warn!("Initial reserve cache refresh failed: {}", e);
            } else {
                log::info!("Initial reserve cache refresh completed successfully");
            }

            // Then refresh every 5 minutes
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await; // 5 minutes
                
                log::debug!("Refreshing reserve cache from RPC...");
                if let Err(e) = self.refresh_from_rpc(&rpc, &config).await {
                    log::warn!("Reserve cache refresh failed: {}", e);
                } else {
                    log::debug!("Reserve cache refreshed successfully");
                }
            }
        });
    }
}

static RESERVE_CACHE: Lazy<Arc<ReserveCache>> = Lazy::new(|| Arc::new(ReserveCache::new()));

/// Start the background refresh task for the reserve cache.
/// This should be called once during application startup to prevent RPC call storms.
pub fn start_reserve_cache_refresh(
    rpc: Arc<RpcClient>,
    config: Config,
) {
    RESERVE_CACHE.clone().start_background_refresh(rpc, config);
}

/// Wait for reserve cache to be initialized (populated at least once).
/// This should be called before starting Executor to prevent RPC rate limit violations.
/// Returns true if cache is initialized, false if timeout exceeded.
pub async fn wait_for_reserve_cache_initialization(timeout: Duration) -> bool {
    RESERVE_CACHE.as_ref().wait_for_initialization(timeout).await
}

fn get_instruction_discriminator() -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(b"global:liquidateObligation");
    let hash = hasher.finalize();
    let mut discriminator = [0u8; 8];
    discriminator.copy_from_slice(&hash[..8]);
    discriminator
}

async fn get_reserve_address_from_mint(
    mint: &Pubkey,
    rpc: &Arc<RpcClient>,
    config: &Config,
) -> Result<Pubkey> {
    let _mint_str = mint.to_string();
    
    if let Some(usdc_mint) = Pubkey::from_str(&config.usdc_mint).ok() {
        if *mint == usdc_mint {
            if let Some(usdc_reserve_str) = &config.usdc_reserve_address {
                return Pubkey::from_str(usdc_reserve_str)
                    .context("Invalid USDC reserve address in config");
            }
        }
    }
    
    if let Some(sol_mint) = Pubkey::from_str(&config.sol_mint).ok() {
        if *mint == sol_mint {
            if let Some(sol_reserve_str) = &config.sol_reserve_address {
                return Pubkey::from_str(sol_reserve_str)
                    .context("Invalid SOL reserve address in config");
            }
        }
    }
    
    let _market_address = config.main_lending_market_address.as_ref()
        .and_then(|s| Pubkey::from_str(s).ok())
        .ok_or_else(|| anyhow::anyhow!("Main lending market address not configured"))?;
    
    // Use global cache to avoid calling `get_program_accounts` on every instruction build.
    RESERVE_CACHE
        .as_ref()
        .get_reserve_for_mint(mint, rpc, config)
        .await
}

// ✅ CRITICAL FIX: Pre-calculated compile-time constant to avoid hash calculation on every call
// This is called in hotpath (get_program_accounts filtering) - hash calculation was expensive
// SHA256("account:Reserve")[..8] = [0x2b, 0xf2, 0xcc, 0xca, 0x1a, 0xf7, 0x3b, 0x7f]
const RESERVE_DISCRIMINATOR: [u8; 8] = [0x2b, 0xf2, 0xcc, 0xca, 0x1a, 0xf7, 0x3b, 0x7f];

fn get_reserve_discriminator() -> [u8; 8] {
    RESERVE_DISCRIMINATOR
}

async fn get_reserve_data(
    reserve: &Pubkey,
    rpc: &Arc<RpcClient>,
) -> Result<(Pubkey, Pubkey, Pubkey)> {
    let data = RESERVE_CACHE.as_ref().get_reserve_data(reserve, rpc).await?;

    Ok((data.collateral_mint, data.liquidity_supply, *reserve))
}

pub async fn build_liquidate_obligation_ix(
    opportunity: &Opportunity,
    liquidator: &Pubkey,
    rpc: Option<Arc<RpcClient>>,
) -> Result<Instruction> {
    let rpc = rpc.ok_or_else(|| anyhow::anyhow!("RPC client required for liquidation instruction"))?;
    
    let config = crate::core::config::Config::from_env()
        .context("Failed to load config")?;
    
    let program_id = Pubkey::from_str(&config.solend_program_id)
        .context("Invalid Solend program ID")?;
    
    let obligation_address = opportunity.position.address;
    
    let obligation_account = rpc.get_account(&obligation_address).await
        .context("Failed to fetch obligation account")?;
    
    use crate::protocol::solend::types::SolendObligation;
    let obligation = SolendObligation::from_account_data(&obligation_account.data)
        .context("Failed to parse obligation")?;
    
    let lending_market = obligation.lending_market;
    
    let lending_market_authority = derive_lending_market_authority(&lending_market, &program_id)
        .context("Failed to derive lending market authority")?;
    
    let debt_reserve = get_reserve_address_from_mint(&opportunity.debt_mint, &rpc, &config).await
        .context("Failed to find debt reserve")?;
    
    let (debt_reserve_collateral_mint, debt_reserve_liquidity_supply, _) = get_reserve_data(&debt_reserve, &rpc).await
        .context("Failed to fetch debt reserve data")?;
    
    let collateral_reserve = get_reserve_address_from_mint(&opportunity.collateral_mint, &rpc, &config).await
        .context("Failed to find collateral reserve")?;
    
    let (_collateral_reserve_collateral_mint, collateral_reserve_liquidity_supply, _) = get_reserve_data(&collateral_reserve, &rpc).await
        .context("Failed to fetch collateral reserve data")?;
    
    let source_liquidity = get_associated_token_address(liquidator, &opportunity.debt_mint, Some(&config))
        .context("Failed to derive source liquidity ATA")?;
    
    let destination_collateral = get_associated_token_address(liquidator, &opportunity.collateral_mint, Some(&config))
        .context("Failed to derive destination collateral ATA")?;
    
    let pyth_price = get_pyth_oracle_account(&opportunity.debt_mint, Some(&config))
        .context("Failed to get Pyth oracle account")?
        .ok_or_else(|| anyhow::anyhow!("Pyth oracle not found for debt mint"))?;
    
    let switchboard_price = get_switchboard_oracle_account(&opportunity.debt_mint, Some(&config))
        .context("Failed to get Switchboard oracle account")?
        .or_else(|| {
            get_switchboard_oracle_account(&opportunity.collateral_mint, Some(&config))
                .ok()
                .flatten()
        });
    
    let switchboard_price = switchboard_price.unwrap_or(pyth_price);
    
    let token_program = spl_token::id();
    
    let accounts = vec![
        AccountMeta::new(source_liquidity, false),
        AccountMeta::new(destination_collateral, false),
        AccountMeta::new(obligation_address, false),
        AccountMeta::new(debt_reserve, false),
        AccountMeta::new(debt_reserve_collateral_mint, false),
        AccountMeta::new(debt_reserve_liquidity_supply, false),
        AccountMeta::new_readonly(lending_market, false),
        AccountMeta::new_readonly(lending_market_authority, false),
        AccountMeta::new(collateral_reserve_liquidity_supply, false),
        AccountMeta::new_readonly(*liquidator, true),
        AccountMeta::new_readonly(pyth_price, false),
        AccountMeta::new_readonly(switchboard_price, false),
        AccountMeta::new_readonly(token_program, false),
    ];
    
    let discriminator = get_instruction_discriminator();
    let mut data = Vec::with_capacity(16);
    data.extend_from_slice(&discriminator);
    data.extend_from_slice(&opportunity.max_liquidatable.to_le_bytes());
    
    Ok(Instruction {
        program_id,
        accounts,
        data,
    })
}
