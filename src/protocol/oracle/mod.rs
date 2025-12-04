pub mod pyth;
pub mod switchboard;

use crate::blockchain::rpc_client::RpcClient;
use crate::utils::helpers;
use anyhow::Result;
use pyth_sdk_solana::state::SolanaPriceAccount;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub fn get_pyth_program_id(config: Option<&crate::config::Config>) -> &str {
    config
        .map(|c| c.pyth_program_id.as_str())
        .unwrap_or("FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH")
}

pub fn get_switchboard_program_id(config: Option<&crate::config::Config>) -> &str {
    config
        .map(|c| c.switchboard_program_id.as_str())
        .unwrap_or("SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f")
}

pub fn get_pyth_oracle_account(
    mint: &Pubkey,
    config: Option<&crate::config::Config>,
) -> Result<Option<Pubkey>> {
    if let Some(cfg) = config {
        for json in [&cfg.oracle_mappings_json, &cfg.default_pyth_oracle_mappings_json] {
            if let Some(ref mappings_json) = json {
                if let Ok(Some(account)) = parse_oracle_mappings_from_json(mappings_json, mint, "pyth") {
                    return Ok(Some(account));
                }
            }
        }
    }

    let mappings = [
        (config.and_then(|c| c.usdc_mint.parse::<Pubkey>().ok()).or_else(|| "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".parse().ok()), "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98"),
        (config.and_then(|c| c.sol_mint.parse::<Pubkey>().ok()).or_else(|| "So11111111111111111111111111111111111111112".parse().ok()), "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
        (config.and_then(|c| c.usdt_mint.as_ref()?.parse::<Pubkey>().ok()).or_else(|| "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB".parse().ok()), "3vxLXJqLqF3JG5TCbYycbKWRBbCJCMx7E4xrTU5XG8Jz"),
        (config.and_then(|c| c.eth_mint.as_ref()?.parse::<Pubkey>().ok()).or_else(|| "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs".parse().ok()), "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB"),
        (config.and_then(|c| c.btc_mint.as_ref()?.parse::<Pubkey>().ok()).or_else(|| "9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E".parse().ok()), "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU"),
    ];

    for (check_mint, oracle) in mappings.iter() {
        if let Some(check) = check_mint {
            if *mint == *check {
                if let Ok(pyth) = oracle.parse::<Pubkey>() {
                    return Ok(Some(pyth));
                }
            }
        }
    }

    Ok(None)
}

pub fn get_switchboard_oracle_account(
    mint: &Pubkey,
    config: Option<&crate::config::Config>,
) -> Result<Option<Pubkey>> {
    if let Some(cfg) = config {
        for json in [&cfg.oracle_mappings_json, &cfg.default_switchboard_oracle_mappings_json] {
            if let Some(ref mappings_json) = json {
                if let Ok(Some(account)) = parse_oracle_mappings_from_json(mappings_json, mint, "switchboard") {
                    return Ok(Some(account));
                }
            }
        }
    }

    let mappings = [
        (config.and_then(|c| c.usdc_mint.parse::<Pubkey>().ok()).or_else(|| "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".parse().ok()), "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD"),
        (config.and_then(|c| c.usdt_mint.as_ref()?.parse::<Pubkey>().ok()).or_else(|| "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB".parse().ok()), "ETAaeeuQBwsh9mM2gqtwWSbEkf2M8GJ2iVZ3gJgKqJz"),
        (config.and_then(|c| c.sol_mint.parse::<Pubkey>().ok()).or_else(|| "So11111111111111111111111111111111111111112".parse().ok()), "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
    ];

    for (check_mint, oracle) in mappings.iter() {
        if let Some(check) = check_mint {
            if *mint == *check {
                if let Ok(switchboard) = oracle.parse::<Pubkey>() {
                    return Ok(Some(switchboard));
                }
            }
        }
    }

    Ok(None)
}

fn parse_oracle_mappings_from_json(
    json: &str,
    mint: &Pubkey,
    oracle_type: &str,
) -> Result<Option<Pubkey>> {
    let mappings: HashMap<String, serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("Failed to parse ORACLE_MAPPINGS_JSON: {}", e))?;

    Ok(mappings
        .get(&mint.to_string())
        .and_then(|token_config| token_config.get(oracle_type)?.as_str())
        .and_then(|account_str| helpers::parse_pubkey_opt(account_str)))
}

pub fn get_oracle_accounts_from_reserve(
    pyth_oracle: Option<Pubkey>,
    switchboard_oracle: Option<Pubkey>,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    if pyth_oracle.is_some() || switchboard_oracle.is_some() {
        return Ok((pyth_oracle, switchboard_oracle));
    }

    log::warn!(
        "No oracle accounts found for reserve. \
         This is acceptable for certain asset pairs (e.g., stablecoin/stablecoin). \
         Proceeding with estimated pricing."
    );

    Ok((None, None))
}

pub fn get_oracle_accounts_from_mint(
    mint: &Pubkey,
    config: Option<&crate::config::Config>,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    Ok((
        get_pyth_oracle_account(mint, config)?,
        get_switchboard_oracle_account(mint, config)?,
    ))
}

#[derive(Debug, Clone)]
pub struct OraclePrice {
    pub price: f64,
    pub confidence: f64,
    pub exponent: i32,
    pub timestamp: i64,
}

pub async fn read_pyth_price(
    oracle_account: &Pubkey,
    rpc_client: Arc<RpcClient>,
    config: Option<&crate::config::Config>,
) -> Result<Option<OraclePrice>> {
    log::debug!("Reading Pyth oracle price from account: {}", oracle_account);

    if let Some(cfg) = config {
        if cfg.is_free_rpc_endpoint() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    let account = match rpc_client.get_account(oracle_account).await {
        Ok(acc) if !acc.data.is_empty() => {
            log::debug!(
                "Pyth oracle account fetched: {} bytes, owner: {}",
                acc.data.len(),
                acc.owner
            );
            acc
        }
        Ok(acc) => {
            log::warn!(
                "Pyth oracle account {} is empty ({} bytes)",
                oracle_account,
                acc.data.len()
            );
            return Ok(None);
        }
        Err(e) => {
            log::error!(
                "Failed to fetch Pyth oracle account {}: {}",
                oracle_account,
                e
            );
            return Ok(None);
        }
    };

    let mut account_mut = account;
    match SolanaPriceAccount::account_to_feed(oracle_account, &mut account_mut) {
        Ok(_feed) => {
            log::debug!(
                "Pyth price feed parsed successfully for account: {}",
                oracle_account
            );
        }
        Err(e) => {
            log::error!("Failed to parse Pyth price feed for account {}: {}. Account data size: {} bytes, owner: {}", 
                oracle_account, e, account_mut.data.len(), account_mut.owner);
            return Ok(None);
        }
    };

    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
        .as_secs() as i64;

    let max_age_seconds = config.map(|c| c.max_oracle_age_seconds).unwrap_or(60);
    log::debug!(
        "Checking price data freshness: current_time={}, max_age={}s",
        current_time,
        max_age_seconds
    );

    // ✅ CRITICAL FIX: Wrap Pyth SDK call in spawn_blocking with timeout
    // Pyth SDK's get_price_no_older_than is SYNC code and can hang or panic
    // spawn_blocking moves it to a blocking thread pool, and timeout ensures it doesn't hang forever
    // This prevents the entire async runtime from blocking if Pyth SDK has issues
    //
    // ✅ CRITICAL FIX: Use RPC client timeout for consistency
    // RPC client timeout is 10s (default), so oracle timeout should match
    // This prevents: Oracle timeout (5s) → RPC timeout (10s) → Total 15s wait
    // Instead: Oracle timeout (10s) matches RPC timeout (10s) → Total 10s wait
    use solana_sdk::account::Account;
    use std::panic::catch_unwind;

    let oracle_timeout = rpc_client.request_timeout();
    let account_data = account_mut.data.clone();
    let account_owner = account_mut.owner;
    let account_lamports = account_mut.lamports;
    let oracle_account_clone = *oracle_account;
    let current_time_clone = current_time;
    let max_age_seconds_clone = max_age_seconds;

    let price_data_result = tokio::time::timeout(
        oracle_timeout,
        tokio::task::spawn_blocking(move || {
            let mut account_for_feed = Account {
                lamports: account_lamports,
                data: account_data,
                owner: account_owner,
                executable: false,
                rent_epoch: 0,
            };

            let price_feed = match SolanaPriceAccount::account_to_feed(
                &oracle_account_clone,
                &mut account_for_feed,
            ) {
                Ok(feed) => feed,
                Err(e) => {
                    log::error!("Failed to parse Pyth price feed in blocking thread: {}", e);
                    return Err(anyhow::anyhow!("Failed to parse price feed"));
                }
            };

            Ok(catch_unwind(std::panic::AssertUnwindSafe(|| {
                price_feed.get_price_no_older_than(current_time_clone, max_age_seconds_clone)
            }))
            .unwrap_or_else(|_| None))
        }),
    )
    .await;

    let price_data = match price_data_result {
        Ok(Ok(result)) => match result {
            Ok(Some(data)) => {
                log::debug!(
                    "Pyth price data found: price={}, expo={}, conf={}, publish_time={}",
                    data.price,
                    data.expo,
                    data.conf,
                    data.publish_time
                );
                data
            }
            Ok(None) => {
                log::warn!(
                    "Pyth price data is stale or not found for account {} (max_age={}s)",
                    oracle_account,
                    max_age_seconds
                );
                return Ok(None);
            }
            Err(_) => {
                log::error!("Pyth SDK panic detected when reading price for account {} - this may indicate malformed data or SDK bug", oracle_account);
                return Ok(None);
            }
        },
        Ok(Err(e)) => {
            log::error!(
                "Pyth SDK blocking task failed for account {}: {}",
                oracle_account,
                e
            );
            return Ok(None);
        }
        Err(_) => {
            log::error!(
                "Pyth SDK call timeout ({:?}) for account {} - SDK may be hanging or RPC is slow",
                oracle_timeout,
                oracle_account
            );
            return Err(anyhow::anyhow!(
                "Pyth SDK timeout after {:?} (matches RPC client timeout)",
                oracle_timeout
            ));
        }
    };

    let price = price_data.price as f64 * 10_f64.powi(price_data.expo);
    let confidence = price_data.conf as f64 * 10_f64.powi(price_data.expo);

    log::info!("Pyth oracle price read successfully: account={}, price=${:.4}, confidence=${:.4}, timestamp={}", 
        oracle_account, price, confidence, price_data.publish_time);

    Ok(Some(OraclePrice {
        price,
        confidence,
        exponent: price_data.expo,
        timestamp: price_data.publish_time,
    }))
}

pub async fn read_switchboard_price(
    oracle_account: &Pubkey,
    rpc_client: Arc<RpcClient>,
) -> Result<Option<OraclePrice>> {
    use crate::protocol::oracle::switchboard::SwitchboardOracle;
    match SwitchboardOracle::read_price(oracle_account, rpc_client).await {
        Ok(price_data) => {
            log::info!("Switchboard oracle price read successfully: account={}, price=${:.4}, confidence=${:.4}, timestamp={}", 
                oracle_account, price_data.price, price_data.confidence, price_data.timestamp);
            Ok(Some(OraclePrice {
                price: price_data.price,
                confidence: price_data.confidence,
                exponent: 0,
                timestamp: price_data.timestamp,
            }))
        }
        Err(e) => {
            log::error!("Failed to read Switchboard oracle price from account {}: {}", oracle_account, e);
            Ok(None)
        }
    }
}

pub async fn read_oracle_price(
    pyth_account: Option<&Pubkey>,
    switchboard_account: Option<&Pubkey>,
    rpc_client: Arc<RpcClient>,
    config: Option<&crate::config::Config>,
) -> Result<Option<OraclePrice>> {
    if let Some(pyth_pubkey) = pyth_account {
        if let Some(price) = read_pyth_price(pyth_pubkey, Arc::clone(&rpc_client), config).await? {
            return Ok(Some(price));
        }
    }
    if let Some(switchboard_pubkey) = switchboard_account {
        if let Some(price) = read_switchboard_price(switchboard_pubkey, rpc_client).await? {
            return Ok(Some(price));
        }
    }
    Ok(None)
}
