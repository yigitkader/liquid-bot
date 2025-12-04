use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use crate::core::types::Opportunity;
use crate::protocol::oracle::{get_pyth_oracle_account, get_switchboard_oracle_account};
use crate::protocol::solend::accounts::{
    derive_lending_market_authority, get_associated_token_address,
};
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use spl_token;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

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

// ✅ FIX: Fetch lock to prevent multiple threads from making concurrent RPC calls
static FETCH_LOCK: Lazy<Arc<tokio::sync::Mutex<()>>> = Lazy::new(|| Arc::new(tokio::sync::Mutex::new(())));

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

    async fn get_reserve_for_mint(
        &self,
        mint: &Pubkey,
        rpc: &Arc<RpcClient>,
        config: &Config,
    ) -> Result<Pubkey> {
        // Fast path: check cache first
        {
            let inner = self.inner.read().await;
            if let Some(reserve) = inner.mint_to_reserve.get(mint) {
                return Ok(*reserve);
            }
        }

        // Slow path: acquire fetch lock to prevent concurrent RPC calls
        let _lock = FETCH_LOCK.lock().await;
        
        // Double-check after acquiring fetch lock (another thread may have populated cache)
        {
            let inner = self.inner.read().await;
            if let Some(reserve) = inner.mint_to_reserve.get(mint) {
                return Ok(*reserve);
            }
        }

        // Fetch from RPC (only one thread will do this)
        let mint_to_reserve = self.fetch_reserve_mapping_from_rpc(rpc, config).await?;
        
        // Update cache using extend instead of override to preserve other entries
        {
            let mut inner = self.inner.write().await;
            // ✅ FIX: Use extend instead of override to prevent losing other thread's entries
            inner.mint_to_reserve.extend(mint_to_reserve);
            inner.initialized = true;
            
            inner
                .mint_to_reserve
                .get(mint)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("Reserve not found for mint: {}", mint))
        }
    }

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

        let data = ReserveData {
            collateral_mint,
            liquidity_supply,
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

    async fn refresh_from_rpc(&self, rpc: &Arc<RpcClient>, config: &Config) -> Result<()> {
        let mint_to_reserve = self.fetch_reserve_mapping_from_rpc(rpc, config).await?;
        let mut inner = self.inner.write().await;
        
        // ✅ CLEANUP: Remove cache entries for reserves that no longer exist on-chain
        // This prevents stale cache entries from accumulating when reserves are removed
        let rpc_reserve_set: HashSet<Pubkey> = mint_to_reserve.values().copied().collect();
        let mut removed_count = 0;
        
        // Remove mint_to_reserve entries for reserves that no longer exist
        inner.mint_to_reserve.retain(|mint, reserve| {
            if rpc_reserve_set.contains(reserve) {
                true // Keep: reserve still exists on-chain
            } else {
                removed_count += 1;
                log::debug!(
                    "SolendProtocol: Removing stale mint_to_reserve entry: mint {} -> reserve {} (reserve no longer exists on-chain)",
                    mint,
                    reserve
                );
                false // Remove: reserve no longer exists on-chain
            }
        });
        
        // Also clean up reserve_data cache for removed reserves
        let mut removed_data_count = 0;
        inner.reserve_data.retain(|reserve, _| {
            if rpc_reserve_set.contains(reserve) {
                true // Keep: reserve still exists
            } else {
                removed_data_count += 1;
                false // Remove: reserve no longer exists
            }
        });
        
        if removed_count > 0 || removed_data_count > 0 {
            log::debug!(
                "Reserve cache cleanup: removed {} mint mappings and {} reserve data entries (reserves no longer exist on-chain)",
                removed_count,
                removed_data_count
            );
        }
        
        // ✅ FIX: Use extend instead of override to prevent losing entries added by other threads
        // Problem: If another thread adds a mapping during refresh, override would lose it
        // Solution: Extend preserves existing entries while updating with fresh RPC data
        inner.mint_to_reserve.extend(mint_to_reserve);
        inner.initialized = true;

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
            // This prevents race condition where cache is empty for 30s, causing
            // Scanner/Executor to make unnecessary RPC calls (get_program_accounts)
            log::info!("Performing initial reserve cache refresh...");
            
            // ✅ FIX: Retry mechanism with exponential backoff for initial refresh
            // This handles transient RPC errors and timeouts
            const MAX_RETRIES: u32 = 3;
            const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(2);
            
            let mut retry_count = 0;
            let mut retry_delay = INITIAL_RETRY_DELAY;
            
            loop {
                match self.refresh_from_rpc(&rpc, &config).await {
                    Ok(()) => {
                        log::info!("✅ Initial reserve cache populated successfully");
                        break;
                    }
                    Err(e) => {
                        retry_count += 1;
                        if retry_count > MAX_RETRIES {
                            log::error!("Initial reserve cache refresh FAILED after {} retries: {}", MAX_RETRIES, e);
                            log::error!("⚠️  Reserve cache will remain empty - Scanner/Executor may make unnecessary RPC calls");
                            break;
                        }
                        log::warn!("Initial reserve cache refresh failed (attempt {}/{}): {}", retry_count, MAX_RETRIES, e);
                        log::info!("Retrying in {:?}...", retry_delay);
                        tokio::time::sleep(retry_delay).await;
                        retry_delay = retry_delay * 2; // Exponential backoff
                    }
                }
            }

            // Now start periodic refresh
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await; // 5 minutes

                log::info!("🔄 Refreshing reserve cache from RPC...");
                match self.refresh_from_rpc(&rpc, &config).await {
                    Ok(()) => {
                        let inner = self.inner.read().await;
                        let reserve_count = inner.reserve_data.len();
                        let mint_count = inner.mint_to_reserve.len();
                        log::info!("✅ Reserve cache refreshed successfully ({} reserves, {} mints)", reserve_count, mint_count);
                    }
                    Err(e) => {
                        log::warn!("⚠️  Reserve cache refresh failed: {}", e);
                    }
                }
            }
        });
    }
}

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

    let _market_address = config
        .main_lending_market_address
        .as_ref()
        .and_then(|s| Pubkey::from_str(s).ok())
        .ok_or_else(|| anyhow::anyhow!("Main lending market address not configured"))?;

    RESERVE_CACHE
        .as_ref()
        .get_reserve_for_mint(mint, rpc, config)
        .await
}

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
    let data = RESERVE_CACHE
        .as_ref()
        .get_reserve_data(reserve, rpc)
        .await?;

    Ok((data.collateral_mint, data.liquidity_supply, *reserve))
}

