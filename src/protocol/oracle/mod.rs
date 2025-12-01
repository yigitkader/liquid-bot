pub mod pyth;
pub mod switchboard;

use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use crate::blockchain::rpc_client::RpcClient;
use crate::utils::helpers;
use pyth_sdk_solana::state::SolanaPriceAccount;

pub fn get_pyth_program_id(config: Option<&crate::config::Config>) -> &str {
    config.map(|c| c.pyth_program_id.as_str()).unwrap_or("FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH")
}

pub fn get_switchboard_program_id(config: Option<&crate::config::Config>) -> &str {
    config.map(|c| c.switchboard_program_id.as_str()).unwrap_or("SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f")
}


pub fn get_pyth_oracle_account(mint: &Pubkey, config: Option<&crate::config::Config>) -> Result<Option<Pubkey>> {
    if let Some(cfg) = config {
        if let Some(ref mappings_json) = cfg.oracle_mappings_json {
            if let Ok(Some(account)) = parse_oracle_mappings_from_json(mappings_json, mint, "pyth") {
                return Ok(Some(account));
            }
        }
        if let Some(ref mappings_json) = cfg.default_pyth_oracle_mappings_json {
            if let Ok(Some(account)) = parse_oracle_mappings_from_json(mappings_json, mint, "pyth") {
                return Ok(Some(account));
            }
        }
    }
    
    if let Some(cfg) = config {
        if let Ok(usdc) = cfg.usdc_mint.parse::<Pubkey>() {
            if *mint == usdc {
                if let Ok(pyth) = "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98".parse::<Pubkey>() {
                    return Ok(Some(pyth));
                }
            }
        }
        if let Ok(sol) = cfg.sol_mint.parse::<Pubkey>() {
            if *mint == sol {
                if let Ok(pyth) = "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG".parse::<Pubkey>() {
                    return Ok(Some(pyth));
                }
            }
        }
        if let Some(ref usdt_str) = cfg.usdt_mint {
            if let Ok(usdt) = usdt_str.parse::<Pubkey>() {
                if *mint == usdt {
                    if let Ok(pyth) = "3vxLXJqLqF3JG5TCbYycbKWRBbCJCMx7E4xrTU5XG8Jz".parse::<Pubkey>() {
                        return Ok(Some(pyth));
                    }
                }
            }
        }
        if let Some(ref eth_str) = cfg.eth_mint {
            if let Ok(eth) = eth_str.parse::<Pubkey>() {
                if *mint == eth {
                    if let Ok(pyth) = "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB".parse::<Pubkey>() {
                        return Ok(Some(pyth));
                    }
                }
            }
        }
        if let Some(ref btc_str) = cfg.btc_mint {
            if let Ok(btc) = btc_str.parse::<Pubkey>() {
                if *mint == btc {
                    if let Ok(pyth) = "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU".parse::<Pubkey>() {
                        return Ok(Some(pyth));
                    }
                }
            }
        }
    } else {
        let default_usdc: Pubkey = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".parse().unwrap();
        let default_sol: Pubkey = "So11111111111111111111111111111111111111112".parse().unwrap();
        let default_usdt: Pubkey = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB".parse().unwrap();
        let default_eth: Pubkey = "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs".parse().unwrap();
        let default_btc: Pubkey = "9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E".parse().unwrap();
        
        if *mint == default_usdc {
            if let Ok(pyth) = "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98".parse::<Pubkey>() {
                return Ok(Some(pyth));
            }
        }
        if *mint == default_sol {
            if let Ok(pyth) = "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG".parse::<Pubkey>() {
                return Ok(Some(pyth));
            }
        }
        if *mint == default_usdt {
            if let Ok(pyth) = "3vxLXJqLqF3JG5TCbYycbKWRBbCJCMx7E4xrTU5XG8Jz".parse::<Pubkey>() {
                return Ok(Some(pyth));
            }
        }
        if *mint == default_eth {
            if let Ok(pyth) = "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB".parse::<Pubkey>() {
                return Ok(Some(pyth));
            }
        }
        if *mint == default_btc {
            if let Ok(pyth) = "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU".parse::<Pubkey>() {
                return Ok(Some(pyth));
            }
        }
    }
    
    Ok(None)
}

