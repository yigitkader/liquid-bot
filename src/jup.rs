use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use solana_sdk::{
    pubkey::Pubkey,
    instruction::Instruction,
};
use std::time::Duration;

const JUPITER_QUOTE_API: &str = "https://quote-api.jup.ag/v6/quote";

// CRITICAL: Increased timeout from 10 to 15 seconds to handle busy periods
// when Jupiter API can take 10+ seconds to respond
const REQUEST_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoutePlan {
    #[serde(rename = "swapInfo")]
    pub swap_info: Option<SwapInfo>,
    pub percent: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    // CRITICAL FIX: Use only reqwest timeout to avoid race conditions with tokio timeout
    // Reqwest timeout handles both connection and read timeouts, including JSON parsing
    // This is cleaner and avoids conflicts between two timeout layers
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .context("Failed to create HTTP client")?;

    // Send request - reqwest timeout will handle both connection and response timeouts
    let response = client
        .get(&url)
        .send()
        .await
        .context(format!(
            "Jupiter API request failed or timed out after {} seconds",
            REQUEST_TIMEOUT_SECS
        ))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Jupiter API returned error: {}",
            response.status()
        ));
    }

    // Parse JSON - reqwest timeout also covers JSON parsing time
    let quote: JupiterQuote = response
        .json()
        .await
        .context("Failed to parse Jupiter quote JSON response")?;

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
                    // ✅ FIXED: Exponential backoff with jitter
                    // Base delay: 500ms * attempt (500ms, 1000ms, 1500ms...)
                    // Jitter: random 0-200ms to prevent thundering herd
                    use rand::Rng;
                    let base_delay_ms = 500 * attempt as u64;
                    let jitter_ms = rand::thread_rng().gen_range(0..200);
                    let delay_ms = base_delay_ms + jitter_ms;
                    log::warn!(
                        "Jupiter quote attempt {}/{} failed, retrying in {}ms (base: {}ms + jitter: {}ms)...",
                        attempt,
                        max_retries,
                        delay_ms,
                        base_delay_ms,
                        jitter_ms
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

/// Build Jupiter swap instruction from quote
/// Returns instruction that swaps input_mint -> output_mint
/// 
/// CRITICAL: This function calls Jupiter's swap-instructions endpoint to get
/// the actual swap instruction that can be included in a transaction.
pub async fn build_jupiter_swap_instruction(
    quote: &JupiterQuote,
    user_pubkey: &Pubkey,
    jupiter_url: &str,
) -> Result<Instruction> {
    let swap_url = format!("{}/v6/swap-instructions", jupiter_url);
    
    // Jupiter v6 swap-instructions API expects:
    // - quoteResponse: The full quote object
    // - userPublicKey: User's wallet public key
    // - wrapAndUnwrapSol: Whether to wrap/unwrap SOL automatically
    // - dynamicComputeUnitLimit: Whether to use dynamic compute unit limits
    let payload = serde_json::json!({
        "quoteResponse": quote,
        "userPublicKey": user_pubkey.to_string(),
        "wrapAndUnwrapSol": true,
        "dynamicComputeUnitLimit": true,
    });
    
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .context("Failed to create HTTP client")?;
    
    let response = client
        .post(&swap_url)
        .json(&payload)
        .send()
        .await
        .context("Jupiter swap instruction request failed")?;
    
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Jupiter swap instruction failed: {} - {}",
            status,
            error_text
        ));
    }
    
    // Jupiter v6 API returns instructions in different formats
    // Try to parse as the standard format first
    #[derive(Deserialize)]
    struct SwapInstructionsResponse {
        #[serde(rename = "swapInstruction")]
        swap_instruction: Option<String>, // Base64 encoded
        #[serde(rename = "setupInstructions")]
        setup_instructions: Option<Vec<String>>,
        #[serde(rename = "cleanupInstruction")]
        cleanup_instruction: Option<String>,
    }
    
    let swap_response: SwapInstructionsResponse = response
        .json()
        .await
        .context("Failed to parse Jupiter swap instruction response")?;
    
    // Get the main swap instruction (base64 encoded)
    let swap_instruction_b64 = swap_response
        .swap_instruction
        .ok_or_else(|| anyhow::anyhow!("Jupiter response missing swapInstruction field"))?;
    
    // Decode base64 instruction
    use base64::Engine;
    let instruction_bytes = base64::engine::general_purpose::STANDARD
        .decode(&swap_instruction_b64)
        .context("Failed to decode swap instruction from base64")?;
    
    // Deserialize instruction
    // CRITICAL: Jupiter returns instructions in a specific format
    // The instruction bytes might be in a wrapper format, so we need to handle it properly
    let instruction: Instruction = bincode::deserialize(&instruction_bytes)
        .context("Failed to deserialize swap instruction from bytes")?;
    
    log::debug!(
        "✅ Jupiter swap instruction built: program_id={}, accounts={}, data_len={}",
        instruction.program_id,
        instruction.accounts.len(),
        instruction.data.len()
    );
    
    Ok(instruction)
}

