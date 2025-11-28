//! Jupiter Swap API Integration
//!
//! Provides real-time slippage estimation using Jupiter's quote API.
//! This replaces or supplements the estimated slippage model with actual market data.
//!
//! Reference: https://quote-api.jup.ag/v6/quote

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;

const JUPITER_QUOTE_API_URL: &str = "https://quote-api.jup.ag/v6/quote";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JupiterQuoteRequest {
    #[serde(rename = "inputMint")]
    pub input_mint: String,
    #[serde(rename = "outputMint")]
    pub output_mint: String,
    #[serde(rename = "amount")]
    pub amount: u64,
    #[serde(rename = "slippageBps")]
    pub slippage_bps: Option<u16>,
    #[serde(rename = "onlyDirectRoutes")]
    pub only_direct_routes: Option<bool>,
    #[serde(rename = "asLegacyTransaction")]
    pub as_legacy_transaction: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JupiterQuoteResponse {
    #[serde(rename = "inputMint")]
    pub input_mint: String,
    #[serde(rename = "inAmount")]
    pub in_amount: String,
    #[serde(rename = "outputMint")]
    pub output_mint: String,
    #[serde(rename = "outAmount")]
    pub out_amount: String,
    #[serde(rename = "otherAmountThreshold")]
    pub other_amount_threshold: String,
    #[serde(rename = "swapMode")]
    pub swap_mode: String,
    #[serde(rename = "priceImpactPct")]
    pub price_impact_pct: Option<String>,
    #[serde(rename = "routePlan")]
    pub route_plan: Vec<serde_json::Value>,
}

/// Get real-time slippage estimate from Jupiter API
///
/// Returns the price impact percentage (slippage) for a given swap.
/// This is more accurate than estimated slippage based on trade size.
///
/// # Arguments
/// * `input_mint` - Input token mint address
/// * `output_mint` - Output token mint address
/// * `amount` - Amount to swap (in token's smallest unit, e.g., lamports for SOL)
/// * `slippage_bps` - Maximum acceptable slippage in basis points (optional, for API request)
///
/// # Returns
/// * `Ok(Some(price_impact_bps))` - Price impact in basis points (100 bps = 1%)
/// * `Ok(None)` - No price impact data available (should fall back to estimated slippage)
/// * `Err` - API request failed (should fall back to estimated slippage)
pub async fn get_jupiter_slippage_estimate(
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: Option<u16>,
) -> Result<Option<u16>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("Failed to create HTTP client")?;

    // Build query string
    let mut query_params = vec![
        ("inputMint", input_mint.to_string()),
        ("outputMint", output_mint.to_string()),
        ("amount", amount.to_string()),
    ];

    if let Some(slippage) = slippage_bps {
        query_params.push(("slippageBps", slippage.to_string()));
    }

    let url = format!("{}?{}", JUPITER_QUOTE_API_URL, 
        query_params.iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&")
    );

    log::debug!(
        "🔍 Jupiter API: Requesting quote for {} -> {} (amount: {})",
        input_mint,
        output_mint,
        amount
    );

    let response = client
        .get(&url)
        .send()
        .await
        .context("Failed to send Jupiter API request")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        log::warn!(
            "⚠️  Jupiter API returned error: {} - {}",
            status,
            error_text
        );
        return Ok(None); // Fall back to estimated slippage
    }

    let quote: JupiterQuoteResponse = response
        .json()
        .await
        .context("Failed to parse Jupiter API response")?;

    // Extract price impact percentage
    if let Some(price_impact_str) = quote.price_impact_pct {
        let price_impact_pct: f64 = price_impact_str
            .parse()
            .context("Failed to parse price impact percentage")?;
        
        // Convert percentage to basis points (1% = 100 bps)
        let price_impact_bps = (price_impact_pct * 100.0) as u16;
        
        log::debug!(
            "✅ Jupiter API: Price impact = {:.2}% ({} bps) for {} -> {}",
            price_impact_pct,
            price_impact_bps,
            input_mint,
            output_mint
        );
        
        Ok(Some(price_impact_bps))
    } else {
        log::debug!(
            "⚠️  Jupiter API: No price impact data available for {} -> {}",
            input_mint,
            output_mint
        );
        Ok(None)
    }
}

/// Calculate estimated slippage using Jupiter API or fallback to model
///
/// This function tries to get real-time slippage from Jupiter API first.
/// If Jupiter API is unavailable or fails, it falls back to the estimated slippage model.
///
/// # Arguments
/// * `input_mint` - Input token mint address
/// * `output_mint` - Output token mint address
/// * `amount` - Amount to swap (in token's smallest unit)
/// * `estimated_slippage_bps` - Fallback estimated slippage in basis points
/// * `use_jupiter_api` - Whether to use Jupiter API (from config)
///
/// # Returns
/// * Slippage in basis points (either from Jupiter API or estimated model)
pub async fn get_slippage_estimate(
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    estimated_slippage_bps: u16,
    use_jupiter_api: bool,
) -> u16 {
    if !use_jupiter_api {
        log::debug!(
            "📊 Using estimated slippage model: {} bps (Jupiter API disabled)",
            estimated_slippage_bps
        );
        return estimated_slippage_bps;
    }

    // Try Jupiter API first
    match get_jupiter_slippage_estimate(input_mint, output_mint, amount, Some(estimated_slippage_bps)).await {
        Ok(Some(jupiter_slippage_bps)) => {
            log::debug!(
                "✅ Using Jupiter API slippage: {} bps (estimated: {} bps)",
                jupiter_slippage_bps,
                estimated_slippage_bps
            );
            // Use the higher of the two as a safety margin
            jupiter_slippage_bps.max(estimated_slippage_bps)
        }
        Ok(None) => {
            log::warn!(
                "⚠️  Jupiter API returned no data, using estimated slippage: {} bps",
                estimated_slippage_bps
            );
            estimated_slippage_bps
        }
        Err(e) => {
            log::warn!(
                "⚠️  Jupiter API failed: {}, using estimated slippage: {} bps",
                e,
                estimated_slippage_bps
            );
            estimated_slippage_bps
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::pubkey::Pubkey;

    #[tokio::test]
    #[ignore] // Requires network access
    async fn test_jupiter_api_quote() {
        // Test with SOL -> USDC swap
        let sol_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
        let usdc_mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let amount = 1_000_000_000; // 1 SOL

        let result = get_jupiter_slippage_estimate(&sol_mint, &usdc_mint, amount, Some(100)).await;
        match result {
            Ok(Some(slippage_bps)) => {
                println!("✅ Jupiter API returned slippage: {} bps", slippage_bps);
                assert!(slippage_bps < 1000, "Slippage should be reasonable (<10%)");
            }
            Ok(None) => {
                println!("⚠️  Jupiter API returned no data");
            }
            Err(e) => {
                println!("❌ Jupiter API error: {}", e);
            }
        }
    }
}

