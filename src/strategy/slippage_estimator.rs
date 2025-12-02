use crate::core::config::Config;
use solana_sdk::pubkey::Pubkey;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

pub struct SlippageEstimator {
    config: Config,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct JupiterQuoteResponse {
    #[serde(rename = "priceImpactPct")]
    price_impact_pct: Option<f64>,
}

impl SlippageEstimator {
    pub fn new(config: Config) -> Self {
        SlippageEstimator {
            config,
            client: reqwest::Client::new(),
        }
    }

    pub async fn estimate_dex_slippage(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
    ) -> Result<u16> {
        if self.config.use_jupiter_api {
            self.estimate_with_jupiter(input_mint, output_mint, amount).await
        } else {
            self.estimate_with_multipliers(amount)
        }
    }

    async fn estimate_with_jupiter(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
    ) -> Result<u16> {
        const MAX_RETRIES: u32 = 3;
        let url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps=50",
            input_mint,
            output_mint,
            amount
        );

        // Helper function for exponential backoff
        // Exponential backoff: 1s, 2s, 4s (better for rate limit handling)
        let get_backoff = |attempt: u32| -> Duration {
            // attempt 1: 1000 * 2^0 = 1000ms = 1s
            // attempt 2: 1000 * 2^1 = 2000ms = 2s
            // attempt 3: 1000 * 2^2 = 4000ms = 4s
            let backoff_ms = 1000 * 2_u64.pow(attempt.saturating_sub(1));
            Duration::from_millis(backoff_ms)
        };

        // Retry loop with exponential backoff
        for attempt in 1..=MAX_RETRIES {
            match self.client.get(&url).send().await {
                Ok(response) => {
                    let status = response.status();
                    
                    if status.is_success() {
                        // Success - parse and return
                        let response_text = match response.text().await {
                            Ok(text) => text,
                            Err(e) => {
                                log::warn!(
                                    "Jupiter API: failed to read response body (attempt {}): {}",
                                    attempt,
                                    e
                                );
                                if attempt < MAX_RETRIES {
                                    let backoff = get_backoff(attempt);
                                    log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                                    tokio::time::sleep(backoff).await;
                                    continue;
                                } else {
                                    log::warn!(
                                        "Jupiter API failed after {} attempts, using fallback",
                                        MAX_RETRIES
                                    );
                                    return self.estimate_with_multipliers(amount);
                                }
                            }
                        };

                        match serde_json::from_str::<JupiterQuoteResponse>(&response_text) {
                            Ok(quote) => {
                                if let Some(price_impact_pct) = quote.price_impact_pct {
                                    // Önce Jupiter API'nin döndürdüğü değeri logla
                                    log::info!("Jupiter raw price_impact_pct: {}", price_impact_pct);

                                    // Ardından doğru çarpanı belirle
                                    let slippage_bps = if price_impact_pct < 1.0 {
                                        // 0.005 formatı (0-1 arası)
                                        (price_impact_pct * 10_000.0) as u16
                                    } else {
                                        // 0.5 formatı (0-100 arası)
                                        (price_impact_pct * 100.0) as u16
                                    };
                                    return Ok(slippage_bps.min(self.config.max_slippage_bps));
                                } else {
                                    log::warn!(
                                        "Jupiter API did not return price impact (attempt {}), will retry",
                                        attempt
                                    );
                                    if attempt < MAX_RETRIES {
                                        let backoff = get_backoff(attempt);
                                        log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                                        tokio::time::sleep(backoff).await;
                                        continue;
                                    } else {
                                        log::warn!(
                                            "Jupiter API failed after {} attempts, using fallback",
                                            MAX_RETRIES
                                        );
                                        return self.estimate_with_multipliers(amount);
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "Jupiter API: failed to parse response (attempt {}): {}",
                                    attempt,
                                    e
                                );
                                if attempt < MAX_RETRIES {
                                    let backoff = get_backoff(attempt);
                                    log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                                    tokio::time::sleep(backoff).await;
                                    continue;
                                } else {
                                    log::warn!(
                                        "Jupiter API failed after {} attempts, using fallback",
                                        MAX_RETRIES
                                    );
                                    return self.estimate_with_multipliers(amount);
                                }
                            }
                        }
                    } else {
                        // HTTP error status
                        let response_text = response.text().await.unwrap_or_default();
                        log::warn!(
                            "Jupiter API error (attempt {}): Status {}, Response: {}",
                            attempt,
                            status,
                            response_text
                        );
                        
                        if attempt < MAX_RETRIES {
                            let backoff = get_backoff(attempt);
                            log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                            tokio::time::sleep(backoff).await;
                            continue;
                        } else {
                            log::warn!(
                                "Jupiter API failed after {} attempts, using fallback",
                                MAX_RETRIES
                            );
                            return self.estimate_with_multipliers(amount);
                        }
                    }
                }
                Err(e) => {
                    // Network error
                    log::warn!(
                        "Jupiter API network error (attempt {}): {}",
                        attempt,
                        e
                    );
                    
                    if attempt < MAX_RETRIES {
                        let backoff = get_backoff(attempt);
                        log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                        tokio::time::sleep(backoff).await;
                        continue;
                    } else {
                        log::warn!(
                            "Jupiter API failed after {} attempts, using fallback",
                            MAX_RETRIES
                        );
                        return self.estimate_with_multipliers(amount);
                    }
                }
            }
        }

        // Should never reach here, but fallback just in case
        log::warn!("Jupiter API failed after {} attempts, using fallback", MAX_RETRIES);
        self.estimate_with_multipliers(amount)
    }

    fn estimate_with_multipliers(&self, amount: u64) -> Result<u16> {
        let size_usd = amount as f64 / 1_000_000.0;
        let multiplier = if size_usd < self.config.slippage_size_small_threshold_usd {
            self.config.slippage_multiplier_small
        } else if size_usd > self.config.slippage_size_large_threshold_usd {
            self.config.slippage_multiplier_large
        } else {
            self.config.slippage_multiplier_medium
        };

        Ok((self.config.max_slippage_bps as f64 * multiplier) as u16)
    }

    pub async fn read_oracle_confidence(&self, mint: Pubkey) -> Result<u16> {
        use crate::protocol::oracle::{get_pyth_oracle_account, read_pyth_price};
        use crate::blockchain::rpc_client::RpcClient;
        use std::sync::Arc;
        
        let rpc = Arc::new(
            RpcClient::new(self.config.rpc_http_url.clone())
                .context("Failed to create RPC client for oracle confidence")?
        );
        
        if let Some(oracle_account) = get_pyth_oracle_account(&mint, Some(&self.config))? {
            if let Some(price_data) = read_pyth_price(&oracle_account, Arc::clone(&rpc), Some(&self.config)).await? {
                let confidence_pct = (price_data.confidence / price_data.price) * 100.0;
                let confidence_bps = (confidence_pct * 100.0) as u16;
                Ok(confidence_bps.min(10000))
            } else {
                Ok(self.config.default_oracle_confidence_slippage_bps)
            }
        } else {
            Ok(self.config.default_oracle_confidence_slippage_bps)
        }
    }
}
