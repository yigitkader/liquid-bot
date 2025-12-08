use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use solana_sdk::{
    pubkey::Pubkey,
    instruction::Instruction,
};
use std::time::Duration;

const JUPITER_QUOTE_API: &str = "https://quote-api.jup.ag/v6/quote";

// ✅ FIXED: Balanced timeout strategy (Problems.md issue #4)
// Jupiter API can take 20-30s during high load, but we also need blockhash freshness
// 
// Strategy: 
// - Primary timeout: 10 seconds (normal conditions - balance between speed and reliability)
// - Retry timeout: 20 seconds (high load scenarios - allow more time for slow API responses)
// - Total worst case: 10s (first) + 20s (retry) = 30s, leaving 30s for TX build + send
// 
// Blockhash expires in ~60s, so we have: Jupiter (10+20=30s) + TX build (20-30s) = 50-60s total
// This leaves 0-10s buffer for blockhash freshness (acceptable trade-off)
// 
// NOTE: These values can be overridden via environment variables:
// - JUPITER_TIMEOUT_NORMAL_SECS (default: 10)
// - JUPITER_TIMEOUT_RETRY_SECS (default: 20)
// 
// Trade-off: Longer timeouts handle Jupiter API slowness better, but reduce blockhash freshness buffer.
// However, missing opportunities due to Jupiter timeout is worse than slightly stale blockhash.
const REQUEST_TIMEOUT_NORMAL_SECS: u64 = 10;   // ✅ Increased from 6s to 10s (Problems.md fix)
const REQUEST_TIMEOUT_RETRY_SECS: u64 = 20;    // ✅ Increased from 12s to 20s (Problems.md fix)

// Legacy constant for backward compatibility (use REQUEST_TIMEOUT_RETRY_SECS)
const REQUEST_TIMEOUT_SECS: u64 = REQUEST_TIMEOUT_RETRY_SECS;

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

