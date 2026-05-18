pub mod address;
pub mod service;

// Re-export key public API items for convenience.
pub use service::{aws_error_response, fallback_handler, AwsService, ServiceRegistry};
