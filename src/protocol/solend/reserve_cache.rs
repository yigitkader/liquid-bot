// Reserve cache module - caches reserve data to reduce RPC calls

use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use crate::utils::error_helpers::{retry_with_backoff, RetryConfig};
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Clone, Debug)]
pub struct ReserveData {
    pub collateral_mint: Pubkey,
    pub liquidity_supply: Pubkey,
    pub loan_to_value_ratio: f64, // LTV as percentage (0.0-1.0)
}

struct ReserveCacheInner {
    mint_to_reserve: HashMap<Pubkey, Pubkey>,
    reserve_data: HashMap<Pubkey, ReserveData>,
    initialized: bool,
}

// ✅ FIX: Fetch lock to prevent multiple threads from making concurrent RPC calls
static FETCH_LOCK: Lazy<Arc<tokio::sync::Mutex<()>>> = Lazy::new(|| Arc::new(tokio::sync::Mutex::new(())));

pub struct ReserveCache {
    inner: RwLock<ReserveCacheInner>,
}

impl ReserveCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(ReserveCacheInner {
                mint_to_reserve: HashMap::new(),
                reserve_data: HashMap::new(),
                initialized: false,
            }),
        }
    }

    pub async fn get_reserve_for_mint(
        &self,
        mint: &Pubkey,
        rpc: &Arc<RpcClient>,
        config: &Config,
    ) -> Result<Pubkey> {
        {
            let inner = self.inner.read().await;
            if let Some(reserve) = inner.mint_to_reserve.get(mint) {
                return Ok(*reserve);
            }
        }

        let _lock = FETCH_LOCK.lock().await;
        
        {
            let inner = self.inner.read().await;
            if let Some(reserve) = inner.mint_to_reserve.get(mint) {
                return Ok(*reserve);
            }
        }

        let mint_to_reserve = self.fetch_reserve_mapping_from_rpc(rpc, config).await?;
        
        {
            let mut inner = self.inner.write().await;
            inner.mint_to_reserve.extend(mint_to_reserve);
            inner.initialized = true;
        }
        
        {
            let inner = self.inner.read().await;
            if let Some(reserve) = inner.mint_to_reserve.get(mint) {
                return Ok(*reserve);
            }
        }
        
        Err(anyhow::anyhow!("Reserve not found for mint: {}", mint))
    }

    pub async fn get_reserve_data(
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

        let collateral_mint_bytes: [u8; 32] = account.data
            [collateral_mint_offset..collateral_mint_offset + 32]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid reserve collateral mint data"))?;
        let collateral_mint = Pubkey::try_from(collateral_mint_bytes)
            .map_err(|_| anyhow::anyhow!("Invalid collateral mint"))?;

        let liquidity_supply_bytes: [u8; 32] = account.data
            [liquidity_supply_offset..liquidity_supply_offset + 32]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid reserve liquidity supply data"))?;
        let liquidity_supply = Pubkey::try_from(liquidity_supply_bytes)
            .map_err(|_| anyhow::anyhow!("Invalid liquidity supply"))?;

        // Parse ReserveConfig to get loanToValueRatio
        // Reserve structure: version(1) + lastUpdate(9) + lendingMarket(32) + liquidity(185) + collateral(72) + config
        // ReserveConfig: optimalUtilizationRate(1) + loanToValueRatio(1) + ...
        const RESERVE_CONFIG_OFFSET: usize = 1 + 9 + 32 + 185 + 72; // 299 bytes
        const LOAN_TO_VALUE_OFFSET: usize = RESERVE_CONFIG_OFFSET + 1; // After optimalUtilizationRate
        
        let loan_to_value_ratio = if account.data.len() > LOAN_TO_VALUE_OFFSET {
            // LTV is stored as u8 (0-100), convert to f64 (0.0-1.0)
            let ltv_u8 = account.data[LOAN_TO_VALUE_OFFSET];
            ltv_u8 as f64 / 100.0
        } else {
            // Fallback: use default LTV if config not available
            log::warn!("Reserve account data too small to read LTV, using default 0.75");
            0.75
        };

        let data = ReserveData {
            collateral_mint,
            liquidity_supply,
            loan_to_value_ratio,
        };

        let mut inner = self.inner.write().await;
        inner.reserve_data.insert(*reserve, data.clone());

        Ok(data)
    }

    async fn fetch_reserve_mapping_from_rpc(
        &self,
        rpc: &Arc<RpcClient>,
        config: &Config,
    ) -> Result<HashMap<Pubkey, Pubkey>> {
        let program_id =
            Pubkey::from_str(&config.solend_program_id).context("Invalid Solend program ID")?;

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

            let liquidity_mint_bytes: [u8; 32] = account.data
                [liquidity_mint_offset..liquidity_mint_offset + 32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid reserve data"))?;
            let liquidity_mint = Pubkey::try_from(liquidity_mint_bytes)
                .map_err(|_| anyhow::anyhow!("Invalid mint in reserve"))?;

            mint_to_reserve.insert(liquidity_mint, pubkey);
        }

        Ok(mint_to_reserve)
    }

    pub async fn refresh_from_rpc(&self, rpc: &Arc<RpcClient>, config: &Config) -> Result<()> {
        let mint_to_reserve = self.fetch_reserve_mapping_from_rpc(rpc, config).await?;
        let mut inner = self.inner.write().await;
        
        let rpc_reserve_set: HashSet<Pubkey> = mint_to_reserve.values().copied().collect();
        let old_mint_count = inner.mint_to_reserve.len();
        let old_data_count = inner.reserve_data.len();
        
        inner.reserve_data.retain(|reserve, _| rpc_reserve_set.contains(reserve));
        inner.mint_to_reserve = mint_to_reserve;
        inner.initialized = true;
        
        let removed_mint_count = old_mint_count.saturating_sub(inner.mint_to_reserve.len());
        let removed_data_count = old_data_count.saturating_sub(inner.reserve_data.len());
        
        if removed_mint_count > 0 || removed_data_count > 0 {
            log::info!("Reserve cache refresh: {} mint mappings (removed {} stale), {} reserve data entries (removed {} stale)", inner.mint_to_reserve.len(), removed_mint_count, inner.reserve_data.len(), removed_data_count);
        } else {
            log::debug!("Reserve cache refresh: {} mint mappings, {} reserve data entries", inner.mint_to_reserve.len(), inner.reserve_data.len());
        }

        Ok(())
    }

    pub async fn is_initialized(&self) -> bool {
        let inner = self.inner.read().await;
        inner.initialized
    }

    pub async fn wait_for_initialization(&self, timeout: Duration) -> bool {
        use tokio::time::{sleep, Instant};
        let start = Instant::now();

        while start.elapsed() < timeout {
            if self.is_initialized().await {
                return true;
            }
            sleep(Duration::from_millis(100)).await;
        }

        false
    }

    pub fn start_background_refresh(self: Arc<Self>, rpc: Arc<RpcClient>, config: Config) {
        tokio::spawn(async move {
            // ✅ FIX: Perform initial refresh immediately, then start periodic refresh
            log::info!("Performing initial reserve cache refresh...");
            
            let retry_config = RetryConfig {
                max_retries: 3,
                initial_delay_ms: 2000,
                max_delay_ms: 16000,
                backoff_multiplier: 2.0,
                is_rate_limit: false,
            };

            match retry_with_backoff(
                || {
                    let cache = Arc::clone(&self);
                    let rpc = Arc::clone(&rpc);
                    let config = config.clone();
                    async move {
                        cache.refresh_from_rpc(&rpc, &config).await
                    }
                },
                retry_config,
            )
            .await
            {
                Ok(_) => {
                    log::info!("✅ Reserve cache initial refresh completed");
                }
                Err(e) => {
                    log::error!("❌ Reserve cache initial refresh failed after retries: {}", e);
                    log::warn!("⚠️  Scanner and Executor may make unnecessary RPC calls until cache is populated");
                }
            }

            // Periodic refresh every 30 seconds
            const REFRESH_INTERVAL_SECONDS: u64 = 30;
            loop {
                tokio::time::sleep(Duration::from_secs(REFRESH_INTERVAL_SECONDS)).await;
                
                let retry_config = RetryConfig {
                    max_retries: 2,
                    initial_delay_ms: 1000,
                    max_delay_ms: 8000,
                    backoff_multiplier: 2.0,
                    is_rate_limit: false,
                };

                if let Err(e) = retry_with_backoff(
                    || {
                        let cache = Arc::clone(&self);
                        let rpc = Arc::clone(&rpc);
                        let config = config.clone();
                        async move {
                            cache.refresh_from_rpc(&rpc, &config).await
                        }
                    },
                    retry_config,
                )
                .await
                {
                    log::warn!("Reserve cache periodic refresh failed: {}", e);
                }
            }
        });
    }
}

