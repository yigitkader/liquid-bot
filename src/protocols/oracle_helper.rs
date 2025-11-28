use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use crate::solana_client::SolanaClient;
use crate::utils;
use pyth_sdk_solana::state::SolanaPriceAccount;

pub const PYTH_PROGRAM_ID: &str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH";
pub const SWITCHBOARD_PROGRAM_ID: &str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";

pub fn get_pyth_program_id(config: Option<&crate::config::Config>) -> &str {
    config.map(|c| c.pyth_program_id.as_str()).unwrap_or(PYTH_PROGRAM_ID)
}

pub fn get_switchboard_program_id(config: Option<&crate::config::Config>) -> &str {
    config.map(|c| c.switchboard_program_id.as_str()).unwrap_or(SWITCHBOARD_PROGRAM_ID)
}

fn get_oracle_account_from_mapping(mint: &Pubkey, mapping: &[(&str, &str)]) -> Option<Pubkey> {
    let mint_str = mint.to_string();
    mapping.iter()
        .find(|(known_mint, _)| mint_str == *known_mint)
        .and_then(|(_, oracle_account)| utils::parse_pubkey_opt(oracle_account))
}

pub fn get_pyth_oracle_account(mint: &Pubkey, config: Option<&crate::config::Config>) -> Result<Option<Pubkey>> {
    if let Some(cfg) = config {
        if let Some(ref mappings_json) = cfg.oracle_mappings_json {
            if let Ok(Some(account)) = parse_oracle_mappings_from_json(mappings_json, mint, "pyth") {
                return Ok(Some(account));
            }
        }
    }
    
    let mapping = &[
        ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98"),
        ("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "3vxLXJqLqF3JG5TCbYycbKWRBbCJCMx7E4xrTU5XG8Jz"),
        ("So11111111111111111111111111111111111111112", "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
        ("7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs", "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB"),
        ("9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E", "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU"),
    ];
    Ok(get_oracle_account_from_mapping(mint, mapping))
}

pub fn get_switchboard_oracle_account(mint: &Pubkey, config: Option<&crate::config::Config>) -> Result<Option<Pubkey>> {
    if let Some(cfg) = config {
        if let Some(ref mappings_json) = cfg.oracle_mappings_json {
            if let Ok(Some(account)) = parse_oracle_mappings_from_json(mappings_json, mint, "switchboard") {
                return Ok(Some(account));
            }
        }
    }
    
    let mapping = &[
        ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD"),
        ("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "ETAaeeuQBwsh9mM2gqtwWSbEkf2M8GJ2iVZ3gJgKqJz"),
        ("So11111111111111111111111111111111111111112", "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
    ];
    Ok(get_oracle_account_from_mapping(mint, mapping))
}

fn parse_oracle_mappings_from_json(json: &str, mint: &Pubkey, oracle_type: &str) -> Result<Option<Pubkey>> {
    let mappings: HashMap<String, serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("Failed to parse ORACLE_MAPPINGS_JSON: {}", e))?;
    
    Ok(mappings
        .get(&mint.to_string())
        .and_then(|token_config| token_config.get(oracle_type)?.as_str())
        .and_then(|account_str| utils::parse_pubkey_opt(account_str)))
}

pub fn get_oracle_accounts_from_reserve(
    reserve_info: &crate::protocols::reserve_helper::ReserveInfo,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    if reserve_info.pyth_oracle.is_some() || reserve_info.switchboard_oracle.is_some() {
        return Ok((reserve_info.pyth_oracle, reserve_info.switchboard_oracle));
    }
    
    Err(anyhow::anyhow!(
        "CRITICAL: No oracle accounts found in reserve {}. \
         This indicates reserve data corruption or version mismatch. \
         DO NOT proceed with liquidation without oracle data.",
        reserve_info.reserve_pubkey
    ))
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
    rpc_client: Arc<SolanaClient>,
    config: Option<&crate::config::Config>,
) -> Result<Option<OraclePrice>> {
    let account = match rpc_client.get_account(oracle_account).await {
        Ok(acc) if !acc.data.is_empty() => acc,
        _ => return Ok(None),
    };
    
    let mut account_mut = account;
    let price_feed = match SolanaPriceAccount::account_to_feed(oracle_account, &mut account_mut) {
        Ok(feed) => feed,
        Err(e) => {
            log::warn!("Failed to parse Pyth price feed: {}", e);
            return Ok(None);
        }
    };
    
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
        .as_secs() as i64;
    
    let max_age_seconds = config.map(|c| c.max_oracle_age_seconds).unwrap_or(60);
    let price_data = match price_feed.get_price_no_older_than(current_time, max_age_seconds) {
        Some(data) => data,
        None => return Ok(None),
    };
    
    let price = price_data.price as f64 * 10_f64.powi(price_data.expo);
    let confidence = price_data.conf as f64 * 10_f64.powi(price_data.expo);
    
    Ok(Some(OraclePrice {
        price,
        confidence,
        exponent: price_data.expo,
        timestamp: price_data.publish_time,
    }))
}

pub async fn read_switchboard_price(
    _oracle_account: &Pubkey,
    _rpc_client: Arc<SolanaClient>,
) -> Result<Option<OraclePrice>> {
    Ok(None)
}

pub async fn read_oracle_price(
    pyth_account: Option<&Pubkey>,
    switchboard_account: Option<&Pubkey>,
    rpc_client: Arc<SolanaClient>,
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

