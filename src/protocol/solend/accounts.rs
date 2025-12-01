use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub fn get_associated_token_program_id(config: Option<&crate::config::Config>) -> Result<Pubkey> {
    let program_id_str = config
        .map(|c| c.associated_token_program_id.as_str())
        .unwrap_or("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
    
    Pubkey::from_str(program_id_str)
        .map_err(|_| anyhow::anyhow!("Invalid associated token program ID: {}", program_id_str))
}

pub fn get_associated_token_address(
    wallet: &Pubkey,
    mint: &Pubkey,
    config: Option<&crate::config::Config>,
) -> Result<Pubkey> {
    let associated_token_program_id = get_associated_token_program_id(config)?;
    
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


