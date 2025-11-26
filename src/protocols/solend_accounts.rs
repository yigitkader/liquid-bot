//! Solend Account Helper Functions
//! 
//! Bu modül, Solend liquidation instruction için gerekli account'ları hesaplamak için
//! helper fonksiyonlar sağlar.

use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

/// Associated Token Account (ATA) adresini hesaplar
/// 
/// ATA = Associated Token Account, bir wallet'ın belirli bir mint için token account'u
/// Formül: PDA(owner, [token_program_id, mint, owner], associated_token_program_id)
pub fn get_associated_token_address(
    wallet: &Pubkey,
    mint: &Pubkey,
) -> Result<Pubkey> {
    // SPL Associated Token Program ID
    let associated_token_program_id = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
        .map_err(|_| anyhow::anyhow!("Invalid associated token program ID"))?;
    
    // Token Program ID
    let token_program_id = spl_token::id();
    
    // PDA seeds: [wallet, token_program_id, mint]
    let seeds = &[
        wallet.as_ref(),
        token_program_id.as_ref(),
        mint.as_ref(),
    ];
    
    // PDA derivation
    Pubkey::try_find_program_address(seeds, &associated_token_program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive associated token address"))
}

/// Lending Market Authority PDA'sını hesaplar
/// 
/// Solend'de lending market authority bir PDA'dır.
/// Formül: PDA(lending_market, [b"lending-market-authority"], program_id)
pub fn derive_lending_market_authority(
    lending_market: &Pubkey,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    let seeds = &[
        b"lending-market-authority".as_ref(),
        lending_market.as_ref(),
    ];
    
    Pubkey::try_find_program_address(seeds, program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive lending market authority"))
}

/// Oracle account'ını bulur (Pyth veya Switchboard)
/// 
/// NOT: Şu an placeholder. Gerçek implementasyonda reserve account'undan oracle pubkey alınmalı.
pub fn find_oracle_account(
    _reserve: &Pubkey,
    _oracle_type: &str, // "pyth" veya "switchboard"
) -> Result<Option<Pubkey>> {
    // Gelecek İyileştirme: Reserve account'undan oracle pubkey'i al
    // Şu an: None döndürülüyor (placeholder)
    Ok(None)
}