// Helper function to get reserve discriminator
fn get_reserve_discriminator() -> &'static [u8; 8] {
    // This is called in hotpath (get_program_accounts filtering) - hash calculation was expensive
    // SHA256("account:Reserve")[..8] = [0x2b, 0xf2, 0xcc, 0xca, 0x1a, 0xf7, 0x3b, 0x7f]
    const RESERVE_DISCRIMINATOR: [u8; 8] = [0x2b, 0xf2, 0xcc, 0xca, 0x1a, 0xf7, 0x3b, 0x7f];
    &RESERVE_DISCRIMINATOR
}

// Global reserve cache instance
static RESERVE_CACHE: Lazy<Arc<ReserveCache>> = Lazy::new(|| Arc::new(ReserveCache::new()));

pub fn start_reserve_cache_refresh(rpc: Arc<RpcClient>, config: Config) {
    RESERVE_CACHE.clone().start_background_refresh(rpc, config);
}

pub async fn wait_for_reserve_cache_initialization(timeout: Duration) -> bool {
    RESERVE_CACHE
        .as_ref()
        .wait_for_initialization(timeout)
        .await
}

pub async fn get_reserve_address_from_mint(
    mint: &Pubkey,
    rpc: &Arc<RpcClient>,
    config: &Config,
) -> Result<Pubkey> {
    // First check config reserves
    let config_reserves = [
        (config.usdc_mint.parse::<Pubkey>().ok(), config.usdc_reserve_address.as_ref()),
        (config.sol_mint.parse::<Pubkey>().ok(), config.sol_reserve_address.as_ref()),
    ];

    for (config_mint, reserve_str) in config_reserves.iter() {
        if let (Some(cfg_mint), Some(reserve)) = (config_mint, reserve_str) {
            if *mint == *cfg_mint {
                return Pubkey::from_str(reserve).context("Invalid reserve address in config");
            }
        }
    }

    RESERVE_CACHE.as_ref().get_reserve_for_mint(mint, rpc, config).await
}

pub async fn get_reserve_data(
    reserve: &Pubkey,
    rpc: &Arc<RpcClient>,
) -> Result<(Pubkey, Pubkey, Pubkey, f64)> {
    let data = RESERVE_CACHE
        .as_ref()
        .get_reserve_data(reserve, rpc)
        .await?;
    
    Ok((
        data.collateral_mint,
        data.liquidity_supply,
        *reserve, // For backward compatibility (third param was reserve address)
        data.loan_to_value_ratio,
    ))
}

