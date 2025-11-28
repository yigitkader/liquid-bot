# Balance Reservation System

## Overview

The balance reservation system prevents race conditions when multiple liquidation opportunities are processed in parallel. It ensures that if two opportunities require the same token balance, only one can be approved at a time.

## Architecture

### Two-Layer Protection

1. **Reservation Layer** (in `strategist.rs`):
   - Atomically checks balance and reserves it
   - Prevents parallel opportunities from over-reserving
   - Uses `try_reserve_with_check()` for atomic operation

2. **Final Check Layer** (in `executor.rs`):
   - Double-checks balance immediately before sending transaction
   - Prevents sending transactions that will fail due to insufficient balance
   - Handles the gap between reservation and transaction send

## Race Condition Protection

### Problem

There's a potential race condition gap between:
1. Balance check (RPC call - async)
2. Reservation (lock acquisition)

During this gap, another transaction could consume the balance.

### Solution

**Layer 1: Atomic Reservation**
```rust
// In strategist.rs
balance_reservation.try_reserve_with_check(
    &debt_mint,
    required_debt_amount,
    wallet_balance_checker.as_ref(),
).await
```

This minimizes the gap by:
- Getting balance (async RPC call)
- Immediately acquiring lock and reserving
- Lock is held for minimal time

**Layer 2: Final Check**
```rust
// In executor.rs - before sending transaction
let current_balance = wallet.get_token_balance(&debt_mint).await?;
let reserved = balance_reservation.get_reserved(&debt_mint).await;
let available = current_balance.saturating_sub(reserved);

if available < opportunity.max_liquidatable_amount {
    // Retry or fail
}
```

This ensures:
- Balance is checked immediately before transaction send
- If insufficient, transaction is not sent (saves fees)
- Retry mechanism handles temporary balance issues

## Flow Diagram

```
┌─────────────────┐
│  Strategist     │
│  (Opportunity)  │
└────────┬────────┘
         │
         │ 1. try_reserve_with_check()
         │    - RPC: get balance
         │    - Lock: reserve amount
         │
         ▼
┌─────────────────┐
│  Reservation    │
│  (Atomic)       │
└────────┬────────┘
         │
         │ 2. ReservationGuard created
         │    (auto-released on drop)
         │
         ▼
┌─────────────────┐
│  Executor       │
│  (Transaction)  │
└────────┬────────┘
         │
         │ 3. Final balance check
         │    - RPC: get current balance
         │    - Check: available >= required
         │
         ▼
┌─────────────────┐
│  Transaction    │
│  Send           │
└─────────────────┘
```

## API Reference

### `try_reserve_with_check()`

Atomically checks balance and reserves it.

```rust
pub async fn try_reserve_with_check<BC: BalanceChecker>(
    &self,
    mint: &Pubkey,
    amount: u64,
    balance_checker: &BC,
) -> Result<Option<ReservationGuard>>
```

**Returns:**
- `Ok(Some(guard))` - Reservation successful
- `Ok(None)` - Insufficient balance
- `Err` - RPC error

**Note:** The guard automatically releases the reservation when dropped.

### `get_reserved()`

Gets the currently reserved amount for a mint.

```rust
pub async fn get_reserved(&self, mint: &Pubkey) -> u64
```

### `get_available()`

Gets the available balance (actual - reserved) for a mint.

```rust
pub async fn get_available(&self, mint: &Pubkey, actual_balance: u64) -> u64
```

## Best Practices

1. **Always use `try_reserve_with_check()`** instead of separate balance check + reserve
2. **Keep reservation guards in scope** until transaction is sent
3. **Release reservation after transaction** (automatic via guard drop)
4. **Final check in executor** prevents sending failing transactions

## Error Handling

### Insufficient Balance

If balance check fails in executor:
- Logs warning with details
- Retries with exponential backoff
- Fails after max retries

### RPC Errors

If RPC call fails:
- Logs warning
- Continues with transaction send (transaction will fail if balance insufficient)
- Handles error in transaction result

## Testing

The reservation system is tested with:
- `test_reservation_success()` - Successful reservation
- `test_reservation_failure()` - Insufficient balance
- `test_reservation_release()` - Automatic cleanup

## Performance Considerations

- **Lock Duration**: Minimal (only during reservation update)
- **RPC Calls**: Two per opportunity (strategist + executor)
- **Memory**: O(n) where n = number of unique mints

## Future Improvements

1. **Reservation Timeout**: Auto-release stale reservations
2. **Reservation Priority**: Queue-based reservation for fairness
3. **Balance Prediction**: Estimate balance changes from pending transactions

