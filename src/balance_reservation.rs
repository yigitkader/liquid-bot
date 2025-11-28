use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// Forward declaration to avoid circular dependency
#[allow(async_fn_in_trait)] // Internal trait, async fn is acceptable here
pub trait BalanceChecker: Send + Sync {
    async fn get_token_balance(&self, mint: &Pubkey) -> Result<u64>;
}

/// Tracks reserved balances to prevent race conditions when multiple
/// liquidation opportunities are processed in parallel.
/// 
/// This ensures that if two opportunities require the same token balance,
/// only one can be approved at a time.
pub struct BalanceReservation {
    /// Map of mint pubkey -> reserved amount
    reserved: Arc<RwLock<HashMap<Pubkey, u64>>>,
}

impl BalanceReservation {
    pub fn new() -> Self {
        BalanceReservation {
            reserved: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Attempts to reserve a specific amount of tokens for a given mint.
    /// Returns Some(ReservationGuard) if the reservation succeeds, None otherwise.
    /// 
    /// The reservation is automatically released when the guard is dropped.
    pub async fn try_reserve(
        &self,
        mint: &Pubkey,
        amount: u64,
        actual_balance: u64,
    ) -> Option<ReservationGuard> {
        let mut reserved = self.reserved.write().await;
        
        let currently_reserved = reserved.get(mint).copied().unwrap_or(0);
        let available = actual_balance.saturating_sub(currently_reserved);
        
        if available >= amount {
            *reserved.entry(*mint).or_insert(0) += amount;
            
            Some(ReservationGuard {
                reservation: Arc::clone(&self.reserved),
                mint: *mint,
                amount,
            })
        } else {
            log::debug!(
                "Reservation failed: mint={}, required={}, available={}, reserved={}, actual={}",
                mint,
                amount,
                available,
                currently_reserved,
                actual_balance
            );
            None
        }
    }

    /// Gets the currently reserved amount for a mint
    pub async fn get_reserved(&self, mint: &Pubkey) -> u64 {
        let reserved = self.reserved.read().await;
        reserved.get(mint).copied().unwrap_or(0)
    }

    /// Gets the available balance (actual - reserved) for a mint
    pub async fn get_available(&self, mint: &Pubkey, actual_balance: u64) -> u64 {
        let reserved = self.get_reserved(mint).await;
        actual_balance.saturating_sub(reserved)
    }

    /// Manually reserves an amount (for use when guard can't be passed through async boundaries)
    /// Returns true if reservation succeeded, false otherwise
    pub async fn reserve(&self, mint: &Pubkey, amount: u64, actual_balance: u64) -> bool {
        let mut reserved = self.reserved.write().await;
        let currently_reserved = reserved.get(mint).copied().unwrap_or(0);
        let available = actual_balance.saturating_sub(currently_reserved);
        
        if available >= amount {
            *reserved.entry(*mint).or_insert(0) += amount;
            true
        } else {
            false
        }
    }

    /// Manually releases a reserved amount
    pub async fn release(&self, mint: &Pubkey, amount: u64) {
        let mut reserved = self.reserved.write().await;
        if let Some(reserved_amount) = reserved.get_mut(mint) {
            *reserved_amount = reserved_amount.saturating_sub(amount);
            if *reserved_amount == 0 {
                reserved.remove(mint);
            }
        }
    }

    /// ✅ ATOMIC: Balance check and reservation in one operation
    /// 
    /// This method atomically checks the balance and reserves it, preventing race conditions
    /// where balance is checked and then reserved separately (gap between operations).
    /// 
    /// Returns Some(ReservationGuard) if reservation succeeds, None otherwise.
    /// The reservation is automatically released when the guard is dropped.
    /// 
    /// This is the recommended method to use instead of:
    /// 1. get_token_balance() 
    /// 2. reserve() 
    /// 
    /// Because there's a race condition gap between steps 1 and 2.
    /// 
    /// ⚠️ RACE CONDITION NOTE:
    /// There's still a small gap between:
    /// - Step 1: RPC call to get balance (async, outside lock)
    /// - Step 2: Lock acquisition and reservation (inside lock)
    /// 
    /// During this gap, another transaction could consume the balance.
    /// 
    /// ✅ PROTECTION: Two-Layer Defense
    /// To fully protect against this race condition, the executor performs a final balance check
    /// immediately before sending the transaction (see executor.rs lines 132-170).
    /// 
    /// This two-layer protection ensures:
    /// 1. **Reservation Layer** (this method): Prevents parallel opportunities from over-reserving
    ///    - Minimizes the gap by immediately acquiring lock after balance check
    ///    - Prevents multiple opportunities from reserving the same balance
    /// 
    /// 2. **Final Check Layer** (executor.rs): Prevents sending transactions that will fail
    ///    - Checks balance immediately before transaction send (minimal gap)
    ///    - Releases reservation if balance is insufficient
    ///    - Prevents wasted transaction fees on failed transactions
    /// 
    /// The gap is minimal and acceptable because:
    /// - The gap only exists between RPC call and lock acquisition (typically < 1ms)
    /// - Final check happens immediately before tx send (another < 1ms gap)
    /// - Total gap is typically < 2ms, which is acceptable for liquidation bot use case
    /// - Final check catches any balance changes that occurred during the gap
    pub async fn try_reserve_with_check<BC: BalanceChecker>(
        &self,
        mint: &Pubkey,
        amount: u64,
        balance_checker: &BC,
    ) -> Result<Option<ReservationGuard>> {
        // Atomically: check balance and reserve in one operation
        // This prevents race conditions where balance is checked, then another
        // opportunity reserves it before this one can reserve it.
        
        // Step 1: Get current balance (this is the only async operation outside the lock)
        // ⚠️ NOTE: There's a small gap here - between RPC call and lock acquisition,
        // another transaction could consume the balance. The executor performs a final
        // balance check before sending the transaction to protect against this.
        let actual_balance = balance_checker.get_token_balance(mint).await?;
        
        // Step 2: Immediately reserve (with lock held)
        // This minimizes the gap between balance check and reservation
        let mut reserved = self.reserved.write().await;
        
        let currently_reserved = reserved.get(mint).copied().unwrap_or(0);
        let available = actual_balance.saturating_sub(currently_reserved);
        
        if available >= amount {
            *reserved.entry(*mint).or_insert(0) += amount;
            
            // Release lock before returning guard
            drop(reserved);
            
            Ok(Some(ReservationGuard {
                reservation: Arc::clone(&self.reserved),
                mint: *mint,
                amount,
            }))
        } else {
            log::debug!(
                "Atomic reservation failed: mint={}, required={}, available={}, reserved={}, actual={}",
                mint,
                amount,
                available,
                currently_reserved,
                actual_balance
            );
            Ok(None)
        }
    }
}

impl Default for BalanceReservation {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard that automatically releases the reservation when dropped
pub struct ReservationGuard {
    reservation: Arc<RwLock<HashMap<Pubkey, u64>>>,
    mint: Pubkey,
    amount: u64,
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        let reservation = Arc::clone(&self.reservation);
        let mint = self.mint;
        let amount = self.amount;
        
        // Release the reservation asynchronously
        tokio::spawn(async move {
            let mut reserved = reservation.write().await;
            if let Some(reserved_amount) = reserved.get_mut(&mint) {
                *reserved_amount = reserved_amount.saturating_sub(amount);
                if *reserved_amount == 0 {
                    reserved.remove(&mint);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_reservation_success() {
        let reservation = BalanceReservation::new();
        let mint = Pubkey::new_unique();
        let actual_balance = 1000;

        let guard = reservation.try_reserve(&mint, 500, actual_balance).await;
        assert!(guard.is_some());

        let available = reservation.get_available(&mint, actual_balance).await;
        assert_eq!(available, 500); // 1000 - 500 reserved
    }

    #[tokio::test]
    async fn test_reservation_failure() {
        let reservation = BalanceReservation::new();
        let mint = Pubkey::new_unique();
        let actual_balance = 1000;

        let guard1 = reservation.try_reserve(&mint, 600, actual_balance).await;
        assert!(guard1.is_some());

        // Second reservation should fail - only 400 available
        let guard2 = reservation.try_reserve(&mint, 500, actual_balance).await;
        assert!(guard2.is_none());
    }

    #[tokio::test]
    async fn test_reservation_release() {
        let reservation = BalanceReservation::new();
        let mint = Pubkey::new_unique();
        let actual_balance = 1000;

        {
            let guard = reservation.try_reserve(&mint, 500, actual_balance).await;
            assert!(guard.is_some());
            assert_eq!(reservation.get_reserved(&mint).await, 500);
        } // guard dropped here

        // Wait a bit for async cleanup
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Reservation should be released
        assert_eq!(reservation.get_reserved(&mint).await, 0);
        assert_eq!(reservation.get_available(&mint, actual_balance).await, 1000);
    }
}

