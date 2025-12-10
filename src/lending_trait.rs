// Common trait for lending protocol obligations
// This allows us to write generic code that works with both Solend and Kamino

use solana_sdk::pubkey::Pubkey;

/// Trait for lending protocol obligations
/// Implemented by both Solend and Kamino obligations
pub trait LendingObligation {
    /// Get total borrowed value in USD
    fn borrowed_value_usd(&self) -> f64;
    
    /// Get total deposited value in USD
    fn deposited_value_usd(&self) -> f64;
    
    /// Get allowed borrow value in USD
    fn allowed_borrow_value_usd(&self) -> f64;
    
    /// Calculate health factor
    /// Returns f64::INFINITY if no debt
    fn health_factor(&self) -> f64;
    
    /// Get obligation owner (wallet pubkey)
    fn owner(&self) -> Pubkey;
    
    /// Get lending market pubkey
    fn lending_market(&self) -> Pubkey;
    
    /// Check if obligation has any debt
    fn has_any_debt(&self) -> bool;
    
    /// Check if obligation has any deposits
    fn has_any_deposits(&self) -> bool;
    
    /// Check if obligation is stale
    fn is_stale(&self) -> bool;
}

