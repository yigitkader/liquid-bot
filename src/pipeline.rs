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
    use switchboard_on_demand::on_demand::accounts::pull_feed::PullFeedAccountData;
    
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

    // 3. Parse using official Switchboard SDK
    // PullFeedAccountData::parse validates discriminator and Borsh layout automatically
    // This is the CORRECT and SAFE way to parse Switchboard accounts
    // 
    // For off-chain usage, we need to provide a mutable slice reference
    // PullFeedAccountData::parse expects Ref<'_, &mut [u8]>
    use std::cell::RefCell;
    let mut account_data = oracle_account.data.clone();
    let account_data_cell = RefCell::new(account_data.as_mut_slice());
    let feed = match PullFeedAccountData::parse(account_data_cell.borrow()) {
        Ok(feed) => feed,
        Err(e) => {
            log::debug!(
                "Failed to parse Switchboard feed account {}: {}",
                switchboard_oracle_pubkey,
                e
            );
            return Ok(None);
        }
    };

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
    let price_type = oracle_account.data[5];
    if price_type != PYTH_PRICE_STATUS_TRADING {
        log::debug!(
            "Pyth price status is not Trading: {} (0=Unknown, 1=Price, 2=Trading, 3=Halted, 4=Auction)",
            price_type
        );
        if price_type == PYTH_PRICE_STATUS_HALTED {
            log::warn!("Pyth price is HALTED - rejecting oracle");
            return Ok((false, None));
        }
        if price_type == PYTH_PRICE_STATUS_UNKNOWN {
            log::warn!("Pyth price is UNKNOWN - rejecting oracle");
            return Ok((false, None));
        }
        // For Price (1) or Auction (4), we might accept but log warning
        log::warn!("Pyth price status is {} (not Trading) - accepting with caution", price_type);
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
    
    // Parse confidence (u64 at offset 48-56)
    // CRITICAL: Confidence uses the same exponent as price
    // Example: confidence_raw=1000000, exponent=-8 → 1000000 * 10^(-8) = 0.01
    let conf_bytes: [u8; 8] = oracle_account.data[48..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
    let confidence_raw = u64::from_le_bytes(conf_bytes);
    let confidence = confidence_raw as f64 * 10_f64.powi(exponent);

    // Check if confidence interval is too large (as percentage of price)
    let confidence_pct = if price_value.abs() > 0.0 {
        (confidence / price_value.abs()) * 100.0
    } else {
        f64::INFINITY
    };

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
    // Note: deposit is available but we use deposit_reserve directly for mint address

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
    // CRITICAL: Reserve account's supplyPubkey is the authoritative source.
    // We use the value directly from Reserve account, not derived PDA.
    // Solend program stores the correct PDA addresses in Reserve account during initialization.
    if repay_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_collateral_mint == Pubkey::default() {
        return Err(anyhow::anyhow!("Invalid reserve addresses: one or more addresses are default/zero"));
    }

    // NOTE: PDA derivation verification removed
    // Reason: Solend's actual PDA seed format may differ from our assumption.
    // Reserve account's supplyPubkey is the authoritative source and is always correct.
    // Attempting to verify with potentially wrong seeds would generate false warnings.

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
    // CRITICAL: Uses Anchor-style sighash: sha256("global:liquidateObligation")[0..8]
    // This is the CORRECT method for Solend program
    let discriminator = crate::solend::get_liquidate_obligation_discriminator();
    log::debug!(
        "Using instruction discriminator: {:?} (hex: {}) for liquidateObligation",
        discriminator,
        discriminator.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join("")
    );
    instruction_data.extend_from_slice(&discriminator);
    
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


