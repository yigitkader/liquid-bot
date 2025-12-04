// Utility macros for common patterns

/// Add context to an error result
/// 
/// Usage:
/// ```rust
/// let result = some_operation().await;
/// bail_context!(result, "Failed to do something");
/// ```
#[macro_export]
macro_rules! bail_context {
    ($result:expr, $msg:expr) => {
        $result.context($msg)?
    };
    ($result:expr, $fmt:expr, $($arg:tt)*) => {
        $result.with_context(|| format!($fmt, $($arg)*))?
    };
}

/// Parse a pubkey with context
/// 
/// Usage:
/// ```rust
/// let pubkey = parse_pubkey_context!("Invalid pubkey", pubkey_str)?;
/// ```
#[macro_export]
macro_rules! parse_pubkey_context {
    ($str:expr, $msg:expr) => {
        $crate::utils::helpers::parse_pubkey($str)
            .context($msg)
    };
    ($str:expr, $fmt:expr, $($arg:tt)*) => {
        $crate::utils::helpers::parse_pubkey($str)
            .with_context(|| format!($fmt, $($arg)*))
    };
}

/// Retry an operation with default RPC config
/// 
/// Usage:
/// ```rust
/// let result = retry_rpc!(|| async { rpc.get_account(&pubkey).await }).await?;
/// ```
#[macro_export]
macro_rules! retry_rpc {
    ($op:expr) => {
        $crate::utils::error_helpers::retry_with_backoff(
            $op,
            $crate::utils::error_helpers::RetryConfig::for_rpc(),
        )
    };
}

/// Retry an operation with rate limit config
/// 
/// Usage:
/// ```rust
/// let result = retry_rate_limit!(|| async { api.call().await }).await?;
/// ```
#[macro_export]
macro_rules! retry_rate_limit {
    ($op:expr) => {
        $crate::utils::error_helpers::retry_with_backoff(
            $op,
            $crate::utils::error_helpers::RetryConfig::for_rate_limit(),
        )
    };
}

/// Retry a transaction with default transaction config
/// 
/// Usage:
/// ```rust
/// let result = retry_tx!(|| async { send_transaction().await }).await?;
/// ```
#[macro_export]
macro_rules! retry_tx {
    ($op:expr) => {
        $crate::utils::error_helpers::retry_with_backoff(
            $op,
            $crate::utils::error_helpers::RetryConfig::for_transaction(),
        )
    };
}

/// Log and return error with context
/// 
/// Usage:
/// ```rust
/// log_and_bail!("Failed to process", error);
/// ```
#[macro_export]
macro_rules! log_and_bail {
    ($msg:expr, $err:expr) => {
        {
            log::error!("{}: {}", $msg, $err);
            return Err($err).context($msg);
        }
    };
    ($fmt:expr, $err:expr, $($arg:tt)*) => {
        {
            let msg = format!($fmt, $($arg)*);
            log::error!("{}: {}", msg, $err);
            return Err($err).context(msg);
        }
    };
}

/// Try operation and return early with context on error
/// 
/// Usage:
/// ```rust
/// let value = try_with_context!(operation(), "Operation failed")?;
/// ```
#[macro_export]
macro_rules! try_with_context {
    ($result:expr, $msg:expr) => {
        match $result {
            Ok(v) => v,
            Err(e) => return Err(e).context($msg),
        }
    };
    ($result:expr, $fmt:expr, $($arg:tt)*) => {
        match $result {
            Ok(v) => v,
            Err(e) => return Err(e).with_context(|| format!($fmt, $($arg)*)),
        }
    };
}

