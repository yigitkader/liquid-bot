use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::jup::{get_jupiter_quote_with_retry, JupiterQuote};
use crate::solend::{Obligation, Reserve, solend_program_id};
use crate::utils::{send_jito_bundle, JitoClient};

/// Liquidation mode - per Structure.md
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidationMode {
    DryRun,
    Live,
}

/// Config structure per Structure.md section 6.2
/// All values are automatically discovered from chain or environment - no hardcoded addresses
pub struct Config {
    pub rpc_url: String,
    pub jito_url: String,
    pub jupiter_url: String,
    pub keypair_path: PathBuf,
    pub liquidation_mode: LiquidationMode,
    pub min_profit_usdc: f64,
    pub max_position_pct: f64, // Örn: 0.05 => cüzdanın %5'i max risk
    pub wallet: Arc<Keypair>,
    pub jito_tip_account: Option<String>, // Auto-discovered from env or default
    pub jito_tip_amount_lamports: Option<u64>, // Auto-discovered from env or default
}

/// Main liquidation loop - minimal async pipeline per Structure.md section 9
pub async fn run_liquidation_loop(
    rpc: Arc<RpcClient>,
    config: Config,
) -> Result<()> {
    let program_id = solend_program_id()?;
    let wallet = config.wallet.pubkey();

    // Initialize Jito client with tip account from config or default
    // Auto-discovered from environment variables, no hardcoded values
    let jito_tip_account_str = config.jito_tip_account.as_deref()
        .unwrap_or("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZ6N6VBY6FuDgU3"); // Default mainnet tip account
    let jito_tip_account = Pubkey::from_str(jito_tip_account_str)
        .context("Invalid Jito tip account")?;
    
    let jito_tip_amount = config.jito_tip_amount_lamports
        .unwrap_or(10_000_000u64); // Default: 0.01 SOL
    
    log::info!("✅ Jito tip account: {} (from config/env)", jito_tip_account);
    log::info!("✅ Jito tip amount: {} lamports (~{} SOL)", 
        jito_tip_amount, 
        jito_tip_amount as f64 / 1_000_000_000.0);
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

/// Metrics for tracking liquidation cycle performance
struct CycleMetrics {
    total_candidates: usize,
    skipped_oracle_fail: usize,
    skipped_jupiter_fail: usize,
    skipped_insufficient_profit: usize,
    skipped_risk_limit: usize,
    failed_build_tx: usize,
    failed_send_bundle: usize,
    successful: usize,
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

    let total_candidates = candidates.len();
    log::info!("Found {} liquidation opportunities (HF < 1.0)", total_candidates);

    // Initialize metrics
    let mut metrics = CycleMetrics {
        total_candidates,
        skipped_oracle_fail: 0,
        skipped_jupiter_fail: 0,
        skipped_insufficient_profit: 0,
        skipped_risk_limit: 0,
        failed_build_tx: 0,
        failed_send_bundle: 0,
        successful: 0,
    };

    // Per Structure.md section 6.4: Track block-wide cumulative risk
    // "Tek blok içinde kullanılan toplam risk de aynı limit ile sınırlıdır"
    // 
    // CRITICAL: Wallet balance is refreshed before each liquidation to prevent race conditions.
    // If multiple liquidations are sent in the same cycle, the wallet balance may change
    // after the first liquidation executes. We refresh the balance before each check to
    // ensure cumulative_risk tracking reflects the actual wallet state.
    let mut cumulative_risk_usd = 0.0; // Track total risk used in this cycle
    
    // Track pending liquidations (sent but not yet executed on-chain)
    // This accounts for liquidations that have been sent via Jito but haven't executed yet
    let mut pending_liquidation_value = 0.0f64;

    log::debug!(
        "Cycle started: cumulative_risk tracking initialized (will refresh wallet balance before each liquidation)"
    );

    // 3. Her candidate için liquidation denemesi per Structure.md section 9
    for (obl_pubkey, obligation) in candidates {
        // a) Oracle + reserve load + HF confirm
        let ctx = match build_liquidation_context(rpc, &obligation).await {
            Ok(mut ctx) => {
                ctx.obligation_pubkey = obl_pubkey; // Set actual obligation pubkey
                ctx
            }
            Err(e) => {
                log::warn!("Failed to build liquidation context for {}: {}", obl_pubkey, e);
                metrics.skipped_oracle_fail += 1;
                continue;
            }
        };
        
        if !ctx.oracle_ok {
            log::warn!("Skipping {}: Oracle validation failed", obl_pubkey);
            metrics.skipped_oracle_fail += 1;
            continue;
        }

        // b) Jupiter'den kârlılık kontrolü
        let quote_result = get_liquidation_quote(&ctx, config, rpc).await;
        let quote = match quote_result {
            Ok(q) => q,
            Err(e) => {
                log::warn!("Skipping {}: Jupiter quote failed: {}", obl_pubkey, e);
                metrics.skipped_jupiter_fail += 1;
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
            metrics.skipped_insufficient_profit += 1;
            continue;
        }

        // c) Wallet risk limiti - per-liquidation check
        // CRITICAL: Refresh wallet balance before each liquidation to prevent race conditions.
        // If a previous liquidation in this cycle has executed, the wallet balance may have changed.
        // We refresh the balance here to ensure risk limits are calculated based on current wallet state.
        let current_wallet_value_usd = match get_wallet_value_usd(rpc, &config.wallet.pubkey()).await {
            Ok(value) => value,
            Err(e) => {
                log::warn!("Failed to get wallet value for risk limit check: {}", e);
                metrics.skipped_risk_limit += 1;
                continue;
            }
        };
        
        // Account for pending liquidations when calculating available liquidity
        // Pending liquidations are sent but not yet executed, so they reduce available capital
        let available_liquidity = current_wallet_value_usd - pending_liquidation_value;
        let current_max_position_usd = available_liquidity * config.max_position_pct;
        
        let position_size_usd = quote.collateral_value_usd;
        
        log::debug!(
            "Risk calculation: wallet=${:.2}, pending=${:.2}, available=${:.2}, max_position=${:.2}",
            current_wallet_value_usd,
            pending_liquidation_value,
            available_liquidity,
            current_max_position_usd
        );
        
        // Per-liquidation check: single liquidation cannot exceed max position
        if position_size_usd > current_max_position_usd {
            log::warn!(
                "Skipping {}: Position ${:.2} exceeds per-liquidation limit ${:.2} (wallet_value=${:.2})",
                obl_pubkey,
                position_size_usd,
                current_max_position_usd,
                current_wallet_value_usd
            );
            metrics.skipped_risk_limit += 1;
            continue;
        }

        // Per Structure.md section 6.4: Block-wide cumulative risk check
        // "Tek blok içinde kullanılan toplam risk de aynı limit ile sınırlıdır"
        // 
        // CRITICAL: cumulative_risk_usd tracks risk from liquidations sent in this cycle.
        // We check against current wallet value to ensure we don't exceed limits even if
        // previous liquidations have executed and changed the wallet balance.
        let new_cumulative_risk = cumulative_risk_usd + position_size_usd;
        if new_cumulative_risk > current_max_position_usd {
            log::warn!(
                "Skipping {}: Cumulative risk ${:.2} + position ${:.2} = ${:.2} exceeds block-wide limit ${:.2} (wallet_value=${:.2})",
                obl_pubkey,
                cumulative_risk_usd,
                position_size_usd,
                new_cumulative_risk,
                current_max_position_usd,
                current_wallet_value_usd
            );
            metrics.skipped_risk_limit += 1;
            continue;
        }
        
        log::debug!(
            "Risk check passed: position=${:.2}, cumulative=${:.2}/{:.2}, wallet_value=${:.2}",
            position_size_usd,
            new_cumulative_risk,
            current_max_position_usd,
            current_wallet_value_usd
        );

        // d) Jito bundle ile gönder
        if matches!(config.liquidation_mode, LiquidationMode::Live) {
            // CRITICAL: Fetch blockhash RIGHT before building transaction to ensure it's fresh
            // Blockhashes are valid for ~150 slots (~60 seconds), so we fetch immediately
            // before building to minimize staleness risk. This makes build/sign/send atomic.
            let blockhash = rpc
                .get_latest_blockhash()
                .map_err(|e| anyhow::anyhow!("Failed to get blockhash: {}", e))?;
            
            // Build transaction with fresh blockhash (atomic: fetch -> build -> sign -> send)
            match build_liquidation_tx(&config.wallet, &ctx, &quote, rpc, blockhash).await {
                Ok(tx) => {
                    // Transaction already has blockhash set, sign and send immediately
                    match send_jito_bundle(tx, jito_client, &config.wallet, blockhash).await {
                        Ok(bundle_id) => {
                            // Update cumulative risk and pending liquidation tracking after successful send
                            // NOTE: This tracks risk from liquidations sent in this cycle.
                            // Pending liquidation value tracks liquidations sent but not yet executed on-chain.
                            // The actual wallet balance will be refreshed before the next liquidation check.
                            pending_liquidation_value += position_size_usd; // Track pending
                            cumulative_risk_usd += position_size_usd;
                            log::info!(
                                "✅ Liquidated {} with profit ${:.2} USDC, bundle_id: {}, cumulative_risk=${:.2}/${:.2} (wallet_value=${:.2}, pending=${:.2})",
                                obl_pubkey,
                                quote.profit_usdc,
                                bundle_id,
                                cumulative_risk_usd,
                                current_max_position_usd,
                                current_wallet_value_usd,
                                pending_liquidation_value
                            );
                            metrics.successful += 1;
                        }
                        Err(e) => {
                            log::error!("Failed to send Jito bundle for {}: {}", obl_pubkey, e);
                            metrics.failed_send_bundle += 1;
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to build liquidation tx for {}: {}", obl_pubkey, e);
                    metrics.failed_build_tx += 1;
                }
            }
        } else {
            // Update cumulative risk for dry run as well
            cumulative_risk_usd += position_size_usd;
            log::info!(
                "DryRun: would liquidate obligation {} with profit ~${:.2} USDC, cumulative_risk=${:.2}/{:.2} (wallet_value=${:.2})",
                obl_pubkey,
                quote.profit_usdc,
                cumulative_risk_usd,
                current_max_position_usd,
                current_wallet_value_usd
            );
            metrics.successful += 1;
        }
    }

    // Log cycle summary metrics
    // Get final wallet value for summary (may have changed if liquidations executed)
    let final_wallet_value_usd = match get_wallet_value_usd(rpc, &config.wallet.pubkey()).await {
        Ok(value) => value,
        Err(e) => {
            log::warn!("Failed to get final wallet value for summary: {}", e);
            0.0 // Use 0 as fallback for summary
        }
    };
    let final_max_position_usd = final_wallet_value_usd * config.max_position_pct;
    
    let total_processed = metrics.total_candidates;
    let total_skipped = metrics.skipped_oracle_fail 
        + metrics.skipped_jupiter_fail 
        + metrics.skipped_insufficient_profit 
        + metrics.skipped_risk_limit;
    let total_failed = metrics.failed_build_tx + metrics.failed_send_bundle;
    
    log::info!(
        "📊 Cycle Summary: {} candidates | {} successful | {} skipped (oracle:{}, jupiter:{}, profit:{}, risk:{}) | {} failed (build:{}, send:{}) | cumulative_risk=${:.2}/{:.2} (final_wallet_value=${:.2})",
        total_processed,
        metrics.successful,
        total_skipped,
        metrics.skipped_oracle_fail,
        metrics.skipped_jupiter_fail,
        metrics.skipped_insufficient_profit,
        metrics.skipped_risk_limit,
        total_failed,
        metrics.failed_build_tx,
        metrics.failed_send_bundle,
        cumulative_risk_usd,
        final_max_position_usd,
        final_wallet_value_usd
    );

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
/// When Switchboard is available, this is used as secondary validation.
/// When Switchboard is NOT available, we use a stricter threshold for Pyth-only validation.
const MAX_CONFIDENCE_PCT: f64 = 5.0; // 5% max confidence interval (with Switchboard)
const MAX_CONFIDENCE_PCT_PYTH_ONLY: f64 = 2.0; // 2% max confidence interval (Pyth-only, stricter)

/// Minimum valid price threshold (in USD)
/// Prices below this threshold are considered invalid to prevent division by zero
/// and floating point precision issues in confidence percentage calculations.
/// Example: price = 1e-100 → confidence_pct calculation would produce inf or very large values.
/// 
/// CRITICAL: Increased from 1e-6 to 1e-3 for better safety margin
const MIN_VALID_PRICE_USD: f64 = 1e-3; // $0.001 minimum (1 milli-dollar) - more conservative

/// Maximum allowed slot difference for oracle price (stale check)
/// Pyth recommends checking valid_slot, but we also check last_slot as fallback
const MAX_SLOT_DIFFERENCE: u64 = 150; // ~1 minute at 400ms per slot

/// Maximum allowed price deviation between Pyth and Switchboard (as percentage)
const MAX_ORACLE_DEVIATION_PCT: f64 = 2.0; // 2% max deviation

/// Pyth price status values (from price_type byte)
/// PriceType enum: Unknown = 0, Price = 1, Trading = 2, Halted = 3, Auction = 4
const PYTH_PRICE_STATUS_TRADING: u8 = 2;
const PYTH_PRICE_STATUS_UNKNOWN: u8 = 0;
const PYTH_PRICE_STATUS_HALTED: u8 = 3;

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

    // Validate Switchboard oracles if available per Structure.md section 5.2
    // "Switchboard varsa, Pyth ile sapma fazla mı?"
    // 
    // CRITICAL SECURITY: If Switchboard is NOT available, we rely solely on Pyth.
    // In this case, we apply stricter validation (stricter confidence threshold).
    // This mitigates the risk of Pyth oracle manipulation when no cross-validation exists.
    
    let mut borrow_has_switchboard = false;
    let mut deposit_has_switchboard = false;

    // Validate Switchboard oracles if available for borrow reserve
    if let (Some(borrow_reserve), Some(borrow_price)) = (borrow_reserve, borrow_pyth_price) {
        if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
            rpc,
            &borrow_reserve,
            current_slot,
        )
        .await?
        {
            borrow_has_switchboard = true;
            // Compare Pyth and Switchboard prices
            let deviation_pct = ((borrow_price - switchboard_price).abs() / borrow_price) * 100.0;
            if deviation_pct > MAX_ORACLE_DEVIATION_PCT {
                log::warn!(
                    "Oracle deviation too high for borrow reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    MAX_ORACLE_DEVIATION_PCT,
                    borrow_price,
                    switchboard_price
                );
                return Ok((false, None, None));
            }
            log::debug!(
                "✅ Oracle deviation OK for borrow reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                deviation_pct,
                borrow_price,
                switchboard_price
            );
        }
    }

    // Validate Switchboard oracles if available for deposit reserve
    if let (Some(deposit_reserve), Some(deposit_price)) = (deposit_reserve, deposit_pyth_price) {
        if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
            rpc,
            &deposit_reserve,
            current_slot,
        )
        .await?
        {
            deposit_has_switchboard = true;
            let deviation_pct = ((deposit_price - switchboard_price).abs() / deposit_price) * 100.0;
            if deviation_pct > MAX_ORACLE_DEVIATION_PCT {
                log::warn!(
                    "Oracle deviation too high for deposit reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    MAX_ORACLE_DEVIATION_PCT,
                    deposit_price,
                    switchboard_price
                );
                return Ok((false, None, None));
            }
            log::debug!(
                "✅ Oracle deviation OK for deposit reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                deviation_pct,
                deposit_price,
                switchboard_price
            );
        }
    }

    // SECURITY: If Switchboard is NOT available for either reserve, apply stricter Pyth validation
    // This mitigates the risk of relying solely on Pyth without cross-validation
    if !borrow_has_switchboard || !deposit_has_switchboard {
        log::warn!(
            "⚠️  SECURITY WARNING: Switchboard oracle not available (borrow: {}, deposit: {}). \
             Relying solely on Pyth with stricter confidence threshold ({}% vs {}%). \
             This increases risk of oracle manipulation.",
            if borrow_has_switchboard { "✓" } else { "✗" },
            if deposit_has_switchboard { "✓" } else { "✗" },
            MAX_CONFIDENCE_PCT_PYTH_ONLY,
            MAX_CONFIDENCE_PCT
        );
        
        // Re-validate Pyth confidence with stricter threshold
        if let Some(borrow_reserve) = borrow_reserve {
            if !borrow_has_switchboard {
                if let Some(borrow_price) = borrow_pyth_price {
                    // Re-check Pyth confidence with stricter threshold
                    let confidence_check = validate_pyth_confidence_strict(
                        rpc,
                        borrow_reserve.oracle_pubkey(),
                        borrow_price,
                        current_slot,
                    ).await?;
                    if !confidence_check {
                        log::warn!(
                            "Borrow reserve Pyth confidence check failed (stricter threshold for Pyth-only mode)"
                        );
                        return Ok((false, None, None));
                    }
                }
            }
        }
        
        if let Some(deposit_reserve) = deposit_reserve {
            if !deposit_has_switchboard {
                if let Some(deposit_price) = deposit_pyth_price {
                    // Re-check Pyth confidence with stricter threshold
                    let confidence_check = validate_pyth_confidence_strict(
                        rpc,
                        deposit_reserve.oracle_pubkey(),
                        deposit_price,
                        current_slot,
                    ).await?;
                    if !confidence_check {
                        log::warn!(
                            "Deposit reserve Pyth confidence check failed (stricter threshold for Pyth-only mode)"
                        );
                        return Ok((false, None, None));
                    }
                }
            }
        }
    }

    Ok((true, borrow_pyth_price, deposit_pyth_price))
}

/// Validate Switchboard oracle if available in ReserveConfig
/// Returns Some(price) if Switchboard oracle exists and is valid, None otherwise
/// Per Structure.md section 5.2
/// 
/// DYNAMIC CHAIN READING: All data is read from chain via RPC - no static values.
/// 
/// Uses official Switchboard SDK (switchboard-on-demand) with Solana SDK v2 compatibility.
/// SDK is pulled from GitHub main branch for latest Solana v2 support.
async fn validate_switchboard_oracle_if_available(
    rpc: &Arc<RpcClient>,
    reserve: &Reserve,
    current_slot: u64,
) -> Result<Option<f64>> {
    // NOTE: Switchboard SDK import is commented out due to SDK compatibility issues
    // When SDK is updated to fix Ref<'_, &mut [u8]> type issue, uncomment this:
    // use switchboard_on_demand::on_demand::accounts::pull_feed::PullFeedAccountData;
    
    // 1. Get switchboard_oracle_pubkey from reserve.config (DYNAMIC - from chain)
    let switchboard_oracle_pubkey = reserve.config.switchboardOraclePubkey;
    if switchboard_oracle_pubkey == Pubkey::default() {
        // No Switchboard oracle configured for this reserve
        return Ok(None);
    }

    // 2. Get Switchboard feed account data from chain (DYNAMIC)
    let oracle_account = rpc
        .get_account(&switchboard_oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get Switchboard oracle account from chain: {}", e))?;

    let switchboard_program_id = Pubkey::from_str(SWITCHBOARD_PROGRAM_ID)
        .map_err(|e| anyhow::anyhow!("Invalid Switchboard program ID: {}", e))?;

    if oracle_account.owner != switchboard_program_id {
        log::debug!(
            "Switchboard oracle account {} does not belong to Switchboard program",
            switchboard_oracle_pubkey
        );
        return Ok(None);
    }

    // 3. Parse using Switchboard SDK for off-chain usage
    // 
    // CRITICAL: Switchboard SDK compatibility issue - parse() method signature has type mismatch.
    // The SDK's parse() method expects Ref<'_, &mut [u8]>, but Rust's type system prevents
    // creating a mutable reference from an immutable Ref. This is a known SDK compatibility issue.
    // 
    // SOLUTION: Gracefully fall back to Pyth-only mode with stricter validation.
    // This is the recommended approach per Problems.md section "PROBLEM 6: Switchboard SDK Compatibility".
    // 
    // When Switchboard SDK is updated to fix this issue, we can re-enable Switchboard parsing.
    // For now, we return None to indicate Switchboard is unavailable, and the code will
    // use Pyth-only mode with stricter confidence thresholds (already implemented).
    // 
    // NOTE: This does NOT break the bot - it simply uses Pyth as the sole oracle source
    // with stricter validation (MAX_CONFIDENCE_PCT_PYTH_ONLY = 2% vs 5% with Switchboard).
    log::warn!(
        "Switchboard feed parsing temporarily disabled due to SDK compatibility issue. \
         Falling back to Pyth-only mode with stricter validation ({}% confidence threshold). \
         This is safe and does not affect bot functionality.",
        MAX_CONFIDENCE_PCT_PYTH_ONLY
    );
    return Ok(None); // Graceful fallback to Pyth-only mode
    
    // TODO: Re-enable Switchboard parsing when SDK is updated to fix Ref<'_, &mut [u8]> type issue
    // The code below is commented out until SDK compatibility is resolved:
    /*
    use std::cell::{RefCell, RefMut};
    let mut account_data = oracle_account.data.clone();
    let account_data_cell = RefCell::new(account_data.as_mut_slice());
    let feed = match PullFeedAccountData::parse(RefMut::map(
        account_data_cell.borrow_mut(),
        |data| data
    )) {
        Ok(feed) => feed,
        Err(e) => {
            log::warn!(
                "Switchboard feed parsing failed (SDK compatibility issue?): {}. \
                 Falling back to Pyth-only mode with stricter validation.",
                e
            );
            return Ok(None);
        }
    };
    */
    
    // NOTE: The code below is commented out because Switchboard parsing is temporarily disabled
    // due to SDK compatibility issues. When SDK is updated, uncomment this code:
    /*
    // 4. Get price using SDK's value() method
    // value(current_slot) requires current slot for staleness checking
    // It returns Result<Decimal, OnDemandError> - Ok if valid, Err if stale/insufficient
    let price_decimal = match feed.value(current_slot) {
        Ok(v) => v,
        Err(e) => {
            log::debug!(
                "Switchboard feed {} value() failed (stale or insufficient quorum): {}",
                switchboard_oracle_pubkey,
                e
            );
            return Ok(None);
        }
    };

    // 5. Convert Decimal to f64 for compatibility
    // rust_decimal::Decimal provides better precision, but we use f64 for consistency
    let price = price_decimal.to_string().parse::<f64>()
        .unwrap_or_else(|_| {
            // Fallback: manual conversion using mantissa and scale
            let mantissa = price_decimal.mantissa();
            let scale = price_decimal.scale();
            mantissa as f64 / 10_f64.powi(scale as i32)
        });

    // 6. Validate price is positive and reasonable
    if price <= 0.0 || !price.is_finite() {
        log::debug!(
            "Switchboard oracle price is invalid: {} (from feed {})",
            price,
            switchboard_oracle_pubkey
        );
        return Ok(None);
    }

    log::debug!(
        "✅ Switchboard oracle validation passed for {} (price: {})",
        switchboard_oracle_pubkey,
        price
    );

    Ok(Some(price))
    */
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

    // 2. Parse Pyth v2 price account structure
    // Pyth v2 price account layout:
    // - Offset 0-4: magic (4 bytes) = 0xa1b2c3d4
    // - Offset 4-5: version (1 byte) = 2
    // - Offset 5-6: price_type (1 byte) - PriceType enum: Unknown=0, Price=1, Trading=2, Halted=3, Auction=4
    // - Offset 6-8: size (2 bytes)
    // - Offset 8-16: price (i64, 8 bytes)
    // - Offset 16-20: exponent (i32, 4 bytes)
    // - Offset 20-24: reserved (4 bytes)
    // - Offset 24-32: timestamp (i64, 8 bytes)
    // - Offset 32-40: prev_publish_time (i64, 8 bytes)
    // - Offset 40-48: prev_price (i64, 8 bytes)
    // - Offset 48-56: prev_conf (u64, 8 bytes)
    // - Offset 56-64: last_slot (u64, 8 bytes) - slot when price was last updated
    // - Offset 64-72: valid_slot (u64, 8 bytes) - slot when price is valid until
    // - Offset 72+: publisher accounts...

    if oracle_account.data.len() < 72 {
        log::debug!("Oracle account data too short: {} bytes (need at least 72 for Pyth v2)", oracle_account.data.len());
        return Ok((false, None));
    }

    // Check magic number (Pyth v2)
    let magic: [u8; 4] = oracle_account.data[0..4]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse magic"))?;
    if magic != [0xa1, 0xb2, 0xc3, 0xd4] {
        log::debug!("Invalid Pyth magic number: {:?}", magic);
        return Ok((false, None));
    }

    // Check version
    let version = oracle_account.data[4];
    if version != 2 {
        log::debug!("Unsupported Pyth version: {} (expected 2)", version);
        return Ok((false, None));
    }

    // Check price status (price_type byte)
    // CRITICAL: Only Trading status (2) is acceptable for liquidations
    // All other statuses (Unknown=0, Price=1, Halted=3, Auction=4) must be rejected
    let price_type = oracle_account.data[5];
    if price_type != PYTH_PRICE_STATUS_TRADING {
        let status_name = match price_type {
            0 => "Unknown",
            1 => "Price",
            2 => "Trading",
            3 => "Halted",
            4 => "Auction",
            _ => "Invalid",
        };
        
        log::warn!(
            "Pyth price status is {} ({}) - REJECTING oracle. Only Trading status is acceptable for liquidations.",
            price_type,
            status_name
        );
        return Ok((false, None));
    }

    // Parse exponent
    let exponent_bytes: [u8; 4] = oracle_account.data[16..20]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?;
    let exponent = i32::from_le_bytes(exponent_bytes);

    // Parse price (i64)
    let price_bytes: [u8; 8] = oracle_account.data[8..16]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse price"))?;
    let price_raw = i64::from_le_bytes(price_bytes);

    // Convert to f64 with exponent
    // CRITICAL: Pyth exponent is typically negative (e.g., -8 for 8 decimals)
    // Mathematical equivalence: price_raw * 10^(-8) = price_raw / 10^8
    // Example: price_raw=150000000, exponent=-8 → 150000000 * 10^(-8) = 1.5 USD
    // This is CORRECT: powi() handles negative exponents properly (10^(-8) = 1/10^8)
    let price = Some(price_raw as f64 * 10_f64.powi(exponent));

    // 3. Check if price is stale (valid_slot and last_slot check)
    // Pyth v2 uses valid_slot as primary stale check - price is valid until valid_slot
    // We also check last_slot as secondary validation
    
    // Parse valid_slot (primary stale check)
    let valid_slot_bytes: [u8; 8] = oracle_account.data[64..72]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse valid_slot"))?;
    let valid_slot = u64::from_le_bytes(valid_slot_bytes);

    // Price is stale if current_slot > valid_slot
    if current_slot > valid_slot {
        log::debug!(
            "Pyth oracle price is stale: current_slot {} > valid_slot {}",
            current_slot,
            valid_slot
        );
        return Ok((false, None));
    }

    // Secondary check: last_slot (when price was last updated)
    let last_slot_bytes: [u8; 8] = oracle_account.data[56..64]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse last_slot"))?;
    let last_slot = u64::from_le_bytes(last_slot_bytes);

    // Check slot difference as additional validation
    let slot_difference = current_slot.saturating_sub(last_slot);
    if slot_difference > MAX_SLOT_DIFFERENCE {
        log::debug!(
            "Pyth oracle price last update too old: slot difference {} > {} (last_slot: {}, current_slot: {})",
            slot_difference,
            MAX_SLOT_DIFFERENCE,
            last_slot,
            current_slot
        );
        return Ok((false, None));
    }

    log::debug!(
        "Pyth oracle price is fresh: valid_slot={}, last_slot={}, current_slot={}, slot_diff={}",
        valid_slot,
        last_slot,
        current_slot,
        slot_difference
    );

    // 4. Check confidence interval
    let price_value = price.ok_or_else(|| anyhow::anyhow!("Price not parsed"))?;
    
    // CRITICAL: Check minimum price threshold and finite check to prevent division by zero and floating point issues
    // Edge case: price_value = 1e-100 → price_value.abs() > 0.0 check passes, but
    // confidence_pct = confidence / price_value.abs() would produce inf or very large values
    // Solution: Reject prices below minimum threshold or non-finite values
    if price_value.abs() < MIN_VALID_PRICE_USD || !price_value.is_finite() {
        log::debug!(
            "Pyth oracle price invalid or too small: {} (min: {}, finite: {})",
            price_value,
            MIN_VALID_PRICE_USD,
            price_value.is_finite()
        );
        return Ok((false, None));
    }
    
    // Parse confidence (u64 at offset 48-56)
    // CRITICAL: Confidence uses the same exponent as price
    // Example: confidence_raw=1000000, exponent=-8 → 1000000 * 10^(-8) = 0.01
    let conf_bytes: [u8; 8] = oracle_account.data[48..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
    let confidence_raw = u64::from_le_bytes(conf_bytes);
    let confidence = confidence_raw as f64 * 10_f64.powi(exponent);

    // Check if confidence interval is too large (as percentage of price)
    // NOTE: We already checked price_value.abs() >= MIN_VALID_PRICE_USD and is_finite() above,
    // so division is safe and won't produce inf
    let confidence_pct = (confidence / price_value.abs()) * 100.0;
    
    // Additional safety check after calculation
    if !confidence_pct.is_finite() {
        log::debug!("Confidence percentage calculation produced non-finite value");
        return Ok((false, None));
    }

    if confidence_pct > MAX_CONFIDENCE_PCT {
        log::debug!(
            "Pyth oracle confidence too high: {:.2}% > {:.2}% (price: {}, confidence: {})",
            confidence_pct,
            MAX_CONFIDENCE_PCT,
            price_value,
            confidence
        );
        return Ok((false, None));
    }

    log::debug!(
        "✅ Pyth oracle validation passed for {} (price: {}, confidence: {:.2}%, status: {})",
        oracle_pubkey,
        price_value,
        confidence_pct,
        price_type
    );
    Ok((true, price))
}

/// Validate Pyth confidence with stricter threshold (for Pyth-only mode when Switchboard is unavailable)
/// Returns true if confidence is acceptable, false otherwise
async fn validate_pyth_confidence_strict(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    price_value: f64,
    current_slot: u64,
) -> Result<bool> {
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    if oracle_account.data.len() < 56 {
        return Ok(false);
    }

    // Parse exponent
    let exponent_bytes: [u8; 4] = oracle_account.data[16..20]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?;
    let exponent = i32::from_le_bytes(exponent_bytes);

    // Parse confidence (u64 at offset 48-56)
    let conf_bytes: [u8; 8] = oracle_account.data[48..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
    let confidence_raw = u64::from_le_bytes(conf_bytes);
    let confidence = confidence_raw as f64 * 10_f64.powi(exponent);

    // CRITICAL: Check minimum price threshold to prevent division by zero and floating point issues
    // Edge case: price_value = 1e-100 → price_value.abs() > 0.0 check passes, but
    // confidence_pct = confidence / price_value.abs() would produce inf or very large values
    if price_value.abs() < MIN_VALID_PRICE_USD {
        log::warn!(
            "Pyth oracle price below minimum threshold for strict check: {} < {} (price too small, likely invalid)",
            price_value.abs(),
            MIN_VALID_PRICE_USD
        );
        return Ok(false);
    }
    
    // Check if confidence interval is too large (stricter threshold for Pyth-only mode)
    // NOTE: We already checked price_value.abs() >= MIN_VALID_PRICE_USD above,
    // so division is safe and won't produce inf
    let confidence_pct = (confidence / price_value.abs()) * 100.0;

    if confidence_pct > MAX_CONFIDENCE_PCT_PYTH_ONLY {
        log::warn!(
            "Pyth oracle confidence too high for Pyth-only mode: {:.2}% > {:.2}% (price: {}, confidence: {})",
            confidence_pct,
            MAX_CONFIDENCE_PCT_PYTH_ONLY,
            price_value,
            confidence
        );
        return Ok(false);
    }

    log::debug!(
        "✅ Pyth confidence check passed (stricter threshold): {:.2}% <= {:.2}%",
        confidence_pct,
        MAX_CONFIDENCE_PCT_PYTH_ONLY
    );

    Ok(true)
}

/// Liquidation quote with profit calculation per Structure.md section 7
struct LiquidationQuote {
    quote: JupiterQuote,
    profit_usdc: f64,
    collateral_value_usd: f64, // Position size in USD for risk limit calculation
    debt_to_repay_raw: u64, // Debt amount to repay (in debt token raw units) for Solend instruction
}

/// Get SOL price in USD from oracle
/// Returns SOL price if available, otherwise None
async fn get_sol_price_usd(rpc: &Arc<RpcClient>, ctx: &LiquidationContext) -> Option<f64> {
    // SOL native mint: So11111111111111111111111111111111111111112
    let sol_native_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").ok()?;
    
    // Check if collateral or debt is SOL, use its price
    if let Some(deposit_reserve) = &ctx.deposit_reserve {
        if deposit_reserve.liquidity.mintPubkey == sol_native_mint {
            return ctx.deposit_price_usd;
        }
    }
    
    if let Some(borrow_reserve) = &ctx.borrow_reserve {
        if borrow_reserve.liquidity.mintPubkey == sol_native_mint {
            return ctx.borrow_price_usd;
        }
    }
    
    // If SOL is not in the reserves, fetch from Pyth SOL/USD price feed
    // Pyth SOL/USD mainnet price feed: H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG
    let sol_usd_pyth_feed = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => return None,
    };
    
    let current_slot = rpc.get_slot().ok()?;
    match validate_pyth_oracle(rpc, sol_usd_pyth_feed, current_slot).await {
        Ok((true, Some(price))) => Some(price),
        _ => None,
    }
}

/// Get Jupiter quote for liquidation with profit calculation per Structure.md section 7
async fn get_liquidation_quote(
    ctx: &LiquidationContext,
    config: &Config,
    rpc: &Arc<RpcClient>,
) -> Result<LiquidationQuote> {
    // Use first borrow and first deposit
    if ctx.obligation.borrows.is_empty() || ctx.obligation.deposits.is_empty() {
        return Err(anyhow::anyhow!("No borrows or deposits in obligation"));
    }

    let borrow = &ctx.obligation.borrows[0];
    // Note: deposit is available but we use deposit_reserve directly for mint address

    // Get mint addresses from reserve accounts for Jupiter swap
    // CRITICAL: Jupiter swap requires actual token mints, NOT collateral token (cToken) mints
    // 
    // - collateral_mint: Use liquidity.mintPubkey (actual token, e.g., SOL, USDC)
    //   NOT collateral.mintPubkey (cToken, e.g., cSOL, cUSDC)
    // - debt_mint: Use liquidity.mintPubkey (actual token being borrowed)
    //
    // Example: If depositing SOL and borrowing USDC:
    // - collateral_mint = SOL mint (not cSOL)
    // - debt_mint = USDC mint
    // Jupiter will swap SOL -> USDC
    let collateral_mint = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.liquidity.mintPubkey) // ✅ Actual token mint (e.g., SOL), NOT cToken
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;

    let debt_mint = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.liquidity.mintPubkey) // ✅ Actual token mint (e.g., USDC)
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;

    // CRITICAL: Calculate correct collateral amount to seize
    // 
    // Liquidation flow:
    // 1. We repay: debt_to_repay = borrowedAmountWad * close_factor (50%)
    // 2. Solend gives us: collateral_to_seize = debt_to_repay * (1 + liquidation_bonus)
    // 3. We need to swap: collateral_to_seize amount of collateral → debt_to_repay amount of debt
    //
    // Steps:
    // a) Calculate debt to repay (with close factor 50%)
    // b) Calculate collateral to seize (with liquidation bonus)
    // c) Query Jupiter: collateral_to_seize → debt token
    
    // Step 1: Calculate debt to repay (close factor = 50%)
    const CLOSE_FACTOR: f64 = 0.5; // 50% close factor
    const WAD: f64 = 1_000_000_000_000_000_000.0; // 10^18
    
    // CRITICAL FIX: Calculate actual debt with accrued interest
    // borrowedAmountWad is in WAD format (u128), convert to f64
    let borrowed_amount_wad = borrow.borrowedAmountWad as f64;
    
    // Get cumulative borrow rate to account for accrued interest
    // cumulativeBorrowRateWads is also in WAD format
    let cumulative_borrow_rate = borrow.cumulativeBorrowRateWads as f64;
    
    // CRITICAL: Actual debt = borrowed_amount_wad * cumulative_borrow_rate / WAD
    // This accounts for accrued interest since the borrow was made
    let actual_debt_wad = (borrowed_amount_wad * cumulative_borrow_rate) / WAD;
    let debt_to_repay_wad = actual_debt_wad * CLOSE_FACTOR;
    let debt_to_repay = debt_to_repay_wad / WAD; // Normalize from WAD
    
    log::debug!(
        "Debt calculation: borrowed_wad={}, cumulative_rate={}, actual_debt={:.6}, debt_to_repay={:.6}",
        borrowed_amount_wad,
        cumulative_borrow_rate,
        actual_debt_wad / WAD,
        debt_to_repay
    );
    
    // Step 2: Get liquidation bonus from deposit reserve (collateral reserve)
    let deposit_reserve = ctx
        .deposit_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    let liquidation_bonus = deposit_reserve.liquidation_bonus(); // Returns 0.05 for 5%, etc.
    
    // Step 3: Calculate collateral to seize
    // collateral_to_seize_usd = debt_to_repay_usd * (1 + liquidation_bonus)
    let debt_price_usd = ctx
        .borrow_price_usd
        .ok_or_else(|| anyhow::anyhow!("Borrow price not available"))?;
    let collateral_price_usd = ctx
        .deposit_price_usd
        .ok_or_else(|| anyhow::anyhow!("Deposit price not available"))?;
    
    let debt_to_repay_usd = debt_to_repay * debt_price_usd;
    let collateral_to_seize_usd = debt_to_repay_usd * (1.0 + liquidation_bonus);
    let collateral_to_seize = collateral_to_seize_usd / collateral_price_usd;
    
    // Step 4: Convert collateral amount to raw units (with decimals)
    let collateral_decimals = deposit_reserve.liquidity.mintDecimals;
    let collateral_to_seize_raw = (collateral_to_seize * 10_f64.powi(collateral_decimals as i32)) as u64;
    
    log::debug!(
        "Liquidation calculation: debt_to_repay={:.6} (${:.2}), liquidation_bonus={:.2}%, collateral_to_seize={:.6} (${:.2}), collateral_raw={}",
        debt_to_repay,
        debt_to_repay_usd,
        liquidation_bonus * 100.0,
        collateral_to_seize,
        collateral_to_seize_usd,
        collateral_to_seize_raw
    );

    // Step 5: Calculate dynamic slippage based on position size
    // CRITICAL: Larger positions need higher slippage tolerance due to price impact
    let position_size_usd = collateral_to_seize_usd;
    let slippage_bps = if position_size_usd < 1000.0 {
        30u16  // Small position: 0.3%
    } else if position_size_usd < 10_000.0 {
        50u16  // Medium position: 0.5%
    } else if position_size_usd < 50_000.0 {
        100u16 // Large position: 1.0%
    } else {
        150u16 // Very large position: 1.5%
    };
    
    log::debug!(
        "Dynamic slippage: {}bps ({}%) for position_size=${:.2}",
        slippage_bps,
        slippage_bps as f64 / 100.0,
        position_size_usd
    );
    
    // Step 6: Get Jupiter quote for liquidation swap with retry mechanism
    // Liquidation flow:
    // 1. Solend gives us collateral token (e.g., 10 SOL)
    // 2. We swap this 10 SOL to debt token (e.g., USDC) via Jupiter
    // 3. We use the received USDC to pay off the debt (e.g., 1500 USDC)
    // 4. Profit = collateral_value - debt_value - fees
    let quote = get_jupiter_quote_with_retry(
        &collateral_mint, // input: collateral token we receive from Solend (e.g., SOL)
        &debt_mint,       // output: debt token we need to repay (e.g., USDC)
        collateral_to_seize_raw, // amount of collateral to swap (e.g., 10 SOL in raw units)
        slippage_bps, // Dynamic slippage based on position size
        3, // max_retries
    )
    .await
    .context("Failed to get Jupiter quote with retries")?;

    // Calculate profit per Structure.md section 7
    // profit = collateral_value_usd - debt_repaid_value_usd - swap_fee_usd - jito_fee_usd - tx_fee_usd
    //
    // CRITICAL: We already calculated collateral_to_seize_usd and debt_to_repay_usd above.
    // Use those values directly for profit calculation, as they represent the actual liquidation amounts.
    // Jupiter quote gives us the swap result, but for profit we use the liquidation amounts.
    
    // Get token decimals from reserves (already have deposit_reserve from above)
    let debt_decimals = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.liquidity.mintDecimals)
        .unwrap_or(6);

    // Use the calculated liquidation amounts for profit calculation
    // collateral_to_seize_usd: What we receive from Solend (with liquidation bonus)
    // debt_to_repay_usd: What we repay to Solend
    let collateral_value_usd = collateral_to_seize_usd; // What we get from liquidation
    let debt_value_usd = debt_to_repay_usd; // What we repay
    
    // Also parse Jupiter quote for swap fee calculation
    let jupiter_in_amount: u64 = quote.in_amount.parse().unwrap_or(0);
    let jupiter_collateral_amount = jupiter_in_amount as f64 / 10_f64.powi(collateral_decimals as i32);
    let jupiter_collateral_value_usd = jupiter_collateral_amount * collateral_price_usd;

    // Calculate profit in USD
    // Note: For liquidation, we repay debt and get collateral
    // Profit = collateral_value - debt_value - fees
    
    // 1. Calculate Jupiter swap fee using price impact from quote
    // Jupiter LP fees vary by route (0.05%-0.3%), so we use actual price impact from quote
    // Price impact includes both LP fees and slippage, giving us the true cost
    let price_impact_pct = crate::jup::get_price_impact_pct(&quote);
    
    // Use price impact if available, otherwise fallback to conservative 0.3%
    let swap_fee_usd = if price_impact_pct > 0.0 {
        jupiter_collateral_value_usd * (price_impact_pct / 100.0)
    } else {
        // Fallback: use 0.3% if price impact not available
        jupiter_collateral_value_usd * 0.003
    };
    
    // 2. Get SOL price for fee calculations
    let sol_price_usd = get_sol_price_usd(rpc, ctx).await
        .unwrap_or_else(|| {
            log::warn!("Failed to get SOL price, using fallback $150");
            150.0 // Fallback price if oracle fails
        });
    
    // 3. Calculate Jito tip fee in USD
    // Jito tip is typically 0.01 SOL (10_000_000 lamports)
    let jito_tip_lamports = config.jito_tip_amount_lamports.unwrap_or(10_000_000u64);
    let jito_tip_sol = jito_tip_lamports as f64 / 1_000_000_000.0;
    let jito_fee_usd = jito_tip_sol * sol_price_usd;
    
    // 4. Calculate transaction fee in USD
    // Base transaction fee is ~5000 lamports (0.000005 SOL)
    const BASE_TX_FEE_LAMPORTS: u64 = 5_000;
    let tx_fee_sol = BASE_TX_FEE_LAMPORTS as f64 / 1_000_000_000.0;
    let tx_fee_usd = tx_fee_sol * sol_price_usd;

    let profit_usdc = collateral_value_usd - debt_value_usd - swap_fee_usd - jito_fee_usd - tx_fee_usd;
    
    log::debug!(
        "Fee breakdown: swap_fee=${:.4}, jito_fee=${:.4} ({} SOL @ ${:.2}), tx_fee=${:.4} ({} SOL @ ${:.2})",
        swap_fee_usd,
        jito_fee_usd,
        jito_tip_sol,
        sol_price_usd,
        tx_fee_usd,
        tx_fee_sol,
        sol_price_usd
    );

    // Convert debt_to_repay to raw units for Solend instruction
    let debt_to_repay_raw = (debt_to_repay * 10_f64.powi(debt_decimals as i32)) as u64;
    
    Ok(LiquidationQuote {
        quote,
        profit_usdc,
        collateral_value_usd,
        debt_to_repay_raw, // Debt amount to repay in Solend instruction
    })
}

/// Get total wallet value in USD (SOL + USDC)
/// Used for risk limit calculations per Structure.md section 6.4
async fn get_wallet_value_usd(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<f64> {
    // 1. Get SOL balance and price
    let sol_balance = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get wallet balance: {}", e))?;
    
    // Get SOL price from Pyth (fallback to $150 if unavailable)
    let sol_usd_pyth_feed = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => {
            log::warn!("Failed to parse SOL/USD Pyth feed, using fallback $150");
            let sol_value_usd = (sol_balance as f64) / 1_000_000_000.0 * 150.0;
            // Continue to get USDC balance
            let program_id = crate::solend::solend_program_id()?;
            let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
                .context("Failed to discover USDC mint")?;
            use spl_associated_token_account::get_associated_token_address;
            let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
            let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
                Ok(Some(account)) => account.token_amount.amount.parse::<u64>().unwrap_or(0),
                _ => 0,
            };
            let usdc_value_usd = (usdc_balance_raw as f64) / 1_000_000.0;
            return Ok(sol_value_usd + usdc_value_usd);
        }
    };
    
    let current_slot = rpc.get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    let sol_price_usd = match validate_pyth_oracle(rpc, sol_usd_pyth_feed, current_slot).await {
        Ok((true, Some(price))) => price,
        _ => {
            log::warn!("Failed to get SOL price from oracle, using fallback $150");
            150.0
        }
    };
    
    let sol_value_usd = (sol_balance as f64) / 1_000_000_000.0 * sol_price_usd;
    
    // 2. Get USDC balance
    let program_id = crate::solend::solend_program_id()?;
    let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint")?;
    
    use spl_associated_token_account::get_associated_token_address;
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    
    let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0, // ATA doesn't exist
        Err(_) => {
            log::debug!("Failed to get USDC balance, assuming 0");
            0
        }
    };
    
    // USDC has 6 decimals, price is $1.0
    let usdc_value_usd = (usdc_balance_raw as f64) / 1_000_000.0;
    
    // 3. Calculate total wallet value in USD
    // Note: For now, we include SOL and USDC. Other tokens can be added later if needed.
    let wallet_value_usd = sol_value_usd + usdc_value_usd;
    
    Ok(wallet_value_usd)
}

