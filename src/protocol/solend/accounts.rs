use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use crate::core::registry::ProgramIds;

fn validate_solend_program_id(program_id: &Pubkey) -> Result<()> {
    let expected_program_id = ProgramIds::solend()?;

    if program_id != &expected_program_id {
        return Err(anyhow::anyhow!(
            "Invalid Solend program ID: expected {}, got {}",
            ProgramIds::SOLEND,
            program_id
        ));
    }

    Ok(())
}

pub fn get_associated_token_program_id(config: Option<&crate::config::Config>) -> Result<Pubkey> {
    // Önce config'den al, yoksa registry'den kullan
    if let Some(cfg) = config {
        cfg.associated_token_program_id.parse()
            .map_err(|_| anyhow::anyhow!("Invalid associated token program ID in config: {}", cfg.associated_token_program_id))
    } else {
        ProgramIds::associated_token()
    }
}

pub fn get_associated_token_address(
    wallet: &Pubkey,
    mint: &Pubkey,
    config: Option<&crate::config::Config>,
) -> Result<Pubkey> {
    let associated_token_program_id = get_associated_token_program_id(config)?;

    let token_program_id = spl_token::id();
    let seeds = &[wallet.as_ref(), token_program_id.as_ref(), mint.as_ref()];

    Pubkey::try_find_program_address(seeds, &associated_token_program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive associated token address"))
}

pub fn derive_lending_market_authority(
    lending_market: &Pubkey,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    validate_solend_program_id(program_id)?;

    let seeds = &[lending_market.as_ref()];

    Pubkey::try_find_program_address(seeds, program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive lending market authority"))
}

pub fn derive_obligation_address(
    wallet_pubkey: &Pubkey,
    lending_market: &Pubkey,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    validate_solend_program_id(program_id)?;

    let seeds = &[
        b"obligation".as_ref(),
        wallet_pubkey.as_ref(),
        lending_market.as_ref(),
    ];

    Pubkey::try_find_program_address(seeds, program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive obligation address"))
}
