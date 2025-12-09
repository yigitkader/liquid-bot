// Liquidation quote and profit calculation module
// Moved from pipeline.rs to reduce code size and improve maintainability

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::env;
use std::str::FromStr;
use std::sync::Arc;

use crate::jup::get_jupiter_quote_with_retry;
use crate::oracle;
use crate::pipeline::{Config, LiquidationContext, LiquidationQuote};

/// Get SOL price in USD from oracle (standalone version - no context required)
/// Returns SOL price if available, otherwise None
pub async fn get_sol_price_usd_standalone(rpc: &Arc<RpcClient>) -> Option<f64> {
    // Method 1: Fetch from Pyth SOL/USD price feed (primary)
    let sol_usd_pyth_feed_primary = {
        let feed_str = env::var("SOL_USD_PYTH_FEED")
            .unwrap_or_else(|_| "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG".to_string());
        
        match Pubkey::from_str(&feed_str) {
            Ok(pk) => pk,
            Err(_) => {
                log::error!("Failed to parse primary Pyth SOL/USD feed address from SOL_USD_PYTH_FEED: {}", feed_str);
                match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
                    Ok(pk) => pk,
                    Err(_) => return None,
                }
            }
        }
    };
    
    let current_slot = rpc.get_slot().ok();
    
    // Try primary Pyth feed
    if let Some(slot) = current_slot {
        match oracle::pyth::validate_pyth_oracle(rpc, sol_usd_pyth_feed_primary, slot).await {
            Ok((true, Some(price))) => {
                log::debug!("✅ SOL price from primary Pyth feed: ${:.2}", price);
                return Some(price);
            }
            _ => {}
        }
    }
    
    // Method 2: Try backup Pyth feed if available
    if let Ok(backup_feed_str) = env::var("SOL_USD_BACKUP_FEED") {
        if let Ok(backup_feed) = Pubkey::from_str(&backup_feed_str) {
            if let Some(slot) = current_slot {
                match oracle::pyth::validate_pyth_oracle(rpc, backup_feed, slot).await {
                    Ok((true, Some(price))) => {
                        log::info!("✅ SOL price from backup Pyth feed: ${:.2}", price);
                        return Some(price);
                    }
                    _ => {}
                }
            }
        }
    }
    
    // Method 3: Try Switchboard feed
    if let Ok(switchboard_feed_str) = env::var("SOL_USD_SWITCHBOARD_FEED") {
        if let Ok(switchboard_feed) = Pubkey::from_str(&switchboard_feed_str) {
            if let Some(slot) = current_slot {
                match oracle::switchboard::validate_switchboard_oracle_by_pubkey(rpc, switchboard_feed, slot).await {
                    Ok(Some(price)) => {
                        log::info!("✅ SOL price from Switchboard feed: ${:.2}", price);
                        return Some(price);
                    }
                    _ => {}
                }
            }
        }
    }
    
    // Method 4: Try Pyth Hermes Price Service API
    const PYTH_HERMES_API: &str = "https://hermes.pyth.network/api/latest_price_feeds?ids[]=0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
    
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build();
        
    if let Ok(client) = &client {
        if let Ok(resp) = client.get(PYTH_HERMES_API).send().await {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(feed) = json.as_array().and_then(|arr| arr.get(0)) {
                        if let Some(price_obj) = feed.get("price") {
                            let price_str = price_obj.get("price").and_then(|p| p.as_str());
                            let expo = price_obj.get("expo").and_then(|e| e.as_i64());
                            
                            if let (Some(p_str), Some(e)) = (price_str, expo) {
                                if let Ok(p_val) = p_str.parse::<i64>() {
                                    let price = (p_val as f64) * 10_f64.powi(e as i32);
                                    log::info!("✅ SOL price from Pyth Hermes API: ${:.2}", price);
                                    return Some(price);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Method 5: Try reliable price APIs
    let client = client.unwrap_or_else(|_| reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default());
    
    // CoinGecko
    const COINGECKO_API: &str = "https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd";
    if let Ok(resp) = client.get(COINGECKO_API).send().await {
        if resp.status().is_success() {
            #[derive(serde::Deserialize)]
            struct CoinGeckoResponse {
                solana: Option<CoinGeckoPrice>,
            }
            #[derive(serde::Deserialize)]
            struct CoinGeckoPrice {
                usd: Option<f64>,
            }
            if let Ok(json) = resp.json::<CoinGeckoResponse>().await {
                if let Some(price) = json.solana.and_then(|p| p.usd) {
                    log::info!("✅ SOL price from CoinGecko API: ${:.2}", price);
                    return Some(price);
                }
            }
        }
    }
    
    // Binance
    const BINANCE_API: &str = "https://api.binance.com/api/v3/ticker/price?symbol=SOLUSDT";
    if let Ok(resp) = client.get(BINANCE_API).send().await {
        if resp.status().is_success() {
            #[derive(serde::Deserialize)]
            struct BinanceResponse {
                price: String,
            }
            if let Ok(json) = resp.json::<BinanceResponse>().await {
                if let Ok(price) = json.price.parse::<f64>() {
                    log::info!("✅ SOL price from Binance API: ${:.2}", price);
                    return Some(price);
                }
            }
        }
    }
    
    // Jupiter Price API
    const JUPITER_PRICE_API: &str = "https://price.jup.ag/v4/price?ids=SOL";
    if let Ok(resp) = client.get(JUPITER_PRICE_API).send().await {
        if resp.status().is_success() {
            #[derive(serde::Deserialize)]
            struct PriceData {
                price: f64,
            }
            #[derive(serde::Deserialize)]
            struct JupiterPriceResponse {
                data: std::collections::HashMap<String, PriceData>,
            }
            if let Ok(json) = resp.json::<JupiterPriceResponse>().await {
                if let Some(data) = json.data.get("SOL") {
                    log::info!("✅ SOL price from Jupiter API: ${:.2}", data.price);
                    return Some(data.price);
                }
            }
        }
    }

    log::error!(
        "❌ CRITICAL: All SOL price oracle methods failed! \
         SOL price is REQUIRED for: profit calculations, fee calculations, risk limit checks."
    );
    
    None
}

/// Get SOL price in USD from oracle (with context - checks if SOL is in liquidation)
pub async fn get_sol_price_usd(rpc: &Arc<RpcClient>, ctx: &LiquidationContext) -> Option<f64> {
    let sol_native_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").ok()?;
    
    // Method 1: Check if collateral or debt is SOL, use its price
    if let Some(deposit_reserve) = &ctx.deposit_reserve {
        if deposit_reserve.liquidity().mintPubkey == sol_native_mint {
            if let Some(price) = ctx.deposit_price_usd {
                log::debug!("Using SOL price from deposit reserve: ${}", price);
                return Some(price);
            }
        }
    }
    
    if let Some(borrow_reserve) = &ctx.borrow_reserve {
        if borrow_reserve.liquidity().mintPubkey == sol_native_mint {
            if let Some(price) = ctx.borrow_price_usd {
                log::debug!("Using SOL price from borrow reserve: ${}", price);
                return Some(price);
            }
        }
    }
    
    // Method 2: Use standalone function
    get_sol_price_usd_standalone(rpc).await
}

/// Get Jupiter quote for liquidation with profit calculation
pub async fn get_liquidation_quote(
    ctx: &LiquidationContext,
    config: &Config,
    rpc: &Arc<RpcClient>,
) -> Result<LiquidationQuote> {
    // Use first borrow and first deposit
    if ctx.borrows.is_empty() || ctx.deposits.is_empty() {
        return Err(anyhow::anyhow!("No borrows or deposits in obligation"));
    }

    let borrow = &ctx.borrows[0];

    // Get token mints
    let collateral_ctoken_mint = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.collateral().mintPubkey)
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let collateral_underlying_mint = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.liquidity().mintPubkey)
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let debt_mint = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.liquidity().mintPubkey)
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    
    log::debug!(
        "Jupiter quote request:\n\
         Collateral cToken: {}\n\
         Collateral underlying: {} (will be used for Jupiter)\n\
         Debt token: {}",
        collateral_ctoken_mint,
        collateral_underlying_mint,
        debt_mint
    );

    // Get reserves and token decimals
    let borrow_reserve = ctx
        .borrow_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = ctx
        .deposit_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let debt_decimals = borrow_reserve.liquidity().mintDecimals;
    let collateral_decimals = deposit_reserve.liquidity().mintDecimals;
    
    // Validate decimal values
    if debt_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid debt decimals: {} (expected 0-18). Reserve: {}.",
            debt_decimals,
            ctx.borrows[0].borrowReserve
        ));
    }
    
    if collateral_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid collateral decimals: {} (expected 0-18). Reserve: {}.",
            collateral_decimals,
            ctx.deposits[0].depositReserve
        ));
    }
    
    // Calculate debt to repay (close factor from reserve config)
    const WAD: u128 = 1_000_000_000_000_000_000; // 10^18
    
    let close_factor_f64 = borrow_reserve.close_factor();
    let close_factor_wad = (close_factor_f64 * WAD as f64) as u128;
    
    log::debug!(
        "Close factor: {:.1}% (from reserve config, fallback to 50% if not available)",
        close_factor_f64 * 100.0
    );
    
    // Calculate actual debt in normalized format (interest included)
    let actual_debt_wad: u128 = borrow.borrowedAmountWads
        .checked_mul(borrow.cumulativeBorrowRateWads)
        .and_then(|v| v.checked_div(WAD))
        .and_then(|v| v.checked_div(WAD))
        .ok_or_else(|| anyhow::anyhow!("Debt calculation overflow: borrowedAmountWad * cumulativeBorrowRateWads"))?;
    
    // Apply close factor
    let debt_to_repay_wad: u128 = actual_debt_wad
        .checked_mul(close_factor_wad)
        .and_then(|v| v.checked_div(WAD))
        .ok_or_else(|| anyhow::anyhow!("Close factor calculation overflow"))?;
    
    // Convert normalized amount to raw token amount with decimals
    let decimals_multiplier = 10_u128
        .checked_pow(debt_decimals as u32)
        .ok_or_else(|| anyhow::anyhow!("Decimals multiplier overflow: 10^{}", debt_decimals))?;
    
    let debt_to_repay_raw = debt_to_repay_wad
        .checked_mul(decimals_multiplier)
        .ok_or_else(|| anyhow::anyhow!("Raw amount conversion overflow"))?
        as u64;
    
    log::debug!(
        "Debt calculation: borrowed_wad={}, cumulative_rate={}, actual_debt_wad={}, debt_to_repay_wad={}, debt_to_repay_raw={} (decimals={})",
        borrow.borrowedAmountWads,
        borrow.cumulativeBorrowRateWads,
        actual_debt_wad,
        debt_to_repay_wad,
        debt_to_repay_raw,
        debt_decimals
    );
    
    // Get liquidation bonus from deposit reserve
    let liquidation_bonus = deposit_reserve.liquidation_bonus();
    
    // Convert debt raw amount to USD for calculations
    let debt_price_usd = ctx
        .borrow_price_usd
        .ok_or_else(|| anyhow::anyhow!("Borrow price not available"))?;
    let collateral_price_usd = ctx
        .deposit_price_usd
        .ok_or_else(|| anyhow::anyhow!("Deposit price not available"))?;
    
    let debt_to_repay_normalized = debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32);
    let debt_to_repay_usd = debt_to_repay_normalized * debt_price_usd;
    
    // Calculate collateral to seize
    let debt_in_collateral_tokens = debt_to_repay_usd / collateral_price_usd;
    let collateral_to_seize_tokens = debt_in_collateral_tokens * (1.0 + liquidation_bonus);
    let collateral_to_seize_usd = collateral_to_seize_tokens * collateral_price_usd;
    let collateral_to_seize_raw = (collateral_to_seize_tokens * 10_f64.powi(collateral_decimals as i32)) as u64;
    
    log::debug!(
        "Collateral calculation: debt_to_repay=${:.2} USD -> {:.6} collateral tokens -> {:.6} with bonus ({:.1}%) -> {:.6} USD -> {} raw",
        debt_to_repay_usd,
        debt_in_collateral_tokens,
        collateral_to_seize_tokens,
        liquidation_bonus * 100.0,
        collateral_to_seize_usd,
        collateral_to_seize_raw
    );
    
    // Calculate actual SOL amount after redemption (cToken exchange rate)
    let ctokens_total_supply = deposit_reserve.collateral().mintTotalSupply;
    let available_amount = deposit_reserve.liquidity().availableAmount;
    let borrowed_amount_wads = deposit_reserve.liquidity().borrowedAmountWads;
    let cumulative_borrow_rate = deposit_reserve.liquidity().cumulativeBorrowRateWads;
    
    let actual_borrowed_normalized = borrowed_amount_wads
        .checked_mul(cumulative_borrow_rate)
        .and_then(|v| v.checked_div(WAD))
        .and_then(|v| v.checked_div(WAD))
        .ok_or_else(|| anyhow::anyhow!("Borrowed amount calculation overflow"))?;
    
    let decimals_multiplier = 10_u128
        .checked_pow(collateral_decimals as u32)
        .ok_or_else(|| anyhow::anyhow!("Decimals multiplier overflow"))?;
    
    let actual_borrowed_u128 = actual_borrowed_normalized
        .checked_mul(decimals_multiplier)
        .ok_or_else(|| anyhow::anyhow!("Overflow in actual borrowed calculation"))?;
    
    if actual_borrowed_u128 > u64::MAX as u128 {
        return Err(anyhow::anyhow!(
            "Borrowed amount exceeds u64::MAX: {} (max: {}).",
            actual_borrowed_u128,
            u64::MAX
        ));
    }
    
    let actual_borrowed_raw = actual_borrowed_u128 as u64;
    let total_underlying_supply = available_amount.saturating_add(actual_borrowed_raw);
    
    let exchange_rate = if ctokens_total_supply > 0 {
        total_underlying_supply as f64 / ctokens_total_supply as f64
    } else {
        1.0
    };
    
    let sol_amount_after_redemption = (collateral_to_seize_raw as f64 * exchange_rate) as u64;
    
    log::debug!(
        "cToken exchange: cTokens={}, exchange_rate={:.6}, SOL_output={}",
        collateral_to_seize_raw,
        exchange_rate,
        sol_amount_after_redemption
    );
    
    // Validate exchange rate
    if exchange_rate < 0.9 || exchange_rate > 1.5 {
        return Err(anyhow::anyhow!(
            "Suspicious exchange rate: {:.6} (expected 0.9-1.5)", 
            exchange_rate
        ));
    }
    
    // Get Jupiter quote with calculated dynamic slippage
    let base_slippage_bps = env::var("MIN_PROFIT_MARGIN_BPS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(50);
    
    let max_slippage_bps = env::var("MAX_SLIPPAGE_BPS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(300);
        
    let buffer_multiplier = 2.0;
    let initial_slippage = ((base_slippage_bps as f64 * buffer_multiplier) as u16).min(max_slippage_bps);
    
    log::debug!(
        "Jupiter quote strategy: Single call with {}bps slippage (base: {}bps, buffer: {:.1}x)",
        initial_slippage,
        base_slippage_bps,
        buffer_multiplier
    );
    
    let max_retries = env::var("JUPITER_MAX_RETRIES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    
    let quote = get_jupiter_quote_with_retry(
        &collateral_underlying_mint,
        &debt_mint,
        sol_amount_after_redemption,
        initial_slippage,
        max_retries,
    )
    .await
    .context("Failed to get Jupiter quote with retries")?;
    
    // Price impact check
    let price_impact_pct = crate::jup::get_price_impact_pct(&quote);
    if price_impact_pct > 5.0 {
        log::warn!(
            "❌ Price impact too high: {:.2}% (max: 5.0%). Skipping liquidation due to low liquidity.",
            price_impact_pct
        );
        return Err(anyhow::anyhow!(
            "Price impact too high: {:.2}% (max: 5.0%)",
            price_impact_pct
        ));
    }
    
    log::debug!(
        "Jupiter quote successful: impact={:.2}%, out_amount={}",
        price_impact_pct,
        quote.out_amount
    );

    // Profit calculation
    let jupiter_out_amount: u64 = quote.out_amount
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid Jupiter out_amount: {}", e))?;
    
    let profit_raw = (jupiter_out_amount as i128) - (debt_to_repay_raw as i128);
    let profit_tokens = (profit_raw as f64) / 10_f64.powi(debt_decimals as i32);
    let profit_before_fees_usd = profit_tokens * debt_price_usd;
    
    // Calculate total fees for flashloan transaction
    let sol_price_usd = match get_sol_price_usd(rpc, ctx).await {
        Some(price) => price,
        None => {
            log::error!(
                "❌ CRITICAL: Cannot calculate liquidation profit without SOL price!"
            );
            return Err(anyhow::anyhow!(
                "Cannot proceed with liquidation: SOL price unavailable from oracle."
            ));
        }
    };
    
    let jito_tip_lamports = config.jito_tip_amount_lamports.unwrap_or(10_000_000u64);
    let jito_tip_sol = jito_tip_lamports as f64 / 1_000_000_000.0;
    
    let tx_base_fee_lamports = env::var("BASE_TRANSACTION_FEE_LAMPORTS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5_000);
    
    let tx_compute_units = env::var("LIQUIDATION_COMPUTE_UNITS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            env::var("DEFAULT_COMPUTE_UNITS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(500_000);
    
    let tx_priority_fee_per_cu = env::var("DEFAULT_PRIORITY_FEE_PER_CU")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            env::var("PRIORITY_FEE_PER_CU")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(1_000);
    
    let tx_priority_fee_lamports = (tx_compute_units * tx_priority_fee_per_cu) / 1_000_000;
    let tx_total_fee_lamports = tx_base_fee_lamports + tx_priority_fee_lamports;
    
    // Flashloan fee calculation
    let flashloan_fee_amount_raw = if let Some(borrow_reserve) = &ctx.borrow_reserve {
        let flashloan_fee_wad = borrow_reserve.config().flashLoanFeeWad as u128;
        (debt_to_repay_raw as u128)
            .checked_mul(flashloan_fee_wad)
            .and_then(|v| v.checked_div(WAD))
            .ok_or_else(|| anyhow::anyhow!("Flashloan fee calculation overflow"))?
            as u64
    } else {
        0
    };
    
    let flashloan_fee_usd = if flashloan_fee_amount_raw > 0 {
        let flashloan_fee_tokens = (flashloan_fee_amount_raw as f64) / 10_f64.powi(debt_decimals as i32);
        flashloan_fee_tokens * debt_price_usd
    } else {
        0.0
    };
    
    // Total fees in USD
    let jito_fee_usd = jito_tip_sol * sol_price_usd;
    let tx_fee_usd = (tx_total_fee_lamports as f64 / 1_000_000_000.0) * sol_price_usd;
    let total_fees_usd = jito_fee_usd + tx_fee_usd + flashloan_fee_usd;
    
    // Final profit
    let profit_usdc = profit_before_fees_usd - total_fees_usd;
    
    log::debug!(
        "Profit calculation (FLASHLOAN - SINGLE TX):\n\
         Jupiter output: {} raw ({:.6} tokens, ${:.2} USD)\n\
         Debt to repay: {} raw ({:.6} tokens, ${:.2} USD)\n\
         Profit before fees: ${:.2}\n\
         Total fees: ${:.4}\n\
         FINAL PROFIT: ${:.2}",
        jupiter_out_amount,
        jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32),
        jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32) * debt_price_usd,
        debt_to_repay_raw,
        debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32),
        debt_to_repay_usd,
        profit_before_fees_usd,
        total_fees_usd,
        profit_usdc
    );
    
    // Negative profit check
    if profit_usdc < 0.0 {
        let jupiter_out_amount_usd = (jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32)) * debt_price_usd;
        let swap_loss_usd = collateral_to_seize_usd - jupiter_out_amount_usd;
        log::warn!(
            "Negative profit detected: ${:.2}. Jupiter swap loss (${:.2}) exceeded liquidation bonus!",
            profit_usdc,
            swap_loss_usd
        );
        return Err(anyhow::anyhow!("Liquidation would lose money: profit=${:.2}", profit_usdc));
    }
    
    // Slippage buffer check
    const SLIPPAGE_BUFFER_PCT: f64 = 0.5;
    let effective_min_profit = config.min_profit_usdc * (1.0 + SLIPPAGE_BUFFER_PCT / 100.0);
    
    if profit_usdc < effective_min_profit {
        return Err(anyhow::anyhow!(
            "Profit ${:.2} below threshold ${:.2} (with {:.1}% slippage buffer)",
            profit_usdc,
            effective_min_profit,
            SLIPPAGE_BUFFER_PCT
        ));
    }
    
    let collateral_value_usd = collateral_to_seize_usd;
    
    Ok(LiquidationQuote {
        quote,
        profit_usdc,
        collateral_value_usd,
        debt_to_repay_raw,
        collateral_to_seize_raw,
        flashloan_fee_raw: flashloan_fee_amount_raw,
    })
}

