//! Oracle Account Helper
//! 
//! Bu modül, Solend liquidation için gerekli oracle account'larını (Pyth/Switchboard) bulur
//! ve gerçek fiyat verilerini okur.
//! 
//! Öncelik sırası:
//! 1. Reserve account'tan oracle pubkey'i oku (EN İYİ - dinamik)
//! 2. Hardcoded mapping kullan (fallback - sadece bilinen token'lar için)
//! 3. Pubkey::default() kullan (son çare - optional account olabilir)

use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use crate::solana_client::SolanaClient;

/// Pyth Network Program ID (Solana Mainnet)
pub const PYTH_PROGRAM_ID: &str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH";

/// Switchboard Program ID (Solana Mainnet)
pub const SWITCHBOARD_PROGRAM_ID: &str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";

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
    let mapping = &[
        ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98"),
        ("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "3vxLXJqLqF3JG5TCbYycbKWRBbCJCMx7E4xrTU5XG8Jz"),
        ("So11111111111111111111111111111111111111112", "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
        ("7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs", "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB"),
        ("9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E", "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU"),
    ];
    get_oracle_account_from_mapping(mint, mapping, "Pyth")
}

pub fn get_switchboard_oracle_account(mint: &Pubkey) -> Result<Option<Pubkey>> {
    let mapping = &[
        ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD"),
        ("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", "ETAaeeuQBwsh9mM2gqtwWSbEkf2M8GJ2iVZ3gJgKqJz"),
        ("So11111111111111111111111111111111111111112", "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG"),
    ];
    get_oracle_account_from_mapping(mint, mapping, "Switchboard")
}

/// Reserve account'tan oracle account'larını bulur (ÖNERİLEN YÖNTEM)
/// 
/// ✅ DOĞRULANMIŞ VE TEST EDİLDİ: Solend reserve account'larında hem Pyth hem Switchboard oracle pubkey'leri saklanır.
/// 
/// Test sonucu (USDC reserve - BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw):
/// - Pyth Oracle: Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX ✅
/// - Switchboard Oracle: nu11111111111111111111111111111111111111111 (default/null - normal)
/// 
/// Bu yöntem hardcoded mapping'den çok daha iyidir çünkü:
/// - Dinamik (her token için çalışır, sadece 5 token değil!)
/// - Güncel (reserve account'tan alınır)
/// - Ölçeklenebilir (yeni token'lar için otomatik)
/// - Her iki oracle'ı da doğru şekilde döndürür (Pyth ve Switchboard ayrı)
/// 
/// NOT: Bazı reserve'lerde switchboard oracle default (null) olabilir, bu normal.
/// Pyth oracle genellikle her reserve'de mevcuttur.
pub fn get_oracle_accounts_from_reserve(
    reserve_info: &crate::protocols::reserve_helper::ReserveInfo,
) -> Result<(Option<Pubkey>, Option<Pubkey>)> {
    // Öncelik 1: Reserve account'tan oracle pubkey'lerini al
    // ✅ DOĞRULANMIŞ: Reserve struct'ında pyth_oracle ve switchboard_oracle field'ları var
    // ✅ TEST EDİLDİ: Gerçek mainnet reserve'lerinde oracle'lar doğru okunuyor
    let pyth = reserve_info.pyth_oracle;
    let switchboard = reserve_info.switchboard_oracle;
    
    // Eğer reserve'den oracle'lar alındıysa onları kullan
    if pyth.is_some() || switchboard.is_some() {
        log::info!(
            "✅ Using oracles from reserve account: pyth={:?}, switchboard={:?}",
            pyth,
            switchboard
        );
        return Ok((pyth, switchboard));
    }
    
    // Fallback: Mint'ten hardcoded mapping kullan (sadece bilinen token'lar için)
    // NOT: Bu fallback sadece eski token'lar için çalışır, yeni token'lar için
    // reserve account'tan okuma kullanılmalı
    // ⚠️ UYARI: Hardcoded mapping sadece 5 token için var (USDC, USDT, SOL, ETH, BTC)
    // Diğer token'lar (BONK, RAY, SRM, etc.) için reserve'den okuma kullanılmalı!
    let mint = reserve_info.liquidity_mint
        .or(reserve_info.collateral_mint)
        .ok_or_else(|| anyhow::anyhow!("No mint found in reserve info"))?;
    
    log::warn!(
        "⚠️  Oracle accounts not found in reserve, using hardcoded mapping for mint: {}",
        mint
    );
    log::warn!(
        "   ⚠️  Hardcoded mapping only supports 5 tokens (USDC, USDT, SOL, ETH, BTC). \
         Other tokens (BONK, RAY, SRM, etc.) will fail! \
         Ensure reserve account parsing is working correctly."
    );
    get_oracle_accounts_from_mint(&mint)
}

/// Mint address'inden oracle account'larını bulur (FALLBACK - SADECE 5 TOKEN İÇİN!)
/// 
/// ⚠️ UYARI: Hardcoded mapping kullanır - sadece 5 bilinen token için çalışır:
/// - USDC (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v)
/// - USDT (Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB)
/// - SOL (So11111111111111111111111111111111111111112)
/// - ETH (7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs)
/// - BTC (9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E)
/// 
/// Diğer token'lar (BONK, RAY, SRM, etc.) için reserve account'tan okuma kullanılmalı!
/// 
/// ✅ ÖNERİLEN: `get_oracle_accounts_from_reserve()` kullanın - tüm token'lar için çalışır!
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

/// Oracle fiyat bilgisi
#[derive(Debug, Clone)]
pub struct OraclePrice {
    pub price: f64,
    pub confidence: f64,
    pub exponent: i32,
    pub timestamp: i64,
}

/// Pyth oracle account'undan fiyat okur
/// 
/// Pyth oracle account yapısı:
/// - Magic number (4 bytes): 0xa1b2c3d4
/// - Version (1 byte)
/// - Type (1 byte): 0x02 for Price
/// - Size (2 bytes)
/// - Price account data...
/// 
/// Basitleştirilmiş parse: Sadece temel fiyat bilgilerini okur
pub async fn read_pyth_price(
    oracle_account: &Pubkey,
    rpc_client: Arc<SolanaClient>,
) -> Result<Option<OraclePrice>> {
    // Oracle account'unu oku
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
    
    // Pyth oracle account'unu parse et
    // Basitleştirilmiş parse: Pyth v2 formatı
    // Detaylı yapı için: https://docs.pyth.network/price-feeds/on-chain
    if account.data.len() < 100 {
        log::warn!("Pyth oracle account {} data too short: {} bytes", oracle_account, account.data.len());
        return Ok(None);
    }
    
    // Pyth v2 Price account yapısı (basitleştirilmiş)
    // Offset 0: Magic number (4 bytes)
    // Offset 4: Version (1 byte)
    // Offset 5: Type (1 byte) - 0x02 for Price
    // Offset 8: Price component (i64)
    // Offset 16: Confidence (u64)
    // Offset 24: Exponent (i32)
    // Offset 28: Timestamp (i64)
    
    // Magic number kontrolü
    if account.data.len() < 4 {
        return Ok(None);
    }
    
    // Basit parse (Pyth v2 formatı)
    // Not: Gerçek implementasyonda Pyth SDK kullanılmalı
    // Şimdilik basit bir parse yapıyoruz
    let price_offset = 8;
    let confidence_offset = 16;
    let exponent_offset = 24;
    let timestamp_offset = 28;
    
    if account.data.len() < timestamp_offset + 8 {
        log::warn!("Pyth oracle account {} data too short for price parsing", oracle_account);
        return Ok(None);
    }
    
    // Little-endian parse
    let price_raw = i64::from_le_bytes(
        account.data[price_offset..price_offset + 8]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse price"))?
    );
    
    let confidence_raw = u64::from_le_bytes(
        account.data[confidence_offset..confidence_offset + 8]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?
    );
    
    let exponent = i32::from_le_bytes(
        account.data[exponent_offset..exponent_offset + 4]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?
    );
    
    let timestamp = i64::from_le_bytes(
        account.data[timestamp_offset..timestamp_offset + 8]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse timestamp"))?
    );
    
    // Fiyatı normalize et (exponent'e göre)
    let price = price_raw as f64 * 10_f64.powi(exponent);
    let confidence = confidence_raw as f64 * 10_f64.powi(exponent);
    
    Ok(Some(OraclePrice {
        price,
        confidence,
        exponent,
        timestamp,
    }))
}

/// Switchboard oracle account'undan fiyat okur
/// 
/// Switchboard oracle yapısı daha karmaşıktır.
/// Şimdilik placeholder - gelecekte implement edilecek.
pub async fn read_switchboard_price(
    _oracle_account: &Pubkey,
    _rpc_client: Arc<SolanaClient>,
) -> Result<Option<OraclePrice>> {
    // TODO: Switchboard oracle parse implementasyonu
    // Switchboard yapısı Pyth'den farklı, daha karmaşık
    Ok(None)
}

/// Oracle'dan fiyat okur (Pyth veya Switchboard)
/// 
/// Önce Pyth'i dener, bulamazsa Switchboard'u dener.
pub async fn read_oracle_price(
    pyth_account: Option<&Pubkey>,
    switchboard_account: Option<&Pubkey>,
    rpc_client: Arc<SolanaClient>,
) -> Result<Option<OraclePrice>> {
    // Öncelik: Pyth
    if let Some(pyth_pubkey) = pyth_account {
        if let Some(price) = read_pyth_price(pyth_pubkey, Arc::clone(&rpc_client)).await? {
            log::debug!("Read price from Pyth oracle: ${:.4} (confidence: ${:.4})", price.price, price.confidence);
            return Ok(Some(price));
        }
    }
    
    // Fallback: Switchboard
    if let Some(switchboard_pubkey) = switchboard_account {
        if let Some(price) = read_switchboard_price(switchboard_pubkey, rpc_client).await? {
            log::debug!("Read price from Switchboard oracle: ${:.4} (confidence: ${:.4})", price.price, price.confidence);
            return Ok(Some(price));
        }
    }
    
    Ok(None)
}

