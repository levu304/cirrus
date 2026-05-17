// Cirrus protocol error types.

use std::fmt;

/// An error that occurs during S3 protocol operations.
#[derive(Debug, Clone)]
pub struct S3Error {
    /// The error code as defined by S3.
    pub code: String,
    /// A human-readable error message.
    pub message: String,
}

impl S3Error {
    /// Creates an InvalidPartNumber error.
    pub fn invalid_part_number(part_number: u32) -> Self {
        S3Error {
            code: "InvalidPartNumber".to_string(),
            message: format!(
                "PartNumber must be between 1 and 10000, got {}",
                part_number
            ),
        }
    }
}

impl fmt::Display for S3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for S3Error {}