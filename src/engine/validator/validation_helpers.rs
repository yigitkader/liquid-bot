use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use crate::protocol::oracle::OraclePrice;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

/// Validation helper functions
pub struct ValidationHelpers;

impl ValidationHelpers {
    /// Validate oracle price data
    pub fn validate_price_data(
        price_data: &OraclePrice,
        mint: &Pubkey,
        oracle_type: &str,
        max_oracle_age_seconds: u64,
    ) -> Result<()> {
        const MAX_PRICE: f64 = 1_000_000_000_000.0;
        const MIN_PRICE: f64 = 0.0001;
        const MAX_CONF_RATIO: f64 = 0.25;

        let age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Time error: {}", e))?
            .as_secs() as i64 - price_data.timestamp;

        if age > max_oracle_age_seconds as i64 {
            return Err(anyhow::anyhow!(
                "{} oracle stale for {}: {}s old (max: {}s)",
                oracle_type,
                mint,
                age,
                max_oracle_age_seconds
            ));
        }
        if price_data.price <= 0.0 {
            return Err(anyhow::anyhow!(
                "{} oracle invalid price for {}: {:.4}",
                oracle_type,
                mint,
                price_data.price
            ));
        }
        if !(MIN_PRICE..=MAX_PRICE).contains(&price_data.price) {
            return Err(anyhow::anyhow!(
                "{} oracle price out of range for {}: {:.4}",
                oracle_type,
                mint,
                price_data.price
            ));
        }
        if price_data.confidence < 0.0 {
            return Err(anyhow::anyhow!(
                "{} oracle negative confidence for {}: {:.4}",
                oracle_type,
                mint,
                price_data.confidence
            ));
        }
        if price_data.price > 0.0 {
            let conf_ratio = price_data.confidence / price_data.price;
            if conf_ratio > MAX_CONF_RATIO {
                return Err(anyhow::anyhow!(
                    "{} oracle confidence too high for {}: {:.2}% (max: {:.0}%)",
                    oracle_type,
                    mint,
                    conf_ratio * 100.0,
                    MAX_CONF_RATIO * 100.0
                ));
            }
        }
        Ok(())
    }

    /// Check oracle freshness for a mint
    pub async fn check_oracle_freshness(
        mint: &Pubkey,
        rpc: &Arc<RpcClient>,
        config: &Config,
        skip_delay: bool,
    ) -> Result<()> {
        use crate::protocol::oracle::{get_pyth_oracle_account, get_switchboard_oracle_account, read_oracle_price};
        use tokio::time::{Duration, timeout};

        let oracle_check_timeout = rpc.request_timeout();

        let timeout_result = timeout(oracle_check_timeout, async {
            if config.is_free_rpc_endpoint() && !skip_delay {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            let pyth_account = get_pyth_oracle_account(mint, Some(config)).ok().flatten();
            let switchboard_account = get_switchboard_oracle_account(mint, Some(config)).ok().flatten();

            if pyth_account.is_none() && switchboard_account.is_none() {
                log::warn!("No oracle found (Pyth or Switchboard) for mint: {}, skipping freshness check", mint);
                return Err(anyhow::anyhow!("No oracle found for mint {}", mint));
            }

            let oracle_type = if pyth_account.is_some() { "Pyth" } else { "Switchboard" };
            let oracle_account = pyth_account.or(switchboard_account).unwrap();

            let price_data = match read_oracle_price(
                pyth_account.as_ref(),
                switchboard_account.as_ref(),
                Arc::clone(rpc),
                Some(config),
            )
            .await
            {
                Ok(Some(data)) => data,
                Ok(None) => {
                    return Err(anyhow::anyhow!(
                        "Failed to read oracle price data for mint {} ({} oracle={}) - price data is None",
                        mint,
                        oracle_type,
                        oracle_account
                    ));
                }
                Err(e) => {
                    let error_str = e.to_string().to_lowercase();
                    if error_str.contains("timeout") || error_str.contains("timed out") {
                        log::error!(
                            "Validator: {} oracle read timeout for mint {} (oracle={}) - RPC may be slow or unresponsive: {}",
                            oracle_type,
                            mint,
                            oracle_account,
                            e
                        );
                        return Err(anyhow::anyhow!(
                            "{} oracle read timeout for mint {} (oracle={}) - RPC may be slow or unresponsive: {}",
                            oracle_type,
                            mint,
                            oracle_account,
                            e
                        ));
                    } else {
                        log::error!(
                            "Validator: failed to read {} price for mint {} (oracle={}): {}",
                            oracle_type,
                            mint,
                            oracle_account,
                            e
                        );
                        return Err(anyhow::anyhow!(
                            "Failed to read oracle price data for mint {} ({} oracle={}): {}",
                            mint,
                            oracle_type,
                            oracle_account,
                            e
                        ));
                    }
                }
            };

            Self::validate_price_data(&price_data, mint, oracle_type, config.max_oracle_age_seconds)?;

            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                .as_secs() as i64;
            let age_seconds = current_time - price_data.timestamp;

            log::debug!(
                "Validator: {} oracle price is valid and fresh for mint {} (oracle={}, age={}s, max_age={}s, price={:.4}, confidence={:.4}, conf_ratio={:.2}%)",
                oracle_type,
                mint,
                oracle_account,
                age_seconds,
                config.max_oracle_age_seconds,
                price_data.price,
                price_data.confidence,
                if price_data.price > 0.0 {
                    (price_data.confidence / price_data.price) * 100.0
                } else {
                    0.0
                }
            );

            Ok(())
        })
        .await;

        timeout_result
            .map_err(|_| anyhow::anyhow!("Oracle check timeout after {:?} for mint {}", oracle_check_timeout, mint))??;
        Ok(())
    }
}