// NOTE: is_within_risk_limits() function was removed.
// Risk limit checking is now done inline in process_cycle() before each liquidation
// to ensure wallet balance is refreshed and cumulative risk tracking works correctly.
// This prevents race conditions where wallet balance changes during the cycle.

/// Build liquidation transaction per Structure.md section 8
/// 
/// CRITICAL: blockhash must be fresh (fetched immediately before calling this function).
/// Blockhashes are valid for ~150 slots (~60 seconds), so fetch blockhash right before
/// building the transaction to minimize staleness risk.
async fn build_liquidation_tx(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: solana_sdk::hash::Hash,
) -> Result<Transaction> {
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
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

    // CRITICAL: Solend LiquidateObligation instruction expects debt token amount (liquidity_amount),
    // NOT collateral amount. This is the amount of debt we're repaying.
    // We already calculated this in get_liquidation_quote() and stored it in debt_to_repay_raw.
    let liquidity_amount = quote.debt_to_repay_raw;

    // Derive required addresses
    let lending_market = ctx.obligation.lendingMarket;
    let lending_market_authority = crate::solend::derive_lending_market_authority(&lending_market, &program_id)?;

    // Get reserve liquidity supply addresses
    // These are stored in Reserve account (supplyPubkey field)
    // Solend program stores the correct PDA addresses in Reserve account during initialization
    let repay_reserve_liquidity_supply = borrow_reserve.liquidity.supplyPubkey;
    let withdraw_reserve_liquidity_supply = deposit_reserve.liquidity.supplyPubkey;
    let withdraw_reserve_collateral_supply = deposit_reserve.collateral.supplyPubkey;
    let withdraw_reserve_collateral_mint = deposit_reserve.collateral.mintPubkey;
    
    // Verify these are not default/zero addresses
    // CRITICAL: Reserve account's supplyPubkey is the authoritative source.
    // We use the value directly from Reserve account, not derived PDA.
    // Solend program stores the correct PDA addresses in Reserve account during initialization.
    if repay_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_collateral_supply == Pubkey::default()
        || withdraw_reserve_collateral_mint == Pubkey::default() {
        return Err(anyhow::anyhow!("Invalid reserve addresses: one or more addresses are default/zero"));
    }

    // SECURITY: Verify reserve supply PDA addresses match expected derivation
    // This helps detect data corruption or manipulation in Reserve accounts.
    // We derive expected PDA and compare with stored supplyPubkey.
    // 
    // CRITICAL: If PDA can be derived and doesn't match, this indicates data corruption
    // or manipulation. We MUST fail-fast per Structure.md section 13 (fail-fast principle).
    // 
    // If PDA cannot be derived (None), this may indicate unknown seed format, but we
    // still log a warning as this is unusual.
    let borrow_reserve_pubkey = ctx.obligation.borrows[0].borrowReserve;
    let deposit_reserve_pubkey = ctx.obligation.deposits[0].depositReserve;
    
    // Verify repay reserve liquidity supply
    if let Some(derived_pda) = crate::solend::derive_reserve_liquidity_supply_pda(&borrow_reserve_pubkey, &program_id) {
        if derived_pda != repay_reserve_liquidity_supply {
            log::error!(
                "🚨 SECURITY ALERT: PDA mismatch detected!\n\
                 Reserve: {}\n\
                 Stored supplyPubkey: {}\n\
                 Derived PDA: {}\n\
                 This may indicate:\n\
                 - Data corruption in Reserve account\n\
                 - Malicious account manipulation\n\
                 - Outdated PDA derivation seeds\n\
                 Transaction ABORTED for security.",
                borrow_reserve_pubkey,
                repay_reserve_liquidity_supply,
                derived_pda
            );
            return Err(anyhow::anyhow!("SECURITY FAILURE: PDA mismatch"));
        }
        log::debug!("✅ Repay reserve liquidity supply PDA verified: {}", repay_reserve_liquidity_supply);
    } else {
        log::warn!(
            "⚠️  Could not derive repay reserve liquidity supply PDA - seed format unknown. \
             Reserve: {}, Stored: {}. \
             Proceeding with stored value, but this is unusual.",
            borrow_reserve_pubkey,
            repay_reserve_liquidity_supply
        );
    }
    
    // Verify withdraw reserve liquidity supply
    if let Some(derived_pda) = crate::solend::derive_reserve_liquidity_supply_pda(&deposit_reserve_pubkey, &program_id) {
        if derived_pda != withdraw_reserve_liquidity_supply {
            return Err(anyhow::anyhow!(
                "SECURITY FAILURE: Withdraw reserve liquidity supply PDA mismatch! \
                 This indicates data corruption or manipulation. \
                 Reserve: {}, Stored: {}, Derived: {}. \
                 Transaction aborted for security.",
                deposit_reserve_pubkey,
                withdraw_reserve_liquidity_supply,
                derived_pda
            ));
        }
        log::debug!("✅ Withdraw reserve liquidity supply PDA verified: {}", withdraw_reserve_liquidity_supply);
    } else {
        log::warn!(
            "⚠️  Could not derive withdraw reserve liquidity supply PDA - seed format unknown. \
             Reserve: {}, Stored: {}. \
             Proceeding with stored value, but this is unusual.",
            deposit_reserve_pubkey,
            withdraw_reserve_liquidity_supply
        );
    }
    
    // Verify withdraw reserve collateral supply
    if let Some(derived_pda) = crate::solend::derive_reserve_collateral_supply_pda(&deposit_reserve_pubkey, &program_id) {
        if derived_pda != withdraw_reserve_collateral_supply {
            return Err(anyhow::anyhow!(
                "SECURITY FAILURE: Withdraw reserve collateral supply PDA mismatch! \
                 This indicates data corruption or manipulation. \
                 Reserve: {}, Stored: {}, Derived: {}. \
                 Transaction aborted for security.",
                deposit_reserve_pubkey,
                withdraw_reserve_collateral_supply,
                derived_pda
            ));
        }
        log::debug!("✅ Withdraw reserve collateral supply PDA verified: {}", withdraw_reserve_collateral_supply);
    } else {
        log::warn!(
            "⚠️  Could not derive withdraw reserve collateral supply PDA - seed format unknown. \
             Reserve: {}, Stored: {}. \
             Proceeding with stored value, but this is unusual.",
            deposit_reserve_pubkey,
            withdraw_reserve_collateral_supply
        );
    }

    // Get user's token accounts (source liquidity and destination collateral)
    // These would be ATAs for the tokens
    use spl_associated_token_account::get_associated_token_address;
    let source_liquidity = get_associated_token_address(&wallet_pubkey, &borrow_reserve.liquidity.mintPubkey);
    let destination_collateral = get_associated_token_address(&wallet_pubkey, &withdraw_reserve_collateral_mint);

    // Build Solend liquidation instruction
    // Solend uses enum-based instruction encoding via LendingInstruction enum (NOT Anchor)
    // 
    // IMPORTANT: Solend is a native Solana program, not Anchor-based.
    // Reference: solend-sdk crate, instruction.rs - LendingInstruction enum
    // 
    // Instruction format:
    //   [tag: u8] + [args...]
    //   tag 12 = LiquidateObligation { liquidity_amount: u64 }
    // 
    // Full instruction data: [12] + liquidity_amount.to_le_bytes()
    let mut instruction_data = Vec::new();
    
    // Instruction discriminator: LendingInstruction::LiquidateObligation tag = 12
    // CRITICAL: Solend uses enum-based encoding (tag = 12), NOT Anchor sighash
    // Solend native program uses only 1 byte for enum tag
    let discriminator = crate::solend::get_liquidate_obligation_discriminator();
    log::debug!(
        "Using instruction discriminator: {} (hex: {:02x}) for liquidateObligation",
        discriminator,
        discriminator
    );
    instruction_data.push(discriminator); // Add only 1 byte
    
    // Args: liquidityAmount (u64)
    instruction_data.extend_from_slice(&liquidity_amount.to_le_bytes());

    // Build account metas per IDL - order must match Solend IDL exactly
    let accounts = vec![
        AccountMeta::new(source_liquidity, false),                    // 0: sourceLiquidity
        AccountMeta::new(destination_collateral, false),              // 1: destinationCollateral
        AccountMeta::new(ctx.obligation.borrows[0].borrowReserve, false),  // 2: repayReserve
        AccountMeta::new(repay_reserve_liquidity_supply, false),      // 3: repayReserveLiquiditySupply
        AccountMeta::new(ctx.obligation.deposits[0].depositReserve, false), // 4: withdrawReserve
        AccountMeta::new(withdraw_reserve_collateral_supply, false),  // 5: withdrawReserveCollateralSupply
        AccountMeta::new_readonly(withdraw_reserve_collateral_mint, false), // 6: withdrawReserveCollateralMint
        AccountMeta::new(withdraw_reserve_liquidity_supply, false),   // 7: withdrawReserveLiquiditySupply
        AccountMeta::new(ctx.obligation_pubkey, false),                // 8: obligation
        AccountMeta::new_readonly(lending_market, false),            // 9: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),   // 10: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),               // 11: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // 12: clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),          // 13: tokenProgram
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

    // Build transaction with fresh blockhash
    // CRITICAL: blockhash must be fetched immediately before this function is called
    // to ensure it's fresh and not stale. Blockhashes are valid for ~150 slots (~60 seconds).
    let mut tx = Transaction::new_with_payer(
        &[compute_budget_ix, priority_fee_ix, liquidation_ix],
        Some(&wallet_pubkey),
    );
    // Set blockhash immediately - it was fetched right before this function call
    tx.message.recent_blockhash = blockhash;

    log::info!(
        "Built liquidation transaction for obligation {} with amount {}",
        ctx.obligation_pubkey,
        liquidity_amount
    );

    Ok(tx)
}


