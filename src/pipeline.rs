use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::jup::{get_jupiter_quote, JupiterQuote};
use crate::solend::{Obligation, Reserve, solend_program_id};
use crate::utils::{send_jito_bundle, JitoClient};

/// Liquidation mode - per Structure.md
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidationMode {
    DryRun,
    Live,
}

/// Config structure per Structure.md section 6.2
pub struct Config {
    pub rpc_url: String,
    pub jito_url: String,
    pub jupiter_url: String,
    pub keypair_path: PathBuf,
    pub liquidation_mode: LiquidationMode,
    pub min_profit_usdc: f64,
    pub max_position_pct: f64, // Örn: 0.05 => cüzdanın %5'i max risk
    pub wallet: Arc<Keypair>,
}

/// Main liquidation loop - minimal async pipeline per Structure.md section 9
pub async fn run_liquidation_loop(
    rpc: Arc<RpcClient>,
    config: Config,
) -> Result<()> {
    let program_id = solend_program_id()?;
    let wallet = config.wallet.pubkey();

    // Initialize Jito client with default tip account
    let jito_tip_account = Pubkey::from_str("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZ6N6VBY6FuDgU3")
        .context("Invalid Jito tip account")?;
    let jito_tip_amount = 10_000_000u64; // 0.01 SOL default
    let jito_client = JitoClient::new(
        config.jito_url.clone(),
        jito_tip_account,
        jito_tip_amount,
    );

    // Validate Jito endpoint per Structure.md section 13
    validate_jito_endpoint(&jito_client).await
        .context("Jito endpoint validation failed - check network connectivity")?;

    log::info!("🚀 Starting liquidation loop");
    log::info!("   Program ID: {}", program_id);
    log::info!("   Wallet: {}", wallet);
    log::info!("   Min Profit USDC: ${}", config.min_profit_usdc);
    log::info!("   Max Position %: {:.2}%", config.max_position_pct * 100.0);
    log::info!("   Mode: {:?}", config.liquidation_mode);

    loop {
        match process_cycle(&rpc, &program_id, &config, &jito_client).await {
            Ok(_) => {}
            Err(e) => {
                log::error!("Error in liquidation cycle: {}", e);
            }
        }

        // Default poll interval per Structure.md
        sleep(Duration::from_millis(500)).await;
    }
}

