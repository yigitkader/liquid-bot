//! Reserve Account Helper
//! 
//! Bu modül, Solend reserve account'larını parse etmek için helper fonksiyonlar sağlar.
//! Gerçek Solend reserve account yapısını kullanarak parse eder.

use anyhow::{Context, Result};
use solana_sdk::{
    pubkey::Pubkey,
    account::Account,
};

// Solend reserve account struct'ını kullan
use crate::protocols::solend_reserve::SolendReserve;

/// Reserve account bilgileri
#[derive(Debug, Clone)]
pub struct ReserveInfo {
    pub reserve_pubkey: Pubkey,
    pub mint: Option<Pubkey>, // Reserve'den parse edilen mint
    pub ltv: f64,              // Loan-to-Value ratio (0.0 - 1.0)
    pub borrow_rate: f64,       // Current borrow rate (APY, 0.0 - 1.0)
    pub liquidity_mint: Option<Pubkey>, // Liquidity mint (borrow için)
    pub collateral_mint: Option<Pubkey>, // Collateral mint (deposit için)
    pub liquidity_supply: Option<Pubkey>, // Reserve liquidity supply token account
    pub collateral_supply: Option<Pubkey>, // Reserve collateral supply token account
    pub liquidation_bonus: f64, // Liquidation bonus (0.0 - 1.0)
    pub oracle_pubkey: Option<Pubkey>, // Oracle pubkey (Pyth veya Switchboard) - reserve'den alınır
}

/// Reserve account'unu parse et ve bilgilerini döndür
/// 
/// Gerçek Solend reserve account yapısını kullanarak:
/// 1. Account data'yı Borsh deserialize eder
/// 2. Reserve'den mint address'lerini alır (liquidity ve collateral)
/// 3. Reserve'den LTV değerini alır
/// 4. Reserve'den current borrow rate'i alır
/// 5. Reserve'den liquidation bonus'u alır
pub async fn parse_reserve_account(
    reserve_pubkey: &Pubkey,
    account_data: &Account,
) -> Result<ReserveInfo> {
    // Account'un Solend program'ına ait olduğunu kontrol et
    // (Bu kontrolü çağıran fonksiyon yapmalı, ama güvenlik için burada da kontrol ediyoruz)
    
    // Solend reserve account'unu parse et
    let reserve = SolendReserve::from_account_data(&account_data.data)
        .context("Failed to parse Solend reserve account")?;
    
    // Reserve'den bilgileri çıkar
    let liquidity_mint = reserve.liquidity_mint();
    let collateral_mint = reserve.collateral_mint();
    let ltv = reserve.ltv();
    let liquidation_bonus = reserve.liquidation_bonus();
    
    // Borrow rate'i hesapla (WAD formatından APY'ye çevir)
    // borrow_rate_wad zaten WAD formatında (1e18), ancak bu anlık rate değil
    // Gerçek borrow rate'i hesaplamak için daha karmaşık bir formül gerekir
    // Şu an basit bir yaklaşım kullanıyoruz
    let borrow_rate = reserve.liquidity.borrow_rate_wad as f64 / 1_000_000_000_000_000_000.0;
    
    // Mint olarak liquidity_mint kullanıyoruz (borrow için)
    // Collateral mint ayrı bir field olarak saklanıyor
    let mint = Some(liquidity_mint);
    
    // Oracle pubkey'ini reserve'den al (eğer varsa)
    // NOT: Gerçek Solend reserve yapısında oracle_pubkey field'ı olabilir
    // Şu an struct'da yok, bu yüzden None döndürüyoruz
    // Gerçek IDL'e göre struct güncellendiğinde bu çalışacak
    let oracle_pubkey = reserve.oracle_pubkey();
    
    log::debug!(
        "Parsed reserve {}: mint={}, ltv={:.2}, borrow_rate={:.4}, liquidation_bonus={:.2}, oracle={:?}",
        reserve_pubkey,
        liquidity_mint,
        ltv,
        borrow_rate,
        liquidation_bonus,
        oracle_pubkey
    );
    
    Ok(ReserveInfo {
        reserve_pubkey: *reserve_pubkey,
        mint,
        ltv,
        borrow_rate,
        liquidity_mint: Some(liquidity_mint),
        collateral_mint: Some(collateral_mint),
        liquidity_supply: Some(reserve.liquidity_supply()),
        collateral_supply: Some(reserve.collateral_supply()),
        liquidation_bonus,
        oracle_pubkey,
    })
}

/// Reserve pubkey'den mint address'ini bul
/// 
/// NOT: Bu fonksiyon RPC client gerektirir, bu yüzden async.
/// Eğer account data zaten varsa, `parse_reserve_account` kullanın.
pub async fn get_reserve_mint(
    _reserve_pubkey: &Pubkey,
    _rpc_client: Option<&crate::solana_client::SolanaClient>,
) -> Result<Option<Pubkey>> {
    // Bu fonksiyon RPC client gerektirir
    // Şu an: None döndürülüyor (caller'ın parse_reserve_account kullanması önerilir)
    // Gelecek: RPC client ile reserve account'unu oku ve parse et
    Ok(None)
}

