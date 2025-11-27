use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

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

pub fn derive_obligation_address(
    wallet_pubkey: &Pubkey,
    lending_market: &Pubkey,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    let seeds = &[
        b"obligation".as_ref(),
        wallet_pubkey.as_ref(),
        lending_market.as_ref(),
    ];
    
    Pubkey::try_find_program_address(seeds, program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive obligation address"))
}


