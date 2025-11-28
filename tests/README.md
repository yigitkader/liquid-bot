# Integration Test Suite

This directory contains integration tests that validate the bot against real mainnet data.

## Running Tests

All integration tests are **ignored by default** and require environment variables to run.

### Run All Tests

```bash
cargo test --test integration_mainnet -- --ignored
```

### Run Specific Test

```bash
cargo test --test integration_mainnet test_real_obligation_parsing -- --ignored --nocapture
```

## Test List

### 1. `test_real_obligation_parsing`

Validates that we can correctly parse a real Solend obligation from mainnet.

**Required env vars:**
- `TEST_OBLIGATION_ADDRESS`: Real obligation address
- `RPC_HTTP_URL`: (optional, defaults to mainnet)

**Example:**
```bash
export TEST_OBLIGATION_ADDRESS=<obligation_pubkey>
cargo test --test integration_mainnet test_real_obligation_parsing -- --ignored --nocapture
```

**How to find an obligation:**
- Visit https://solend.fi/dashboard and find an obligation
- Or use: `cargo run --bin find_my_obligation`

### 2. `test_real_reserve_parsing`

Validates that we can correctly parse a real Solend reserve from mainnet.

**Required env vars:**
- `TEST_RESERVE_ADDRESS`: Real reserve address (e.g., USDC reserve)
- `RPC_HTTP_URL`: (optional, defaults to mainnet)

**Example:**
```bash
export TEST_RESERVE_ADDRESS=BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw
cargo test --test integration_mainnet test_real_reserve_parsing -- --ignored --nocapture
```

**Known reserve addresses:**
- USDC: `BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw`
- SOL: `8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36`

### 3. `test_real_oracle_reading`

Validates that we can correctly read oracle prices from mainnet.

**Required env vars (one of):**
- `TEST_ORACLE_ADDRESS`: Real Pyth oracle address
- `TEST_RESERVE_ADDRESS`: Real reserve address (will extract oracle from reserve)
- `RPC_HTTP_URL`: (optional, defaults to mainnet)

**Example:**
```bash
# Option 1: Direct oracle address
export TEST_ORACLE_ADDRESS=<pyth_oracle_pubkey>
cargo test --test integration_mainnet test_real_oracle_reading -- --ignored --nocapture

# Option 2: Get oracle from reserve
export TEST_RESERVE_ADDRESS=BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw
cargo test --test integration_mainnet test_real_oracle_reading -- --ignored --nocapture
```

### 4. `test_profit_calculation_against_known_opportunity`

Validates profit calculation using a real liquidation opportunity from mainnet.

**Required env vars:**
- `TEST_OBLIGATION_ADDRESS`: Real obligation address (must be liquidatable)
- `RPC_HTTP_URL`: (optional, defaults to mainnet)

**Optional env vars:**
- `TEST_EXPECTED_PROFIT_USD`: Expected profit in USD (for validation)

**Example:**
```bash
export TEST_OBLIGATION_ADDRESS=<obligation_pubkey>
export TEST_EXPECTED_PROFIT_USD=50.0  # Optional
cargo test --test integration_mainnet test_profit_calculation_against_known_opportunity -- --ignored --nocapture
```

**Note:** The obligation must have a health factor < 1.0 to be liquidatable.

### 5. `test_instruction_building_against_known_tx`

Validates that we can build liquidation instructions that match real mainnet transactions.

**Required env vars:**
- `TEST_LIQUIDATION_TX`: Real liquidation transaction signature
- `RPC_HTTP_URL`: (optional, defaults to mainnet)

**Example:**
```bash
export TEST_LIQUIDATION_TX=<transaction_signature>
cargo test --test integration_mainnet test_instruction_building_against_known_tx -- --ignored --nocapture
```

**Note:** For detailed instruction validation, use:
```bash
cargo run --bin validate_instruction_accounts -- --tx <transaction_signature>
```

## Finding Test Data

### Finding Obligations

1. **Solend Dashboard:**
   - Visit https://solend.fi/dashboard
   - Find an obligation with health factor < 1.0
   - Copy the obligation address

2. **Using find_my_obligation binary:**
   ```bash
   cargo run --bin find_my_obligation -- --wallet <wallet_path>
   ```

### Finding Reserves

Known mainnet reserve addresses:
- **USDC Reserve:** `BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw`
- **SOL Reserve:** `8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36`

### Finding Liquidation Transactions

1. **Solana Explorer:**
   - Visit https://explorer.solana.com/
   - Search for Solend program: `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo`
   - Filter for liquidation transactions
   - Copy transaction signature

2. **Solend Protocol:**
   - Monitor Solend's liquidation events
   - Extract transaction signatures from on-chain events

## Troubleshooting

### RPC Rate Limiting

If you encounter rate limiting errors:
- Use a premium RPC endpoint (Helius, Triton, etc.)
- Set `RPC_HTTP_URL` to your premium endpoint
- Add delays between tests

### Missing Environment Variables

All tests will skip gracefully if required environment variables are not set. Check the test output for specific requirements.

### Test Failures

If a test fails:
1. Check that the test data (obligation, reserve, etc.) still exists on mainnet
2. Verify RPC endpoint is accessible
3. Check that the data format hasn't changed (Solend protocol updates)
4. Review test output for specific error messages

## Continuous Integration

These tests are designed to be run manually or in CI with proper environment variables set. They are not included in the default test suite to avoid:
- Requiring mainnet access for all developers
- Rate limiting issues with free RPC endpoints
- Flakiness due to mainnet data changes

For CI/CD, consider:
- Running tests only on mainnet deployments
- Using premium RPC endpoints
- Caching test data when possible
- Running tests in parallel with rate limiting

