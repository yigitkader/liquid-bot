use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use crate::solana_client::SolanaClient;
use pyth_sdk_solana::state::SolanaPriceAccount;

pub const PYTH_PROGRAM_ID: &str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH"; //todo: why this is hardcoded here

pub const SWITCHBOARD_PROGRAM_ID: &str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";  //todo: why this is hardcoded here

fn get_oracle_account_from_mapping(mint: &Pubkey, mapping: &[(&str, &str)], oracle_type: &str) -> Result<Option<Pubkey>> {
    let mint_str = mint.to_string();
    for (known_mint, oracle_account) in mapping {
        if mint_str == *known_mint {
            return Ok(Some(Pubkey::from_str(oracle_account)
                .map_err(|_| anyhow::anyhow!("Invalid oracle account address"))?));
        }
    }
    log::warn!("{} oracle account not found for mint: {}", oracle_type, mint);
    Ok(None)
}

pub fn get_pyth_oracle_account(mint: &Pubkey) -> Result<Option<Pubkey>> {
    let mapping = &[ //todo: why this is hardcoded here
        ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98"),
        ("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "3vxLXJqLqF3JG5TCbYycbKWRBbCJCMx7E4xrTU5XG8Jz"),
        ("So11111111111111111111111111111111111111112", "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
        ("7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs", "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB"),
        ("9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E", "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU"),
    ];
    get_oracle_account_from_mapping(mint, mapping, "Pyth")
}

pub fn get_switchboard_oracle_account(mint: &Pubkey) -> Result<Option<Pubkey>> {
    let mapping = &[//todo: why this is hardcoded here
        ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD"),
        ("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "ETAaeeuQBwsh9mM2gqtwWSbEkf2M8GJ2iVZ3gJgKqJz"),
        ("So11111111111111111111111111111111111111112", "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
    ];
    get_oracle_account_from_mapping(mint, mapping, "Switchboard")
}

pub fn get_oracle_accounts_from_reserve(
    reserve_info: &crate::protocols::reserve_helper::ReserveInfo,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    let pyth = reserve_info.pyth_oracle;
    let switchboard = reserve_info.switchboard_oracle;
    
    if pyth.is_some() || switchboard.is_some() {
        log::info!(
            "✅ Using oracles from reserve account: pyth={:?}, switchboard={:?}",
            pyth,
            switchboard
        );
        return Ok((pyth, switchboard));
    }
    
    let mint = reserve_info.liquidity_mint
        .or(reserve_info.collateral_mint)
        .ok_or_else(|| anyhow::anyhow!("No mint found in reserve info"))?;
    
    log::warn!(
        "⚠️  Oracle accounts not found in reserve, using hardcoded mapping for mint: {}",
        mint
    );
    log::warn!(//todo: why this is hardcoded here
        "   ⚠️  Hardcoded mapping only supports 5 tokens (USDC, USDT, SOL, ETH, BTC). \
         Other tokens (BONK, RAY, SRM, etc.) will fail! \
         Ensure reserve account parsing is working correctly."
    );
    get_oracle_accounts_from_mint(&mint)
}

pub fn get_oracle_accounts_from_mint(
    mint: &Pubkey,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    let pyth = get_pyth_oracle_account(mint)?;
    let switchboard = get_switchboard_oracle_account(mint)?;
    
    if pyth.is_none() && switchboard.is_none() {
        log::warn!(
            "⚠️  No oracle accounts found for mint {} in hardcoded mapping. \
             This token is not supported by hardcoded mapping. \
             Use reserve account parsing instead!",
            mint
        );
    }
    
    Ok((pyth, switchboard))
}
pub fn get_oracle_accounts(
    mint: &Pubkey,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    // Fallback: Hardcoded mapping (reserve account bilgisi yoksa)
    get_oracle_accounts_from_mint(mint)
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
) -> Result<Option<OraclePrice>> {
    let account = match rpc_client.get_account(oracle_account).await {
        Ok(acc) => acc,
        Err(e) => {
            log::warn!("Failed to read Pyth oracle account {}: {}", oracle_account, e);
            return Ok(None);
        }
    };
    
    if account.data.is_empty() {
        log::warn!("Pyth oracle account {} is empty", oracle_account);
        return Ok(None);
    }
    
    let mut account_mut = account;
    let price_feed = match SolanaPriceAccount::account_to_feed(
        oracle_account,
        &mut account_mut,
    ) {
        Ok(feed) => feed,
        Err(e) => {
            log::warn!(
                "Failed to parse Pyth price feed from account {}: {}",
                oracle_account,
                e
            );
            return Ok(None);
        }
    };
    
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
        .as_secs() as i64;
    
    let price_data = match price_feed.get_price_no_older_than(current_time, 60) {
        Some(data) => data,
        None => {
            log::warn!(
                "No current price available (or price is stale) for Pyth oracle account {}",
                oracle_account
            );
            return Ok(None);
        }
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
    // TODO: Switchboard oracle parse implementasyonu
    // Switchboard yapısı Pyth'den farklı, daha karmaşık
    Ok(None)
}

pub async fn read_oracle_price(
    pyth_account: Option<&Pubkey>,
    switchboard_account: Option<&Pubkey>,
    rpc_client: Arc<SolanaClient>,
) -> Result<Option<OraclePrice>> {
    if let Some(pyth_pubkey) = pyth_account {
        if let Some(price) = read_pyth_price(pyth_pubkey, Arc::clone(&rpc_client)).await? {
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

