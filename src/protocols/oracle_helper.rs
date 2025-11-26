//! Oracle Account Helper
//! 
//! Bu modül, Solend liquidation için gerekli oracle account'larını (Pyth/Switchboard) bulur.
//! 
//! Öncelik sırası:
//! 1. Reserve account'tan oracle pubkey'i oku (EN İYİ - dinamik)
//! 2. Hardcoded mapping kullan (fallback - sadece bilinen token'lar için)
//! 3. Pubkey::default() kullan (son çare - optional account olabilir)

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

/// Reserve account'tan oracle account'larını bulur (ÖNERİLEN YÖNTEM)
/// 
/// Solend reserve account'larında oracle pubkey saklanır.
/// Bu yöntem hardcoded mapping'den çok daha iyidir çünkü:
/// - Dinamik (her token için çalışır)
/// - Güncel (reserve account'tan alınır)
/// - Ölçeklenebilir (yeni token'lar için otomatik)
/// 
/// NOT: Gerçek Solend reserve yapısında oracle_pubkey field'ı olmalı.
/// Şu an struct'da yok, bu yüzden fallback kullanılıyor.
pub fn get_oracle_accounts_from_reserve(
    reserve_info: &crate::protocols::reserve_helper::ReserveInfo,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    // Öncelik 1: Reserve account'tan oracle pubkey'i al
    if let Some(oracle_pubkey) = reserve_info.oracle_pubkey {
        // Solend'de genellikle tek bir oracle kullanılır (Pyth veya Switchboard)
        // Hangi oracle olduğunu anlamak için oracle account'unu kontrol etmek gerekir
        // Şimdilik her ikisini de aynı oracle olarak kullanıyoruz
        // Gelecek: Oracle account'unu parse ederek Pyth/Switchboard ayrımı yap
        
        log::debug!("Using oracle from reserve account: {}", oracle_pubkey);
        return Ok((Some(oracle_pubkey), Some(oracle_pubkey)));
    }
    
    // Fallback: Mint'ten hardcoded mapping kullan
    let mint = reserve_info.liquidity_mint
        .or(reserve_info.collateral_mint)
        .ok_or_else(|| anyhow::anyhow!("No mint found in reserve info"))?;
    
    get_oracle_accounts_from_mint(&mint)
}

/// Mint address'inden oracle account'larını bulur (FALLBACK)
/// 
/// Hardcoded mapping kullanır - sadece bilinen token'lar için çalışır.
/// Yeni token'lar için reserve account'tan okuma kullanılmalı.
pub fn get_oracle_accounts_from_mint(
    mint: &Pubkey,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    let pyth = get_pyth_oracle_account(mint)?;
    let switchboard = get_switchboard_oracle_account(mint)?;
    
    Ok((pyth, switchboard))
}

/// Her iki oracle account'unu da bulur (Pyth ve Switchboard)
/// 
/// Solend liquidation instruction'ı her iki oracle'ı da bekliyor olabilir.
/// Eğer bir oracle bulunamazsa, Pubkey::default() kullanılabilir (optional account).
/// 
/// Öncelik: Reserve account'tan okuma > Hardcoded mapping > Default
pub fn get_oracle_accounts(
    mint: &Pubkey,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    // Fallback: Hardcoded mapping (reserve account bilgisi yoksa)
    get_oracle_accounts_from_mint(mint)
}