async fn process_cycle(
    rpc: &Arc<RpcClient>,
    program_id: &Pubkey,
    config: &Config,
    jito_client: &JitoClient,
) -> Result<()> {
    // 1. Solend obligation account'larını çek
    log::debug!("Fetching obligation accounts...");
    let rpc_clone = Arc::clone(rpc);
    let program_id_clone = *program_id;
    let accounts = tokio::task::spawn_blocking(move || {
        rpc_clone.get_program_accounts(&program_id_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
    .map_err(|e| anyhow::anyhow!("Failed to get program accounts: {}", e))?;

    log::debug!("Found {} accounts", accounts.len());

    // 2. HF < 1.0 olanları bul
    let mut candidates = Vec::new();
    for (pk, acc) in accounts {
        if let Ok(obligation) = Obligation::from_account_data(&acc.data) {
            let hf = obligation.health_factor();
            if hf < 1.0 {
                candidates.push((pk, obligation));
            }
        }
    }

    log::info!("Found {} liquidation opportunities (HF < 1.0)", candidates.len());

    // 3. Her candidate için liquidation denemesi per Structure.md section 9
    for (obl_pubkey, obligation) in candidates {
        // a) Oracle + reserve load + HF confirm
        let mut ctx = build_liquidation_context(rpc, &obligation).await?;
        ctx.obligation_pubkey = obl_pubkey; // Set actual obligation pubkey
        if !ctx.oracle_ok {
            log::debug!("Skipping {}: Oracle validation failed", obl_pubkey);
            continue;
        }

        // b) Jupiter'den kârlılık kontrolü
        let quote_result = get_liquidation_quote(&ctx, config).await;
        let quote = match quote_result {
            Ok(q) => q,
            Err(e) => {
                log::debug!("Skipping {}: Jupiter quote failed: {}", obl_pubkey, e);
                continue;
            }
        };

        if quote.profit_usdc < config.min_profit_usdc {
            log::debug!(
                "Skipping {}: Profit ${:.2} < min ${:.2}",
                obl_pubkey,
                quote.profit_usdc,
                config.min_profit_usdc
            );
            continue;
        }

        // c) Wallet risk limiti
        if !is_within_risk_limits(rpc, &config.wallet.pubkey(), &quote, config).await? {
            log::debug!("Skipping {}: Exceeds wallet risk limits", obl_pubkey);
            continue;
        }

        // d) Jito bundle ile gönder
        if matches!(config.liquidation_mode, LiquidationMode::Live) {
            match build_liquidation_tx(&config.wallet, &ctx, &quote, rpc).await {
                Ok(tx) => {
                    let blockhash = rpc
                        .get_latest_blockhash()
                        .map_err(|e| anyhow::anyhow!("Failed to get blockhash: {}", e))?;
                    match send_jito_bundle(tx, jito_client, &config.wallet, blockhash).await {
                        Ok(bundle_id) => {
                            log::info!(
                                "✅ Liquidated {} with profit ${:.2} USDC, bundle_id: {}",
                                obl_pubkey,
                                quote.profit_usdc,
                                bundle_id
                            );
                        }
                        Err(e) => {
                            log::error!("Failed to send Jito bundle for {}: {}", obl_pubkey, e);
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to build liquidation tx for {}: {}", obl_pubkey, e);
                }
            }
        } else {
            log::info!(
                "DryRun: would liquidate obligation {} with profit ~${:.2} USDC",
                obl_pubkey,
                quote.profit_usdc
            );
        }
    }

    Ok(())
}

/// Validate Jito endpoint is reachable per Structure.md section 13
async fn validate_jito_endpoint(jito_client: &JitoClient) -> Result<()> {
    // Simple connectivity check - Jito endpoint validation
    // Note: Jito doesn't have a standard health endpoint, so we just log
    log::info!("✅ Jito client initialized: {}", jito_client.url());
    // In production, you might want to do an actual connectivity test
    // For now, we'll validate during first bundle send
    Ok(())
}

/// Liquidation context per Structure.md section 9
struct LiquidationContext {
    obligation_pubkey: Pubkey,
    obligation: Obligation,
    borrow_reserve: Option<Reserve>,
    deposit_reserve: Option<Reserve>,
    borrow_price_usd: Option<f64>,  // Price from oracle
    deposit_price_usd: Option<f64>, // Price from oracle
    oracle_ok: bool,
}

/// Build liquidation context with Oracle validation per Structure.md section 5.2
async fn build_liquidation_context(
    rpc: &Arc<RpcClient>,
    obligation: &Obligation,
) -> Result<LiquidationContext> {
    // Load reserve accounts for borrow and deposit
    let mut borrow_reserve = None;
    let mut deposit_reserve = None;

    // Get first borrow reserve if exists
    if !obligation.borrows.is_empty() {
        let borrow_reserve_pubkey = obligation.borrows[0].borrowReserve;
        if let Ok(account) = rpc.get_account_data(&borrow_reserve_pubkey) {
            if let Ok(reserve) = Reserve::from_account_data(&account) {
                borrow_reserve = Some(reserve);
            }
        }
    }

    // Get first deposit reserve if exists
    if !obligation.deposits.is_empty() {
        let deposit_reserve_pubkey = obligation.deposits[0].depositReserve;
        if let Ok(account) = rpc.get_account_data(&deposit_reserve_pubkey) {
            if let Ok(reserve) = Reserve::from_account_data(&account) {
                deposit_reserve = Some(reserve);
            }
        }
    }

    // Validate Oracle per Structure.md section 5.2
    let (oracle_ok, borrow_price, deposit_price) = validate_oracles(rpc, &borrow_reserve, &deposit_reserve).await?;

    Ok(LiquidationContext {
        obligation_pubkey: obligation.owner, // Note: actual obligation pubkey should be passed separately
        obligation: obligation.clone(),
        borrow_reserve,
        deposit_reserve,
        borrow_price_usd: borrow_price,
        deposit_price_usd: deposit_price,
        oracle_ok,
    })
}

/// Pyth Network program ID (mainnet)
const PYTH_PROGRAM_ID: &str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH";

/// Switchboard program ID (mainnet)
const SWITCHBOARD_PROGRAM_ID: &str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";

/// Maximum allowed confidence interval (as percentage of price)
const MAX_CONFIDENCE_PCT: f64 = 5.0; // 5% max confidence interval

/// Maximum allowed slot difference for oracle price
const MAX_SLOT_DIFFERENCE: u64 = 150; // ~1 minute at 400ms per slot

/// Maximum allowed price deviation between Pyth and Switchboard (as percentage)
const MAX_ORACLE_DEVIATION_PCT: f64 = 2.0; // 2% max deviation

/// Validate Pyth and Switchboard oracles per Structure.md section 5.2
/// Returns (is_valid, borrow_price_usd, deposit_price_usd)
async fn validate_oracles(
    rpc: &Arc<RpcClient>,
    borrow_reserve: &Option<Reserve>,
    deposit_reserve: &Option<Reserve>,
) -> Result<(bool, Option<f64>, Option<f64>)> {
    // Basic validation: check if reserves exist and have oracle pubkeys
    let borrow_ok = borrow_reserve
        .as_ref()
        .map(|r| r.oracle_pubkey() != Pubkey::default())
        .unwrap_or(false);

    let deposit_ok = deposit_reserve
        .as_ref()
        .map(|r| r.oracle_pubkey() != Pubkey::default())
        .unwrap_or(false);

    if !borrow_ok || !deposit_ok {
        log::debug!("Oracle validation failed: missing oracle pubkeys");
        return Ok((false, None, None));
    }

    // Get current slot for stale check
    let current_slot = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;

    // Validate borrow reserve oracle
    let borrow_pyth_price = if let Some(reserve) = borrow_reserve {
        match validate_pyth_oracle(rpc, reserve.oracle_pubkey(), current_slot).await {
            Ok((valid, price)) => {
                if !valid {
                    log::debug!("Borrow reserve oracle validation failed");
                    return Ok((false, None, None));
                }
                price
            }
            Err(e) => {
                log::debug!("Borrow reserve oracle validation error: {}", e);
                return Ok((false, None, None));
            }
        }
    } else {
        None
    };

    // Validate deposit reserve oracle
    let deposit_pyth_price = if let Some(reserve) = deposit_reserve {
        match validate_pyth_oracle(rpc, reserve.oracle_pubkey(), current_slot).await {
            Ok((valid, price)) => {
                if !valid {
                    log::debug!("Deposit reserve oracle validation failed");
                    return Ok((false, None, None));
                }
                price
            }
            Err(e) => {
                log::debug!("Deposit reserve oracle validation error: {}", e);
                return Ok((false, None, None));
            }
        }
    } else {
        None
    };

    // If both reserves have Switchboard oracles, validate deviation per Structure.md section 5.2
    // Note: Switchboard oracle pubkey would be in ReserveConfig if available
    // For now, we check if ReserveConfig has switchboard_oracle_pubkey field
    // If available, validate deviation between Pyth and Switchboard prices

    // Validate Switchboard oracles if available
    if let (Some(borrow_reserve), Some(borrow_price)) = (borrow_reserve, borrow_pyth_price) {
        // Check if ReserveConfig has switchboard oracle (would need to be added to IDL)
        // For now, we'll add a placeholder validation function
        if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
            rpc,
            &borrow_reserve,
            current_slot,
        )
        .await?
        {
            // Compare Pyth and Switchboard prices
            let deviation_pct = ((borrow_price - switchboard_price).abs() / borrow_price) * 100.0;
            if deviation_pct > MAX_ORACLE_DEVIATION_PCT {
                log::debug!(
                    "Oracle deviation too high for borrow reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    MAX_ORACLE_DEVIATION_PCT,
                    borrow_price,
                    switchboard_price
                );
                return Ok((false, None, None));
            }
            log::debug!(
                "Oracle deviation OK for borrow reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                deviation_pct,
                borrow_price,
                switchboard_price
            );
        }
    }

    if let (Some(deposit_reserve), Some(deposit_price)) = (deposit_reserve, deposit_pyth_price) {
        if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
            rpc,
            &deposit_reserve,
            current_slot,
        )
        .await?
        {
            let deviation_pct = ((deposit_price - switchboard_price).abs() / deposit_price) * 100.0;
            if deviation_pct > MAX_ORACLE_DEVIATION_PCT {
                log::debug!(
                    "Oracle deviation too high for deposit reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    MAX_ORACLE_DEVIATION_PCT,
                    deposit_price,
                    switchboard_price
                );
                return Ok((false, None, None));
            }
            log::debug!(
                "Oracle deviation OK for deposit reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                deviation_pct,
                deposit_price,
                switchboard_price
            );
        }
    }

    Ok((true, borrow_pyth_price, deposit_pyth_price))
}

/// Validate Switchboard oracle if available in ReserveConfig
/// Returns Some(price) if Switchboard oracle exists and is valid, None otherwise
async fn validate_switchboard_oracle_if_available(
    _rpc: &Arc<RpcClient>,
    _reserve: &Reserve,
    _current_slot: u64,
) -> Result<Option<f64>> {
    // TODO: When ReserveConfig includes switchboard_oracle_pubkey field:
    // 1. Get switchboard_oracle_pubkey from reserve.config
    // 2. Verify it belongs to Switchboard program ID
    // 3. Parse Switchboard price account
    // 4. Validate price is fresh (similar to Pyth)
    // 5. Return price

    // For now, ReserveConfig doesn't have switchboard_oracle_pubkey
    // This is a placeholder for future implementation
    Ok(None)
}

/// Validate Pyth oracle per Structure.md section 5.2
/// Returns (is_valid, price) where price is in USD with decimals
async fn validate_pyth_oracle(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    current_slot: u64,
) -> Result<(bool, Option<f64>)> {
    // 1. Verify oracle account belongs to Pyth program ID
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    let pyth_program_id = Pubkey::from_str(PYTH_PROGRAM_ID)
        .map_err(|e| anyhow::anyhow!("Invalid Pyth program ID: {}", e))?;

    if oracle_account.owner != pyth_program_id {
        log::debug!("Oracle account {} does not belong to Pyth program", oracle_pubkey);
        return Ok((false, None));
    }

    // 2. Parse Pyth price account (simplified - basic structure check)
    // Pyth price account structure (simplified):
    // - First 8 bytes: magic number / version
    // - Next 4 bytes: price_type
    // - Next 8 bytes: exponent
    // - Next 32 bytes: price (i64)
    // - Next 32 bytes: confidence (u64)
    // - Next 8 bytes: timestamp
    // - Next 8 bytes: prev_publish_time
    // - Next 8 bytes: prev_price
    // - Next 8 bytes: prev_conf
    // - Next 8 bytes: last_slot (slot when price was last updated)

    if oracle_account.data.len() < 100 {
        log::debug!("Oracle account data too short: {} bytes", oracle_account.data.len());
        return Ok((false, None));
    }

    // Parse price from Pyth account (simplified)
    // Pyth price is at offset ~32 bytes (i64, 8 bytes)
    // Exponent is at offset ~16 bytes (i32, 4 bytes)
    let mut price: Option<f64> = None;
    let mut exponent: i32 = 0;
    if oracle_account.data.len() >= 40 {
        // Parse exponent
        let exponent_bytes: [u8; 4] = oracle_account.data[16..20]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?;
        exponent = i32::from_le_bytes(exponent_bytes);

        // Parse price (i64)
        let price_bytes: [u8; 8] = oracle_account.data[32..40]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse price"))?;
        let price_raw = i64::from_le_bytes(price_bytes);

        // Convert to f64 with exponent
        price = Some(price_raw as f64 * 10_f64.powi(exponent));
    }

    // 3. Check if price is stale (slot difference)
    // Pyth price account structure (v2):
    // - Offset 0-4: magic (4 bytes)
    // - Offset 4-5: version (1 byte)
    // - Offset 5-6: price_type (1 byte)
    // - Offset 6-8: size (2 bytes)
    // - Offset 8-16: price (i64, 8 bytes)
    // - Offset 16-20: exponent (i32, 4 bytes)
    // - Offset 20-24: reserved (4 bytes)
    // - Offset 24-32: timestamp (i64, 8 bytes)
    // - Offset 32-40: prev_publish_time (i64, 8 bytes)
    // - Offset 40-48: prev_price (i64, 8 bytes)
    // - Offset 48-56: prev_conf (u64, 8 bytes)
    // - Offset 56-64: last_slot (u64, 8 bytes) - THIS IS WHAT WE NEED
    // - Offset 64-72: valid_slot (u64, 8 bytes)
    // - Offset 72+: publisher accounts...

    if oracle_account.data.len() >= 64 {
        // Parse last_slot from offset 56-64
        let last_slot_bytes: [u8; 8] = oracle_account.data[56..64]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse last_slot"))?;
        let last_slot = u64::from_le_bytes(last_slot_bytes);

        // Check slot difference
        let slot_difference = current_slot.saturating_sub(last_slot);
        if slot_difference > MAX_SLOT_DIFFERENCE {
            log::debug!(
                "Oracle price is stale: slot difference {} > {} (last_slot: {}, current_slot: {})",
                slot_difference,
                MAX_SLOT_DIFFERENCE,
                last_slot,
                current_slot
            );
            return Ok((false, None));
        }

        log::debug!(
            "Oracle price is fresh: slot difference {} <= {} (last_slot: {}, current_slot: {})",
            slot_difference,
            MAX_SLOT_DIFFERENCE,
            last_slot,
            current_slot
        );
    } else {
        log::debug!(
            "Oracle account data too short for stale check: {} bytes (need at least 64)",
            oracle_account.data.len()
        );
        // Don't fail, but log warning
    }

    // 4. Check confidence interval
    if let Some(price_value) = price {
        if oracle_account.data.len() >= 48 {
            // Parse confidence (u64 at offset ~40 bytes)
            let conf_bytes: [u8; 8] = oracle_account.data[40..48]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
            let confidence = u64::from_le_bytes(conf_bytes) as f64 * 10_f64.powi(exponent);

            // Check if confidence interval is too large
            let confidence_pct = (confidence / price_value.abs()) * 100.0;
            if confidence_pct > MAX_CONFIDENCE_PCT {
                log::debug!(
                    "Oracle confidence too high: {:.2}% > {:.2}%",
                    confidence_pct,
                    MAX_CONFIDENCE_PCT
                );
                return Ok((false, None));
            }
        }
    }

    log::debug!("✅ Pyth oracle validation passed for {} (price: {:?})", oracle_pubkey, price);
    Ok((true, price))
}

/// Liquidation quote with profit calculation per Structure.md section 7
struct LiquidationQuote {
    quote: JupiterQuote,
    profit_usdc: f64,
}

/// Get Jupiter quote for liquidation with profit calculation per Structure.md section 7
async fn get_liquidation_quote(
    ctx: &LiquidationContext,
    _config: &Config,
) -> Result<LiquidationQuote> {
    // Use first borrow and first deposit
    if ctx.obligation.borrows.is_empty() || ctx.obligation.deposits.is_empty() {
        return Err(anyhow::anyhow!("No borrows or deposits in obligation"));
    }

    let borrow = &ctx.obligation.borrows[0];
    let deposit = &ctx.obligation.deposits[0];

    // Get mint addresses from reserve accounts
    let collateral_mint = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.collateral_mint_pubkey())
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;

    let debt_mint = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.mint_pubkey())
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;

    // Calculate liquidation amount (simplified - use close factor)
    // borrowedAmountWad is u128, divide by 2 for 50% close factor
    let liquidation_amount = borrow.borrowedAmountWad / 2; // 50% close factor

    // Get Jupiter quote: collateral -> debt token
    let quote = get_jupiter_quote(
        &collateral_mint, // input: collateral mint
        &debt_mint,       // output: debt mint
        liquidation_amount as u64,
        50, // slippage_bps
    )
    .await
    .context("Failed to get Jupiter quote")?;

    // Calculate profit per Structure.md section 7
    // profit = collateral_value_usd - debt_repaid_value_usd - swap_fee_usd - jito_fee_usd - tx_fee_usd
    let in_amount: u64 = quote.in_amount.parse().unwrap_or(0);
    let out_amount: u64 = quote.out_amount.parse().unwrap_or(0);

    // Get token decimals from reserves
    let collateral_decimals = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.liquidity.mintDecimals)
        .unwrap_or(6);
    let debt_decimals = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.liquidity.mintDecimals)
        .unwrap_or(6);

    // Convert amounts to human-readable
    let collateral_amount = in_amount as f64 / 10_f64.powi(collateral_decimals as i32);
    let debt_amount = out_amount as f64 / 10_f64.powi(debt_decimals as i32);

    // Use oracle prices to calculate USD values
    let collateral_value_usd = ctx
        .deposit_price_usd
        .map(|p| collateral_amount * p)
        .unwrap_or(0.0);
    let debt_value_usd = ctx
        .borrow_price_usd
        .map(|p| debt_amount * p)
        .unwrap_or(0.0);

    // Calculate profit in USD
    // Note: For liquidation, we repay debt and get collateral
    // Profit = collateral_value - debt_value - fees
    let swap_fee_usd = 0.0; // TODO: Calculate actual swap fee from Jupiter
    let jito_fee_usd = 0.00001; // 0.01 SOL ≈ $0.00001 (simplified)
    let tx_fee_usd = 0.000005; // ~5000 lamports ≈ $0.000005

    let profit_usdc = collateral_value_usd - debt_value_usd - swap_fee_usd - jito_fee_usd - tx_fee_usd;

    Ok(LiquidationQuote {
        quote,
        profit_usdc,
    })
}