pub fn get_switchboard_oracle_account(mint: &Pubkey, config: Option<&crate::config::Config>) -> Result<Option<Pubkey>> {
    if let Some(cfg) = config {
        if let Some(ref mappings_json) = cfg.oracle_mappings_json {
            if let Ok(Some(account)) = parse_oracle_mappings_from_json(mappings_json, mint, "switchboard") {
                return Ok(Some(account));
            }
        }
        if let Some(ref mappings_json) = cfg.default_switchboard_oracle_mappings_json {
            if let Ok(Some(account)) = parse_oracle_mappings_from_json(mappings_json, mint, "switchboard") {
                return Ok(Some(account));
            }
        }
    }
    
    if let Some(cfg) = config {
        if let Ok(usdc) = cfg.usdc_mint.parse::<Pubkey>() {
            if *mint == usdc {
                if let Ok(switchboard) = "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD".parse::<Pubkey>() {
                    return Ok(Some(switchboard));
                }
            }
        }
        if let Some(ref usdt_str) = cfg.usdt_mint {
            if let Ok(usdt) = usdt_str.parse::<Pubkey>() {
                if *mint == usdt {
                    if let Ok(switchboard) = "ETAaeeuQBwsh9mM2gqtwWSbEkf2M8GJ2iVZ3gJgKqJz".parse::<Pubkey>() {
                        return Ok(Some(switchboard));
                    }
                }
            }
        }
        if let Ok(sol) = cfg.sol_mint.parse::<Pubkey>() {
            if *mint == sol {
                if let Ok(switchboard) = "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG".parse::<Pubkey>() {
                    return Ok(Some(switchboard));
                }
            }
        }
    } else {
        let default_usdc: Pubkey = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".parse().unwrap();
        let default_usdt: Pubkey = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB".parse().unwrap();
        let default_sol: Pubkey = "So11111111111111111111111111111111111111112".parse().unwrap();
        
        if *mint == default_usdc {
            if let Ok(switchboard) = "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD".parse::<Pubkey>() {
                return Ok(Some(switchboard));
            }
        }
        if *mint == default_usdt {
            if let Ok(switchboard) = "ETAaeeuQBwsh9mM2gqtwWSbEkf2M8GJ2iVZ3gJgKqJz".parse::<Pubkey>() {
                return Ok(Some(switchboard));
            }
        }
        if *mint == default_sol {
            if let Ok(switchboard) = "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG".parse::<Pubkey>() {
                return Ok(Some(switchboard));
            }
        }
    }
    
    Ok(None)
}

fn parse_oracle_mappings_from_json(json: &str, mint: &Pubkey, oracle_type: &str) -> Result<Option<Pubkey>> {
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
    
    let account = match rpc_client.get_account(oracle_account).await {
        Ok(acc) if !acc.data.is_empty() => {
            log::debug!("Pyth oracle account fetched: {} bytes, owner: {}", acc.data.len(), acc.owner);
            acc
        },
        Ok(acc) => {
            log::warn!("Pyth oracle account {} is empty ({} bytes)", oracle_account, acc.data.len());
            return Ok(None);
        },
        Err(e) => {
            log::error!("Failed to fetch Pyth oracle account {}: {}", oracle_account, e);
            return Ok(None);
        }
    };
    
    let mut account_mut = account;
    let price_feed = match SolanaPriceAccount::account_to_feed(oracle_account, &mut account_mut) {
        Ok(feed) => {
            log::debug!("Pyth price feed parsed successfully for account: {}", oracle_account);
            feed
        },
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
    log::debug!("Checking price data freshness: current_time={}, max_age={}s", current_time, max_age_seconds);
    
    let price_data = match price_feed.get_price_no_older_than(current_time, max_age_seconds) {
        Some(data) => {
            log::debug!("Pyth price data found: price={}, expo={}, conf={}, publish_time={}", 
                data.price, data.expo, data.conf, data.publish_time);
            data
        },
        None => {
            let latest_price = price_feed.get_price_no_older_than(current_time, u64::MAX);
            if let Some(latest) = latest_price {
                let age_seconds = current_time - latest.publish_time;
                log::warn!("Pyth price data is stale for account {}: age={}s (max={}s), latest_price={}, expo={}, publish_time={}", 
                    oracle_account, age_seconds, max_age_seconds, latest.price, latest.expo, latest.publish_time);
            } else {
                log::error!("No price data found in Pyth feed for account: {}", oracle_account);
            }
            return Ok(None);
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
    _oracle_account: &Pubkey,
    _rpc_client: Arc<RpcClient>,
) -> Result<Option<OraclePrice>> {
    Ok(None)
}

pub async fn read_oracle_price(
    pyth_account: Option<&Pubkey>,
    switchboard_account: Option<&Pubkey>,
    rpc_client: Arc<RpcClient>,
    config: Option<&crate::config::Config>,
) -> Result<Option<OraclePrice>> {
    if let Some(pyth_pubkey) = pyth_account {
        if let Some(price) = read_pyth_price(pyth_pubkey, Arc::clone(&rpc_client), config).await? {
            log::debug!("Read price from Pyth oracle: ${:.4} (confidence: ${:.4})", price.price, price.confidence);
            return Ok(Some(price));
        }
    }
    
    if let Some(switchboard_pubkey) = switchboard_account {
        if let Some(price) = read_switchboard_price(switchboard_pubkey, rpc_client).await? {
            log::debug!("Read price from Switchboard oracle: ${:.4} (confidence: ${:.4})", price.price, price.confidence);
            return Ok(Some(price));
        }
    }
    
    Ok(None)
}

