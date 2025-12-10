// Liquidation quote and profit calculation module
// Moved from pipeline.rs to reduce code size and improve maintainability

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::env;
use std::str::FromStr;
use std::sync::Arc;

use crate::jup::get_jupiter_quote_with_retry;
use crate::pipeline::{Config, LiquidationContext, LiquidationQuote};

const WAD: u128 = 1_000_000_000_000_000_000;

#[derive(serde::Deserialize)]
struct CoinGeckoPriceResponse {
    solana: CoinGeckoSolanaPrice,
}

#[derive(serde::Deserialize)]
struct CoinGeckoSolanaPrice {
    usd: f64,
}

/// Get SOL price from CoinGecko API
/// Used as a fallback when on-chain oracles are unavailable
async fn get_solana_price_coingecko(client: &reqwest::Client) -> Result<f64> {
    let url = "https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd";
    
    // User suggestion implementation integrated with existing client
    let response = client.get(url)
        .send()
        .await
        .context("Failed to send CoinGecko request")?;
        
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("CoinGecko API returned error status: {}", response.status()));
    }

    let json: CoinGeckoPriceResponse = response.json()
        .await
        .context("Failed to parse CoinGecko response")?;
        
    Ok(json.solana.usd)
}

/// Get SOL price in USD from HTTP APIs (standalone version - no context required)
/// Returns SOL price if available, otherwise None
/// 
/// CRITICAL: This function tries multiple HTTP sources in order:
/// 1. CoinGecko API (primary, reliable)
/// 2. Pyth Hermes API (official Pyth HTTP endpoint)
/// 3. Binance API (high volume exchange)
/// 4. Jupiter Price API (Solana native)
/// 
/// NOTE: On-chain Pyth feeds are NOT used because they can return stale data.
/// The feed H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG was returning $42.95
/// instead of the actual ~$138 price as of Dec 2025.
pub async fn get_sol_price_usd_standalone(_rpc: &Arc<RpcClient>) -> Option<f64> {
    log::debug!("🔍 Starting SOL price discovery from multiple oracle sources...");
    
    // 🔴 CRITICAL FIX: Default to HTTP price sources (CoinGecko, Binance, Jupiter)
    // The on-chain Pyth feed H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG was returning
    // stale/incorrect prices ($42.95 instead of ~$138 as of Dec 2025).
    // HTTP sources are more reliable and provide real-time prices.
    // Set PREFER_HTTP_PRICE=false to use on-chain oracles instead.
    let prefer_http_price = env::var("PREFER_HTTP_PRICE")
        .map(|v| v.to_lowercase() != "false") // Default true unless explicitly "false"
        .unwrap_or(true); // ✅ DEFAULT: true (use HTTP sources)
        
    // HTTP client with 5s timeout (CoinGecko can be slow)
    // 2s was too short and caused frequent failures
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();
        
    // ==========================================================================
    // SOL PRICE DISCOVERY - HTTP APIs ONLY (no stale on-chain feeds)
    // ==========================================================================
    // Priority order:
    // 1. CoinGecko API (reliable, free)
    // 2. Pyth Hermes API (official Pyth HTTP endpoint)
    // 3. Binance API (high volume exchange)
    // 4. Jupiter API (Solana native)
    // 
    // NOTE: On-chain Pyth feeds REMOVED from fallback chain because:
    // - H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG returns stale $42.95
    // - Pyth Core "Pull" model requires active price updates
    // - HTTP APIs are more reliable for SOL/USD price
    // ==========================================================================
    
    // Method 1: CoinGecko API (primary)
    if prefer_http_price {
        log::info!("🌐 Fetching SOL price from CoinGecko API...");
        if let Ok(price) = get_solana_price_coingecko(&client).await {
            log::info!("✅ SOL price from CoinGecko API: ${:.2}", price);
            return Some(price);
        }
        log::debug!("⚠️  CoinGecko failed, trying Pyth Hermes...");
    }
    
    // Method 2: Pyth Hermes API via simple_pyth_client_rs (dynamic feed discovery)
    // This uses the hermes module which dynamically discovers feed IDs at runtime
    // No more hardcoded feed addresses - 100% accurate
    match crate::oracle::hermes::get_price_from_hermes("SOL").await {
        Ok(Some(price)) => {
            log::info!("✅ SOL price from Pyth Hermes SDK: ${:.2}", price);
                return Some(price);
        }
        Ok(None) => {
            log::debug!("⚠️  Pyth Hermes SDK returned no price, trying Binance...");
        }
        Err(e) => {
            log::debug!("⚠️  Pyth Hermes SDK failed: {}, trying Binance...", e);
        }
    }
    
    // Method 3: Binance API
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
         SOL price is REQUIRED for: profit calculations, fee calculations, risk limit checks.\n\
         \n\
         Troubleshooting:\n\
         1. Check network connectivity (CoinGecko, Pyth Hermes, Binance, Jupiter APIs)\n\
         2. Check RPC connection (get_slot() should work)\n\
         3. Check for rate limiting on HTTP APIs\n\
         4. Verify Hermes API is accessible: https://hermes.pyth.network"
    );
    
    None
}

