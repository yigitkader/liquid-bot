// Address validation modülü

use anyhow::Result;
use liquid_bot::core::config::Config;
use liquid_bot::core::registry::{ProgramIds, ReserveAddresses, LendingMarketAddresses};
use solana_sdk::pubkey::Pubkey;
use super::{ValidationBuilder, TestResult};

pub async fn validate_addresses(config: Option<&Config>) -> Result<Vec<TestResult>> {
    let mut builder = ValidationBuilder::new();

    let solend_program_id_str = config
        .map(|c| c.solend_program_id.as_str())
        .unwrap_or(ProgramIds::SOLEND);
    
    builder = builder.check_result_with_details(
        "Solend Program ID",
        Pubkey::try_from(solend_program_id_str),
        "Valid",
        |pid| format!("Address: {}", pid),
    );

    let main_market_str = config
        .and_then(|c| c.main_lending_market_address.as_ref().map(|s| s.as_str()))
        .unwrap_or(LendingMarketAddresses::MAIN);
    
    builder = builder.check_result_with_details(
        "Main Lending Market",
        main_market_str.parse::<Pubkey>(),
        "Valid",
        |market| format!("Address: {}", market),
    );

    let usdc_reserve_str = config
        .and_then(|c| c.usdc_reserve_address.as_ref().map(|s| s.as_str()))
        .unwrap_or(ReserveAddresses::USDC);
    let sol_reserve_str = config
        .and_then(|c| c.sol_reserve_address.as_ref().map(|s| s.as_str()))
        .unwrap_or(ReserveAddresses::SOL);
    
    let known_reserves = vec![
        ("USDC Reserve", usdc_reserve_str),
        ("SOL Reserve", sol_reserve_str),
    ];

    for (name, addr_str) in known_reserves {
        builder = builder.check_result(
            name,
            addr_str.parse::<Pubkey>(),
            &format!("Valid: {}", addr_str),
        );
    }

    Ok(builder.build())
}

