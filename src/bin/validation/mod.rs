// Validation framework - modüler ve yeniden kullanılabilir validation sistemi

pub mod builder;
pub mod result;
pub mod macros;

// Validation modülleri
pub mod config;
pub mod addresses;
pub mod rpc;

pub use builder::ValidationBuilder;
pub use result::TestResult;

