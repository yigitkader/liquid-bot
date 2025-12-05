use anyhow::Result;
use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;

const JUPITER_QUOTE_API: &str = "https://quote-api.jup.ag/v6/quote";

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

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Jupiter API request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Jupiter API returned error: {}",
            response.status()
        ));
    }

    let quote: JupiterQuote = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse Jupiter quote: {}", e))?;

    Ok(quote)
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