pub async fn build_liquidate_obligation_ix(
    opportunity: &Opportunity,
    liquidator: &Pubkey,
    rpc: Option<Arc<RpcClient>>,
) -> Result<Instruction> {
    let rpc =
        rpc.ok_or_else(|| anyhow::anyhow!("RPC client required for liquidation instruction"))?;

    let config = crate::core::config::Config::from_env().context("Failed to load config")?;

    let program_id =
        Pubkey::from_str(&config.solend_program_id).context("Invalid Solend program ID")?;

    let obligation_address = opportunity.position.address;

    let obligation_account = rpc
        .get_account(&obligation_address)
        .await
        .context("Failed to fetch obligation account")?;

    use crate::protocol::solend::types::SolendObligation;
    let obligation = SolendObligation::from_account_data(&obligation_account.data)
        .context("Failed to parse obligation")?;

    let lending_market = obligation.lending_market;

    let lending_market_authority = derive_lending_market_authority(&lending_market, &program_id)
        .context("Failed to derive lending market authority")?;

    let debt_reserve = get_reserve_address_from_mint(&opportunity.debt_mint, &rpc, &config)
        .await
        .context("Failed to find debt reserve")?;

    let (debt_reserve_collateral_mint, debt_reserve_liquidity_supply, _) =
        get_reserve_data(&debt_reserve, &rpc)
            .await
            .context("Failed to fetch debt reserve data")?;

    let collateral_reserve =
        get_reserve_address_from_mint(&opportunity.collateral_mint, &rpc, &config)
            .await
            .context("Failed to find collateral reserve")?;

    let (_collateral_reserve_collateral_mint, collateral_reserve_liquidity_supply, _) =
        get_reserve_data(&collateral_reserve, &rpc)
            .await
            .context("Failed to fetch collateral reserve data")?;

    // ✅ FIX: Native SOL is handled specially in Solend
    // For native SOL, use wallet account directly instead of ATA
    // Solend protocol uses native SOL directly, not wrapped SOL
    use std::str::FromStr;
    let sol_mint = Pubkey::from_str(&config.sol_mint)
        .context("Failed to parse SOL mint address")?;
    
    let source_liquidity = if opportunity.debt_mint == sol_mint {
        // Native SOL: use wallet account directly
        *liquidator
    } else {
        // SPL tokens: use ATA
        get_associated_token_address(liquidator, &opportunity.debt_mint, Some(&config))
            .context("Failed to derive source liquidity ATA")?
    };

    let destination_collateral = if opportunity.collateral_mint == sol_mint {
        // Native SOL: use wallet account directly
        *liquidator
    } else {
        // SPL tokens: use ATA
        get_associated_token_address(liquidator, &opportunity.collateral_mint, Some(&config))
            .context("Failed to derive destination collateral ATA")?
    };

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
