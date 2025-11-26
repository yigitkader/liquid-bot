//! Oracle Account Helper
//! 
//! Bu modül, Solend liquidation için gerekli oracle account'larını (Pyth/Switchboard) bulur.
//! 
//! NOT: Solend'de oracle account'ları genellikle mint address'inden türetilir veya
//! bilinen oracle mapping'lerinden alınır.

use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

/// Pyth Network Program ID (Solana Mainnet)
pub const PYTH_PROGRAM_ID: &str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH";

/// Switchboard Program ID (Solana Mainnet)
pub const SWITCHBOARD_PROGRAM_ID: &str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";

/// Mint address'inden Pyth oracle account'unu bulur
/// 
/// NOT: Bu basit bir yaklaşım. Gerçek implementasyonda:
/// 1. Solend'in oracle mapping'ini kullan (eğer varsa)
/// 2. Veya gerçek Pyth price feed account'unu mint'ten türet
/// 
/// Şu an: Bilinen mint'ler için hardcoded mapping kullanılıyor
/// Gelecek: Pyth SDK kullanarak dinamik olarak bul
pub fn get_pyth_oracle_account(mint: &Pubkey) -> Result<Option<Pubkey>> {
    // Bilinen mint'ler için Pyth oracle account'ları
    // Bu mapping Solend'in kullandığı oracle account'larına göre güncellenmeli
    let oracle_mapping: Vec<(&str, &str)> = vec![
        // USDC
        ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98"),
        // USDT
        ("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "3vxLXJqLqF3JG5TCbYycbKWRBbCJCMx7E4xrTU5XG8Jz"),
        // SOL
        ("So11111111111111111111111111111111111111112", "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
        // ETH
        ("7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs", "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB"),
        // BTC
        ("9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E", "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU"),
    ];
    
    let mint_str = mint.to_string();
    for (known_mint, oracle_account) in oracle_mapping {
        if mint_str == known_mint {
            return Ok(Some(Pubkey::from_str(oracle_account)
                .map_err(|_| anyhow::anyhow!("Invalid oracle account address"))?));
        }
    }
    
    // Mapping'de yoksa None döndür (fallback olarak Pubkey::default() kullanılabilir)
    log::warn!("Pyth oracle account not found for mint: {}", mint);
    Ok(None)
}

/// Mint address'inden Switchboard oracle account'unu bulur
/// 
/// NOT: Bu basit bir yaklaşım. Gerçek implementasyonda:
/// 1. Solend'in oracle mapping'ini kullan (eğer varsa)
/// 2. Veya gerçek Switchboard price feed account'unu mint'ten türet
/// 
/// Şu an: Bilinen mint'ler için hardcoded mapping kullanılıyor
/// Gelecek: Switchboard SDK kullanarak dinamik olarak bul
pub fn get_switchboard_oracle_account(mint: &Pubkey) -> Result<Option<Pubkey>> {
    // Bilinen mint'ler için Switchboard oracle account'ları
    // Bu mapping Solend'in kullandığı oracle account'larına göre güncellenmeli
    let oracle_mapping: Vec<(&str, &str)> = vec![
        // USDC
        ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD"),
        // USDT
        ("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "ETAaeeuQBwsh9mM2gqtwWSbEkf2M8GJ2iVZ3gJgKqJz"),
        // SOL
        ("So11111111111111111111111111111111111111112", "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
    ];
    
    let mint_str = mint.to_string();
    for (known_mint, oracle_account) in oracle_mapping {
        if mint_str == known_mint {
            return Ok(Some(Pubkey::from_str(oracle_account)
                .map_err(|_| anyhow::anyhow!("Invalid oracle account address"))?));
        }
    }
    
    // Mapping'de yoksa None döndür (fallback olarak Pubkey::default() kullanılabilir)
    log::warn!("Switchboard oracle account not found for mint: {}", mint);
    Ok(None)
}

/// Her iki oracle account'unu da bulur (Pyth ve Switchboard)
/// 
/// Solend liquidation instruction'ı her iki oracle'ı da bekliyor olabilir.
/// Eğer bir oracle bulunamazsa, Pubkey::default() kullanılabilir (optional account).
pub fn get_oracle_accounts(
    mint: &Pubkey,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    let pyth = get_pyth_oracle_account(mint)?;
    let switchboard = get_switchboard_oracle_account(mint)?;
    
    Ok((pyth, switchboard))
}

