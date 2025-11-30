//! Oracle Reading Tests
//!
//! Tests for oracle account reading from reserve accounts.
//! 
//! These tests verify:
//! 1. Oracle accounts are correctly read from reserve accounts
//! 2. When oracle accounts are not found, returns Ok((None, None)) (not error)
//!    - This is acceptable for certain asset pairs (e.g., stablecoin/stablecoin)
//!    - The caller should handle this case appropriately (e.g., reject opportunity)
//! 3. Integration with reserve parsing

use anyhow::Result;
use liquid_bot::protocols::oracle_helper::get_oracle_accounts_from_reserve;
use liquid_bot::protocols::reserve_helper::ReserveInfo;
use solana_sdk::pubkey::Pubkey;

#[test]
fn test_get_oracle_accounts_from_reserve_with_oracles() {
    // Test case: Reserve with both Pyth and Switchboard oracles
    let pyth_oracle = Pubkey::new_unique();
    let switchboard_oracle = Pubkey::new_unique();
    
    let reserve_info = ReserveInfo {
        reserve_pubkey: Pubkey::new_unique(),
        mint: Some(Pubkey::new_unique()),
        ltv: 0.7,
        liquidation_threshold: 0.77,
        borrow_rate: 0.05,
        liquidity_mint: Some(Pubkey::new_unique()),
        collateral_mint: Some(Pubkey::new_unique()),
        liquidity_supply: Some(Pubkey::new_unique()),
        collateral_supply: Some(Pubkey::new_unique()),
        liquidation_bonus: 0.03,
        pyth_oracle: Some(pyth_oracle),
        switchboard_oracle: Some(switchboard_oracle),
    };
    
    let result = get_oracle_accounts_from_reserve(&reserve_info);
    assert!(result.is_ok(), "Should succeed when oracles are present");
    
    let (pyth, switchboard) = result.unwrap();
    assert_eq!(pyth, Some(pyth_oracle), "Pyth oracle should match");
    assert_eq!(switchboard, Some(switchboard_oracle), "Switchboard oracle should match");
}

#[test]
fn test_get_oracle_accounts_from_reserve_with_pyth_only() {
    // Test case: Reserve with only Pyth oracle
    let pyth_oracle = Pubkey::new_unique();
    
    let reserve_info = ReserveInfo {
        reserve_pubkey: Pubkey::new_unique(),
        mint: Some(Pubkey::new_unique()),
        ltv: 0.7,
        liquidation_threshold: 0.77,
        borrow_rate: 0.05,
        liquidity_mint: Some(Pubkey::new_unique()),
        collateral_mint: Some(Pubkey::new_unique()),
        liquidity_supply: Some(Pubkey::new_unique()),
        collateral_supply: Some(Pubkey::new_unique()),
        liquidation_bonus: 0.03,
        pyth_oracle: Some(pyth_oracle),
        switchboard_oracle: None,
    };
    
    let result = get_oracle_accounts_from_reserve(&reserve_info);
    assert!(result.is_ok(), "Should succeed when at least one oracle is present");
    
    let (pyth, switchboard) = result.unwrap();
    assert_eq!(pyth, Some(pyth_oracle), "Pyth oracle should match");
    assert_eq!(switchboard, None, "Switchboard oracle should be None");
}

#[test]
fn test_get_oracle_accounts_from_reserve_with_switchboard_only() {
    // Test case: Reserve with only Switchboard oracle
    let switchboard_oracle = Pubkey::new_unique();
    
    let reserve_info = ReserveInfo {
        reserve_pubkey: Pubkey::new_unique(),
        mint: Some(Pubkey::new_unique()),
        ltv: 0.7,
        liquidation_threshold: 0.77,
        borrow_rate: 0.05,
        liquidity_mint: Some(Pubkey::new_unique()),
        collateral_mint: Some(Pubkey::new_unique()),
        liquidity_supply: Some(Pubkey::new_unique()),
        collateral_supply: Some(Pubkey::new_unique()),
        liquidation_bonus: 0.03,
        pyth_oracle: None,
        switchboard_oracle: Some(switchboard_oracle),
    };
    
    let result = get_oracle_accounts_from_reserve(&reserve_info);
    assert!(result.is_ok(), "Should succeed when at least one oracle is present");
    
    let (pyth, switchboard) = result.unwrap();
    assert_eq!(pyth, None, "Pyth oracle should be None");
    assert_eq!(switchboard, Some(switchboard_oracle), "Switchboard oracle should match");
}

#[test]
fn test_get_oracle_accounts_from_reserve_no_oracles_returns_none() {
    // Test case: Reserve with no oracles - should return Ok((None, None)) (not error)
    // This is acceptable for certain asset pairs (e.g., stablecoin/stablecoin)
    // The caller should handle this case appropriately (e.g., reject opportunity)
    let reserve_pubkey = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    
    let reserve_info = ReserveInfo {
        reserve_pubkey,
        mint: Some(mint),
        ltv: 0.7,
        liquidation_threshold: 0.77,
        borrow_rate: 0.05,
        liquidity_mint: Some(mint),
        collateral_mint: Some(Pubkey::new_unique()),
        liquidity_supply: Some(Pubkey::new_unique()),
        collateral_supply: Some(Pubkey::new_unique()),
        liquidation_bonus: 0.03,
        pyth_oracle: None,
        switchboard_oracle: None,
    };
    
    let result = get_oracle_accounts_from_reserve(&reserve_info);
    assert!(result.is_ok(), "Should return Ok when no oracles are found (not error)");
    
    let (pyth, switchboard) = result.unwrap();
    assert_eq!(pyth, None, "Pyth oracle should be None");
    assert_eq!(switchboard, None, "Switchboard oracle should be None");
}

#[test]
fn test_get_oracle_accounts_from_reserve_no_mint_returns_none() {
    // Test case: Reserve with no oracles and no mint - should return Ok((None, None))
    // This is acceptable - the caller should handle this case appropriately
    let reserve_info = ReserveInfo {
        reserve_pubkey: Pubkey::new_unique(),
        mint: None,
        ltv: 0.7,
        liquidation_threshold: 0.77,
        borrow_rate: 0.05,
        liquidity_mint: None,
        collateral_mint: None,
        liquidity_supply: Some(Pubkey::new_unique()),
        collateral_supply: Some(Pubkey::new_unique()),
        liquidation_bonus: 0.03,
        pyth_oracle: None,
        switchboard_oracle: None,
    };
    
    let result = get_oracle_accounts_from_reserve(&reserve_info);
    assert!(result.is_ok(), "Should return Ok when no oracles are found (not error)");
    
    let (pyth, switchboard) = result.unwrap();
    assert_eq!(pyth, None, "Pyth oracle should be None");
    assert_eq!(switchboard, None, "Switchboard oracle should be None");
}

