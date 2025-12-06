use anyhow::{Context, Result};
use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use std::time::Duration;

const JUPITER_QUOTE_API: &str = "https://quote-api.jup.ag/v6/quote";

// CRITICAL: Increased timeout from 10 to 15 seconds to handle busy periods
// when Jupiter API can take 10+ seconds to respond
const REQUEST_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Deserialize)]
pub struct JupiterQuote {
    #[serde(rename = "inputMint")]
    pub input_mint: String,
    #[serde(rename = "outputMint")]
    pub output_mint: String,
    #[serde(rename = "inAmount")]
    pub in_amount: String,
    #[serde(rename = "outAmount")]
    pub out_amount: String,
    #[serde(rename = "priceImpactPct")]
    pub price_impact_pct: Option<String>,
    #[serde(rename = "routePlan")]
    pub route_plan: Option<Vec<RoutePlan>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutePlan {
    #[serde(rename = "swapInfo")]
    pub swap_info: Option<SwapInfo>,
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SwapInfo {
    #[serde(rename = "ammKey")]
    pub amm_key: Option<String>,
    pub label: Option<String>,
    #[serde(rename = "inputMint")]
    pub input_mint: Option<String>,
    #[serde(rename = "outputMint")]
    pub output_mint: Option<String>,
}

/// Get Jupiter quote for a swap
/// 
/// CRITICAL: Includes timeout protection to prevent hanging on slow/down Jupiter API.
/// Timeout is set to 10 seconds to handle busy periods when Jupiter API can take 5-10 seconds.
pub async fn get_jupiter_quote(
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: u16,
) -> Result<JupiterQuote> {
    let url = format!(
        "{}?inputMint={}&outputMint={}&amount={}&slippageBps={}",
        JUPITER_QUOTE_API, input_mint, output_mint, amount, slippage_bps
    );

    // Create HTTP client with timeout configuration
    // CRITICAL: Set timeout to prevent hanging on slow/down Jupiter API
    // 15 seconds allows for busy periods when Jupiter API can take 10+ seconds
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .context("Failed to create HTTP client")?;

    // Wrap the entire request in a tokio timeout for additional safety
    // This ensures we fail fast even if reqwest timeout doesn't work
    let timeout_duration = Duration::from_secs(REQUEST_TIMEOUT_SECS);
    
    let response_result = tokio::time::timeout(timeout_duration, async {
        client
            .get(&url)
            .send()
            .await
    })
    .await;

    let response = match response_result {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            return Err(anyhow::anyhow!(
                "Jupiter API request failed: {}",
                e
            ));
        }
        Err(_) => {
            return Err(anyhow::anyhow!(
                "Jupiter API request timed out after {} seconds",
                REQUEST_TIMEOUT_SECS
            ));
        }
    };

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Jupiter API returned error: {}",
            response.status()
        ));
    }

    // Parse JSON with timeout protection
    let quote_result = tokio::time::timeout(timeout_duration, response.json::<JupiterQuote>()).await;
    
    let quote: JupiterQuote = match quote_result {
        Ok(Ok(q)) => q,
        Ok(Err(e)) => {
            return Err(anyhow::anyhow!(
                "Failed to parse Jupiter quote: {}",
                e
            ));
        }
        Err(_) => {
            return Err(anyhow::anyhow!(
                "Jupiter API response parsing timed out after {} seconds",
                REQUEST_TIMEOUT_SECS
            ));
        }
    };

    Ok(quote)
}

/// Get Jupiter quote with retry mechanism
/// 
/// CRITICAL: Retries failed requests with exponential backoff to handle
/// transient Jupiter API failures and timeouts. This prevents missing
/// liquidation opportunities due to temporary API issues.
pub async fn get_jupiter_quote_with_retry(
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: u16,
    max_retries: u32,
) -> Result<JupiterQuote> {
    let mut last_error = None;
    
    for attempt in 1..=max_retries {
        match get_jupiter_quote(input_mint, output_mint, amount, slippage_bps).await {
            Ok(quote) => {
                log::debug!("Jupiter quote succeeded on attempt {}/{}", attempt, max_retries);
                return Ok(quote);
            }
            Err(e) => {
                last_error = Some(e);
                if attempt < max_retries {
                    let delay_ms = 500 * attempt as u64;
                    log::warn!(
                        "Jupiter quote attempt {}/{} failed, retrying in {}ms...",
                        attempt,
                        max_retries,
                        delay_ms
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }
    
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("All Jupiter quote attempts failed")))
}

/// Calculate profit from Jupiter quote
/// Returns profit in output token amount (as u64)
pub fn calculate_profit_from_quote(
    quote: &JupiterQuote,
    input_amount: u64,
) -> Result<i64> {
    let out_amount: u64 = quote
        .out_amount
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse out_amount: {}", e))?;

    // For liquidation: we repay debt (input) and get collateral (output)
    // Profit = output_amount - input_amount (in output token terms)
    // But we need to account for slippage and fees
    // For simplicity, return the output amount and let caller compare with input
    Ok(out_amount as i64 - input_amount as i64)
}

/// Get price impact percentage from quote
pub fn get_price_impact_pct(quote: &JupiterQuote) -> f64 {
    quote
        .price_impact_pct
        .as_ref()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// Get hop count from route plan
pub fn get_hop_count(quote: &JupiterQuote) -> u8 {
    quote
        .route_plan
        .as_ref()
        .map(|plan| plan.len() as u8)
        .unwrap_or(1)
}

