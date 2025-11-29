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

    /// ✅ ATOMIC: Balance check and reservation in one operation with double-check pattern
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
    /// ✅ DOUBLE-CHECK PATTERN: Try-with-verify
    /// This method uses a double-check pattern to minimize race conditions:
    /// 1. **First Check** (outside lock): Get initial balance via RPC
    /// 2. **Lock Acquisition**: Acquire write lock on reservation map
    /// 3. **Second Check** (inside lock): Re-verify balance hasn't changed
    ///    - If balance changed, log warning and use fresh balance
    /// 4. **Reservation**: Reserve amount if sufficient balance available
    /// 
    /// This pattern ensures:
    /// - Balance is verified immediately before reservation (minimal gap)
    /// - Any balance changes during lock acquisition are detected
    /// - Reservation uses the most up-to-date balance information
    /// 
    /// ✅ ADDITIONAL PROTECTION: Final Check Layer (executor.rs)
    /// The executor performs a final balance check immediately before sending the transaction
    /// (see executor.rs lines 132-170). This provides a third layer of protection:
    /// - Catches any balance changes that occurred after reservation
    /// - Prevents sending transactions that will fail (saves fees)
    /// - Releases reservation if balance is insufficient
    /// 
    /// This three-layer protection ensures maximum safety:
    /// 1. **Reservation Layer** (this method): Prevents parallel opportunities from over-reserving
    /// 2. **Double-Check Layer** (this method): Verifies balance immediately before reservation
    /// 3. **Final Check Layer** (executor.rs): Prevents sending transactions that will fail
    pub async fn try_reserve_with_check<BC: BalanceChecker>(
        &self,
        mint: &Pubkey,
        amount: u64,
        balance_checker: &BC,
    ) -> Result<Option<ReservationGuard>> {
        // Step 1: Get initial balance (outside lock)
        // This is the first check to quickly determine if reservation might succeed
        let initial_balance = balance_checker.get_token_balance(mint).await?;
        
        // Step 2: Acquire lock and perform double-check
        // Lock is held during the second check to ensure atomicity
        let mut reserved = self.reserved.write().await;
        
        // Step 3: ✅ DOUBLE CHECK: Re-verify balance inside lock
        // This catches any balance changes that occurred between Step 1 and lock acquisition
        let fresh_balance = balance_checker.get_token_balance(mint).await?;
        
        if fresh_balance != initial_balance {
            log::warn!(
                "Balance changed during reservation: mint={}, initial={}, fresh={}, delta={}",
                mint,
                initial_balance,
                fresh_balance,
                fresh_balance as i64 - initial_balance as i64
            );
        }
        
        // Use fresh balance for reservation calculation
        let currently_reserved = reserved.get(mint).copied().unwrap_or(0);
        let available = fresh_balance.saturating_sub(currently_reserved);
        
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
                "Atomic reservation failed: mint={}, required={}, available={}, reserved={}, fresh_balance={}, initial_balance={}",
                mint,
                amount,
                available,
                currently_reserved,
                fresh_balance,
                initial_balance
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

