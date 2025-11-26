//! Solend Account Helper Functions
//! 
//! Bu modül, Solend liquidation instruction için gerekli account'ları hesaplamak için
//! helper fonksiyonlar sağlar.

use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

// NOT: Lending Market Authority PDA için seed kullanılmıyor!
// Solend SDK'ya göre sadece lending_market pubkey'i seed olarak kullanılıyor.
// Reference: https://github.com/solendprotocol/solend-sdk/blob/master/src/core/LendingMarket.ts
// 
// export const deriveLendingMarketAuthority = (lendingMarket: PublicKey, programId: PublicKey) => {
//   return PublicKey.findProgramAddressSync(
//     [lendingMarket.toBuffer()],  // Sadece lending_market, başka seed yok!
//     programId
//   );
// };

/// Associated Token Account (ATA) adresini hesaplar
pub fn get_associated_token_address(
    wallet: &Pubkey,
    mint: &Pubkey,
) -> Result<Pubkey> {
    let associated_token_program_id = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
        .map_err(|_| anyhow::anyhow!("Invalid associated token program ID"))?;
    
    let token_program_id = spl_token::id();
    let seeds = &[
        wallet.as_ref(),
        token_program_id.as_ref(),
        mint.as_ref(),
    ];
    
    Pubkey::try_find_program_address(seeds, &associated_token_program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive associated token address"))
}

/// Lending Market Authority PDA'sını hesaplar
/// 
/// Solend'de lending market authority bir PDA'dır.
/// 
/// ✅ DOĞRULANMIŞ: Solend SDK'ya göre güncellendi
/// 
/// PDA derivation formülü (Solend SDK'dan):
/// - Seeds: [lending_market] (SADECE lending_market, başka seed YOK!)
/// - Program ID: Solend program ID
/// 
/// Referans: Solend SDK - LendingMarket.ts
/// https://github.com/solendprotocol/solend-sdk/blob/master/src/core/LendingMarket.ts
/// 
/// ```typescript
/// export const deriveLendingMarketAuthority = (lendingMarket: PublicKey, programId: PublicKey) => {
///   return PublicKey.findProgramAddressSync(
///     [lendingMarket.toBuffer()],  // Sadece lending_market!
///     programId
///   );
/// };
/// ```
pub fn derive_lending_market_authority(
    lending_market: &Pubkey,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    // ✅ DOĞRU: Solend SDK'ya göre sadece lending_market seed olarak kullanılıyor
    // "lending-market-authority" string'i kullanılmıyor!
    let seeds = &[
        lending_market.as_ref(),
    ];
    
    Pubkey::try_find_program_address(seeds, program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive lending market authority"))
}