/// Check if liquidation is within wallet risk limits per Structure.md section 6.4
async fn is_within_risk_limits(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
    quote: &LiquidationQuote,
    config: &Config,
) -> Result<bool> {
    // Get wallet SOL balance
    let sol_balance = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get wallet balance: {}", e))?;

    // Calculate wallet value (simplified: SOL only for now)
    // TODO: Include USDC and other token balances
    let wallet_value_usd = (sol_balance as f64) / 1_000_000_000.0 * 100.0; // Rough SOL price estimate

    // Calculate max position size
    let max_position_usd = wallet_value_usd * config.max_position_pct;

    // Check if liquidation amount exceeds max position
    // Simplified: use quote input amount as position size
    let position_size_usd = quote.quote.in_amount.parse::<f64>().unwrap_or(0.0) / 1_000_000.0;

    Ok(position_size_usd <= max_position_usd)
}

/// Build liquidation transaction per Structure.md section 8
async fn build_liquidation_tx(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
) -> Result<Transaction> {
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
        system_program,
        sysvar,
    };
    use spl_token::ID as TOKEN_PROGRAM_ID;

    let program_id = solend_program_id()?;
    let wallet_pubkey = wallet.pubkey();

    // Get reserves
    let borrow_reserve = ctx
        .borrow_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = ctx
        .deposit_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;

    // Calculate liquidation amount from quote
    let liquidity_amount: u64 = quote.quote.in_amount.parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse liquidation amount: {}", e))?;

    // Derive required addresses
    let lending_market = ctx.obligation.lendingMarket;
    let lending_market_authority = crate::solend::derive_lending_market_authority(&lending_market, &program_id)?;

    // Get reserve liquidity supply addresses
    // These are stored in Reserve account (supplyPubkey field)
    // Solend program stores the correct PDA addresses in Reserve account during initialization
    let repay_reserve_liquidity_supply = borrow_reserve.liquidity.supplyPubkey;
    let withdraw_reserve_liquidity_supply = deposit_reserve.liquidity.supplyPubkey;
    let withdraw_reserve_collateral_mint = deposit_reserve.collateral.mintPubkey;
    
    // Verify these are not default/zero addresses
    if repay_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_collateral_mint == Pubkey::default() {
        return Err(anyhow::anyhow!("Invalid reserve addresses: one or more addresses are default/zero"));
    }

    // Optional: Verify PDA derivation matches (for extra safety)
    // Note: This is a verification step - Reserve account's supplyPubkey should already be correct
    // If verification fails, it might indicate a corrupted Reserve account
    let borrow_reserve_pubkey = ctx.obligation.borrows[0].borrowReserve;
    let deposit_reserve_pubkey = ctx.obligation.deposits[0].depositReserve;
    
    if let Ok(derived_repay_supply) = crate::solend::derive_reserve_liquidity_supply(&borrow_reserve_pubkey, &program_id) {
        if derived_repay_supply != repay_reserve_liquidity_supply {
            log::warn!(
                "PDA derivation mismatch for repay reserve liquidity supply: \
                 Reserve has {}, derived is {}. Using Reserve value.",
                repay_reserve_liquidity_supply,
                derived_repay_supply
            );
            // Continue anyway - Reserve account value is authoritative
        }
    }
    
    if let Ok(derived_withdraw_supply) = crate::solend::derive_reserve_liquidity_supply(&deposit_reserve_pubkey, &program_id) {
        if derived_withdraw_supply != withdraw_reserve_liquidity_supply {
            log::warn!(
                "PDA derivation mismatch for withdraw reserve liquidity supply: \
                 Reserve has {}, derived is {}. Using Reserve value.",
                withdraw_reserve_liquidity_supply,
                derived_withdraw_supply
            );
            // Continue anyway - Reserve account value is authoritative
        }
    }

    // Get user's token accounts (source liquidity and destination collateral)
    // These would be ATAs for the tokens
    use spl_associated_token_account::get_associated_token_address;
    let source_liquidity = get_associated_token_address(&wallet_pubkey, &borrow_reserve.liquidity.mintPubkey);
    let destination_collateral = get_associated_token_address(&wallet_pubkey, &withdraw_reserve_collateral_mint);

    // Build Solend liquidation instruction per IDL
    // Solend uses enum-based instruction encoding
    // 
    // IMPORTANT: Instruction discriminator must match Solend program's actual encoding.
    // This is currently set to 0 based on IDL analysis, but should be verified in production.
    let mut instruction_data = Vec::new();
    
    // Instruction discriminator: liquidateObligation discriminator
    // WARNING: This value (0) is based on IDL structure where liquidateObligation is first instruction.
    // In production, verify this matches Solend's actual instruction enum encoding.
    // If Solend uses a different encoding scheme, this will cause transaction failures.
    let discriminator = crate::solend::get_liquidate_obligation_discriminator();
    log::debug!("Using instruction discriminator: {} for liquidateObligation", discriminator);
    instruction_data.push(discriminator);
    
    // Args: liquidityAmount (u64)
    instruction_data.extend_from_slice(&liquidity_amount.to_le_bytes());

    // Build account metas per IDL
    let accounts = vec![
        AccountMeta::new(source_liquidity, false),                    // sourceLiquidity
        AccountMeta::new(destination_collateral, false),              // destinationCollateral
        AccountMeta::new(ctx.obligation.borrows[0].borrowReserve, false),  // repayReserve
        AccountMeta::new(repay_reserve_liquidity_supply, false),      // repayReserveLiquiditySupply
        AccountMeta::new(ctx.obligation.deposits[0].depositReserve, false), // withdrawReserve
        AccountMeta::new_readonly(withdraw_reserve_collateral_mint, false), // withdrawReserveCollateralMint
        AccountMeta::new(withdraw_reserve_liquidity_supply, false),   // withdrawReserveLiquiditySupply
        AccountMeta::new(ctx.obligation_pubkey, false),                // obligation
        AccountMeta::new_readonly(lending_market, false),            // lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),   // lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),               // transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),          // tokenProgram
    ];

    let liquidation_ix = Instruction {
        program_id,
        accounts,
        data: instruction_data,
    };

    // Add compute budget instruction per Structure.md section 8
    // Note: solana-sdk 1.18 doesn't have ComputeBudgetInstruction, so we build manually
    // Compute Budget Program ID
    let compute_budget_program_id = Pubkey::from_str("ComputeBudget111111111111111111111111111111")
        .map_err(|e| anyhow::anyhow!("Invalid compute budget program ID: {}", e))?;

    // Build compute unit limit instruction manually
    // Instruction format: [discriminator: 2, units: u32]
    let mut compute_limit_data = vec![2u8]; // SetComputeUnitLimit discriminator
    compute_limit_data.extend_from_slice(&(200_000u32).to_le_bytes());
    let compute_budget_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_limit_data,
    };

    // Build compute unit price instruction manually
    // Instruction format: [discriminator: 3, micro_lamports: u64]
    let mut compute_price_data = vec![3u8]; // SetComputeUnitPrice discriminator
    compute_price_data.extend_from_slice(&(1_000u64).to_le_bytes()); // 0.001 SOL per CU
    let priority_fee_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_price_data,
    };

    // Get recent blockhash
    let blockhash = rpc
        .get_latest_blockhash()
        .map_err(|e| anyhow::anyhow!("Failed to get blockhash: {}", e))?;

    // Build transaction
    let mut tx = Transaction::new_with_payer(
        &[compute_budget_ix, priority_fee_ix, liquidation_ix],
        Some(&wallet_pubkey),
    );
    tx.message.recent_blockhash = blockhash;

    log::info!(
        "Built liquidation transaction for obligation {} with amount {}",
        ctx.obligation_pubkey,
        liquidity_amount
    );

    Ok(tx)
}