/// Internal: Get Jupiter quote with specific timeout
/// 
/// CRITICAL: This is the core function that makes the actual API call.
/// Timeout is configurable to allow adaptive timeout strategy.
async fn get_jupiter_quote_with_timeout(
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: u16,
    timeout_secs: u64,
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
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("Failed to create HTTP client")?;

    // Send request - reqwest timeout will handle both connection and response timeouts
    let response = client
        .get(&url)
        .send()
        .await
        .context(format!(
            "Jupiter API request failed or timed out after {} seconds",
            timeout_secs
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

/// Get Jupiter quote for a swap (backward compatibility wrapper)
/// 
/// Uses default timeout (15 seconds). For adaptive timeout, use get_jupiter_quote_with_retry().
#[allow(dead_code)] // Kept for backward compatibility, but get_jupiter_quote_with_retry is preferred
pub async fn get_jupiter_quote(
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: u16,
) -> Result<JupiterQuote> {
    get_jupiter_quote_with_timeout(
        input_mint,
        output_mint,
        amount,
        slippage_bps,
        REQUEST_TIMEOUT_SECS,
    ).await
}

/// Get Jupiter quote with retry mechanism and adaptive timeout
/// 
/// CRITICAL: Uses adaptive timeout strategy to balance blockhash freshness and API reliability:
/// - First attempt: 10 seconds (default, configurable via JUPITER_TIMEOUT_NORMAL_SECS)
/// - Retries: 20 seconds (default, configurable via JUPITER_TIMEOUT_RETRY_SECS)
/// - Max retries: 2 (configurable via JUPITER_MAX_RETRIES, default: 2)
/// 
/// Blockhash is valid for ~60 seconds. Bundle expires in ~60 seconds.
/// Worst case: 10s (first) + 20s (retry) = 30s for Jupiter, leaving 30s for TX build + send.
/// 
/// NOTE: Jupiter API can sometimes take 30+ seconds during high load periods.
/// These timeout values balance between:
/// - Preserving blockhash freshness (shorter = better)
/// - Handling Jupiter API slowness (longer = better)
/// 
/// If Jupiter is consistently slow, consider:
/// - Increasing JUPITER_TIMEOUT_NORMAL_SECS and JUPITER_TIMEOUT_RETRY_SECS
/// - Accepting that some opportunities may be missed due to blockhash staleness
/// - Or reducing max retries to fail faster
pub async fn get_jupiter_quote_with_retry(
    input_mint: &Pubkey,
    output_mint: &Pubkey,
    amount: u64,
    slippage_bps: u16,
    max_retries: u32,
) -> Result<JupiterQuote> {
    let mut last_error = None;
    
    for attempt in 1..=max_retries {
        // CRITICAL: Use configurable timeout strategy
        // Default: 10s first attempt, 20s for retries (updated to handle Jupiter API high-load)
        // Read from environment variables to allow runtime configuration
        use std::env;
        
        let timeout_secs = if attempt == 1 {
            env::var("JUPITER_TIMEOUT_NORMAL_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(REQUEST_TIMEOUT_NORMAL_SECS) // Default: 10s (balance speed and reliability)
        } else {
            env::var("JUPITER_TIMEOUT_RETRY_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(REQUEST_TIMEOUT_RETRY_SECS) // Default: 20s (handle high-load scenarios)
        };
        
        log::debug!(
            "Jupiter quote attempt {}/{} with {} second timeout",
            attempt,
            max_retries,
            timeout_secs
        );
        
        match get_jupiter_quote_with_timeout(
            input_mint, 
            output_mint, 
            amount, 
            slippage_bps,
            timeout_secs
        ).await {
            Ok(quote) => {
                log::debug!(
                    "✅ Jupiter quote succeeded on attempt {}/{} ({} second timeout)",
                    attempt,
                    max_retries,
                    timeout_secs
                );
                return Ok(quote);
            }
            Err(e) => {
                last_error = Some(e);
                
                // Check if timeout error
                let error_msg = match last_error.as_ref() {
                    Some(e) => e.to_string(),
                    None => "Unknown error".to_string(),
                };
                
                if error_msg.contains("timeout") || error_msg.contains("timed out") {
                    log::warn!(
                        "⏱️  Jupiter quote timeout on attempt {}/{} ({} second timeout)",
                        attempt,
                        max_retries,
                        timeout_secs
                    );
                } else {
                    log::warn!(
                        "⚠️  Jupiter quote failed on attempt {}/{}: {}",
                        attempt,
                        max_retries,
                        error_msg
                    );
                }
                
                if attempt < max_retries {
                    // Exponential backoff with jitter
                    // Base delay: 300ms * attempt (300ms, 600ms, 900ms...)
                    // Jitter: random 0-200ms to prevent thundering herd
                    use rand::Rng;
                    let base_delay_ms = 300 * attempt as u64;
                    let jitter_ms = rand::thread_rng().gen_range(0..200);
                    let delay_ms = base_delay_ms + jitter_ms;
                    
                    log::debug!(
                        "Retrying Jupiter quote in {}ms (base: {}ms + jitter: {}ms)...",
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

/// Build Jupiter swap instructions from quote
/// Returns a vector of instructions that swap input_mint -> output_mint
/// Includes setup instructions (ATA creation), swap instruction, and cleanup instruction (WSOL close)
/// 
/// CRITICAL: This function calls Jupiter's swap-instructions endpoint to get
/// the actual swap instructions that can be included in a transaction.
/// Returns Vec<Instruction> to support setup and cleanup instructions.
pub async fn build_jupiter_swap_instruction(
    quote: &JupiterQuote,
    user_pubkey: &Pubkey,
    jupiter_url: &str,
) -> Result<Vec<Instruction>> {
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
    
    // Use configurable timeout for swap-instructions endpoint
    // Default: 20 seconds (same as retry timeout, since this is a critical operation)
    use std::env;
    let swap_timeout_secs = env::var("JUPITER_TIMEOUT_RETRY_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(REQUEST_TIMEOUT_RETRY_SECS); // Default: 20s
    
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(swap_timeout_secs))
        .build()
        .context("Failed to create HTTP client")?;
    
    let response = client
        .post(&swap_url)
        .json(&payload)
        .send()
        .await
        .context(format!(
            "Jupiter swap instruction request failed or timed out after {} seconds",
            swap_timeout_secs
        ))?;
    
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
    
    let mut instructions = Vec::new();
    
    // 1. Setup Instructions (e.g. create ATAs)
    if let Some(setup_ixs_b64) = swap_response.setup_instructions {
        for ix_b64 in setup_ixs_b64 {
            let ix_bytes = base64::engine::general_purpose::STANDARD
                .decode(&ix_b64)
                .context("Failed to decode setup instruction from base64")?;
            let ix: Instruction = bincode::deserialize(&ix_bytes)
                .context("Failed to deserialize setup instruction")?;
            instructions.push(ix);
        }
    }
    
    // 2. Main Swap Instruction
    let swap_instruction_b64 = swap_response
        .swap_instruction
        .ok_or_else(|| anyhow::anyhow!("Jupiter response missing swapInstruction field"))?;
    
    use base64::Engine;
    let instruction_bytes = base64::engine::general_purpose::STANDARD
        .decode(&swap_instruction_b64)
        .context("Failed to decode swap instruction from base64")?;
    
    let instruction: Instruction = bincode::deserialize(&instruction_bytes)
        .context("Failed to deserialize swap instruction from bytes")?;
    instructions.push(instruction);
    
    // 3. Cleanup Instruction (e.g. close WSOL account)
    if let Some(cleanup_ix_b64) = swap_response.cleanup_instruction {
        let ix_bytes = base64::engine::general_purpose::STANDARD
            .decode(&cleanup_ix_b64)
            .context("Failed to decode cleanup instruction from base64")?;
        let ix: Instruction = bincode::deserialize(&ix_bytes)
            .context("Failed to deserialize cleanup instruction")?;
        instructions.push(ix);
    }
    
    log::debug!(
        "✅ Jupiter swap instructions built: {} instructions total",
        instructions.len()
    );
    
    Ok(instructions)
}