/// Get SOL price in USD from oracle (with context - checks if SOL is in liquidation)
pub async fn get_sol_price_usd(rpc: &Arc<RpcClient>, ctx: &LiquidationContext) -> Option<f64> {
    let sol_native_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").ok()?;
    
    // Method 1: Check if collateral or debt is SOL, use its price
    if let Some(deposit_reserve) = &ctx.deposit_reserve {
        if deposit_reserve.collateral_mint_pubkey() == sol_native_mint {
            if let Some(price) = ctx.deposit_price_usd {
                log::debug!("Using SOL price from deposit reserve: ${}", price);
                return Some(price);
            }
        }
    }
    
    if let Some(borrow_reserve) = &ctx.borrow_reserve {
        if borrow_reserve.mint_pubkey() == sol_native_mint {
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
/// Kamino Lend protocol only
pub async fn get_liquidation_quote(
    ctx: &LiquidationContext,
    config: &Config,
    rpc: &Arc<RpcClient>,
) -> Result<LiquidationQuote> {
    get_kamino_liquidation_quote(ctx, config, rpc).await
}

/// Get liquidation quote for Kamino Lend
async fn get_kamino_liquidation_quote(
    ctx: &LiquidationContext,
    config: &Config,
    rpc: &Arc<RpcClient>,
) -> Result<LiquidationQuote> {
    use crate::kamino::{Reserve as KaminoReserve, ObligationLiquidity as KaminoObligationLiquidity, ObligationCollateral as KaminoObligationCollateral, SCALE_FACTOR};
    
    let borrows = &ctx.borrows;
    let deposits = &ctx.deposits;
    let borrow_reserve = &ctx.borrow_reserve;
    let deposit_reserve = &ctx.deposit_reserve;
    let borrow_price_usd = &ctx.borrow_price_usd;
    let deposit_price_usd = &ctx.deposit_price_usd;
    
    // Use first borrow and first deposit
    if borrows.is_empty() || deposits.is_empty() {
        return Err(anyhow::anyhow!("No borrows or deposits in Kamino obligation"));
    }
    
    let borrow = &borrows[0];
    let deposit = &deposits[0];
    
    let borrow_reserve = borrow_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = deposit_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    // Get token mints
    let collateral_mint = deposit_reserve.collateral_mint_pubkey();
    let debt_mint = borrow_reserve.mint_pubkey();
    
    let debt_decimals = borrow_reserve.mint_decimals();
    let collateral_decimals = deposit_reserve.mint_decimals();
    
    // Get close factor (Kamino uses protocol-level close factor, default to 50%)
    let close_factor_f64 = deposit_reserve.close_factor();
    
    // Calculate actual debt (Kamino uses scale factor, not WAD)
    let actual_debt_sf = borrow.borrowed_amount_sf;
    
    // Apply close factor
    let debt_to_repay_sf = (actual_debt_sf as f64 * close_factor_f64) as u128;
    
    // Convert to raw token amount
    let decimals_multiplier = 10_u128.pow(debt_decimals as u32);
    let debt_to_repay_raw = (debt_to_repay_sf as f64 / SCALE_FACTOR as f64 * decimals_multiplier as f64) as u64;
    
    // Get liquidation bonus
    let liquidation_bonus = deposit_reserve.liquidation_bonus_bps() as f64 / 10000.0;
    
    // Calculate collateral to seize
    let debt_to_repay_usd = borrow_price_usd.unwrap_or(0.0) * (debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32));
    let collateral_to_seize_tokens = (debt_to_repay_usd / deposit_price_usd.unwrap_or(1.0)) * (1.0 + liquidation_bonus);
    let collateral_to_seize_raw = (collateral_to_seize_tokens * 10_f64.powi(collateral_decimals as i32)) as u64;
    
    // Get Jupiter quote (swap collateral -> debt token)
    let base_slippage_bps = env::var("MIN_PROFIT_MARGIN_BPS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(50);
    let max_slippage_bps = env::var("MAX_SLIPPAGE_BPS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(300);
    let initial_slippage = ((base_slippage_bps as f64 * 2.0) as u16).min(max_slippage_bps);
    let max_retries = env::var("JUPITER_MAX_RETRIES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    
    let quote = get_jupiter_quote_with_retry(
        &collateral_mint,
        &debt_mint,
        collateral_to_seize_raw,
        initial_slippage,
        max_retries,
    ).await?;
    
    // Calculate profit
    let collateral_value_usd = deposit_price_usd.unwrap_or(0.0) * (collateral_to_seize_raw as f64 / 10_f64.powi(collateral_decimals as i32));
    let debt_value_usd = borrow_price_usd.unwrap_or(0.0) * (debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32));
    let jupiter_out_amount: u64 = quote.out_amount.parse()
        .map_err(|e| anyhow::anyhow!("Invalid Jupiter out_amount: {}", e))?;
    let profit_usdc = collateral_value_usd - debt_value_usd - (jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32) * borrow_price_usd.unwrap_or(0.0));
    
    Ok(LiquidationQuote {
        quote,
        profit_usdc,
        collateral_value_usd,
        debt_to_repay_raw,
        collateral_to_seize_raw,
        flashloan_fee_raw: 0, // TODO: Calculate Kamino flashloan fee
    })
}


