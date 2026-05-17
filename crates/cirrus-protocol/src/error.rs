// Cirrus protocol AWS S3 error types.
// Implements AWS S3 error responses as per https://docs.aws.amazon.com/AmazonS3/latest/API/ErrorResponses.html

use std::fmt;

/// An error that occurs during S3 protocol operations.
#[derive(Debug)]
pub struct AwsError {
    /// The specific error kind.
    pub kind: AwsErrorKind,
    /// The request ID associated with the error.
    pub request_id: Option<String>,
    /// The host ID associated with the error.
    pub host_id: Option<String>,
}

impl AwsError {
    /// Creates a new AwsError with the given kind.
    pub const fn new(kind: AwsErrorKind) -> Self {
        Self {
            kind,
            request_id: None,
            host_id: None,
        }
    }

    /// Sets the request ID for this error.
    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Sets the host ID for this error.
    pub fn host_id(mut self, host_id: impl Into<String>) -> Self {
        self.host_id = Some(host_id.into());
        self
    }

    /// Returns the AWS error code string.
    pub fn error_code(&self) -> &'static str {
        self.kind.error_code()
    }

    /// Returns the HTTP status code for this error.
    pub fn status_code(&self) -> u16 {
        self.kind.status_code()
    }

    /// Returns the human-readable error message.
    pub fn message(&self) -> String {
        self.kind.message()
    }

    /// Serializes the error to an AWS S3-compatible XML response.
    ///
    /// This function hand-builds the XML string for performance, avoiding
    /// serialization overhead for small error responses.
    pub fn to_xml(&self) -> String {
        // Escape XML special characters in the message and code.
        let code = escape_xml(self.error_code());
        let message = escape_xml(&self.message());
        let request_id = self
            .request_id
            .as_ref()
            .map(|id| format!("<RequestId>{}</RequestId>", escape_xml(id)))
            .unwrap_or_default();
        let host_id = self
            .host_id
            .as_ref()
            .map(|id| format!("<HostId>{}</HostId>", escape_xml(id)))
            .unwrap_or_default();

        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <Error>\n\
             <Code>{code}</Code>\n\
             <Message>{message}</Message>\n\
             {request_id}\
             {host_id}\
             </Error>",
            code = code,
            message = message,
            request_id = request_id,
            host_id = host_id
        )
    }
}

/// The specific kind of AWS S3 error.
#[derive(Debug)]
pub enum AwsErrorKind {
    /// The server does not support the operation requested.
    NotImplemented,
    /// The specified bucket does not exist.
    NoSuchBucket {
        /// The name of the bucket that does not exist.
        bucket_name: String,
    },
    /// The specified key does not exist.
    NoSuchKey {
        /// The name of the bucket containing the key.
        bucket_name: String,
        /// The key that does not exist.
        key: String,
    },
    /// The specified upload does not exist.
    NoSuchUpload {
        /// The ID of the upload that does not exist.
        upload_id: String,
    },
    /// The part number is not in the valid range (1-10000).
    InvalidPartNumber {
        /// The invalid part number that was provided.
        part_number: u32,
    },
    /// The upload ID is missing or invalid.
    InvalidUploadId {
        /// The invalid or missing upload ID.
        upload_id: String,
    },
    /// The request is missing a required body.
    MissingRequestBody,
    /// The request is missing a required header.
    MissingRequestHeader {
        /// The name of the missing header.
        header_name: String,
    },
    /// The specified version does not exist.
    NoSuchVersion {
        /// The ID of the version that does not exist.
        version_id: String,
    },
    /// The bucket already exists and you do not own it.
    BucketAlreadyExists {
        /// The name of the bucket that already exists.
        bucket_name: String,
    },
    /// The bucket already exists and is owned by you.
    BucketAlreadyOwnedByYou {
        /// The name of the bucket that already exists and is owned by you.
        bucket_name: String,
    },
    /// The operation is not valid for the current state of the bucket.
    BucketNotEmpty {
        /// The name of the bucket that is not empty.
        bucket_name: String,
    },
    /// The provided credentials are invalid or expired.
    InvalidAccessKeyId,
    /// The signature does not match the request.
    SignatureDoesNotMatch,
    /// The request is missing required authentication information.
    MissingAuthentication,
    /// The request uses an unsupported HTTP method.
    MethodNotAllowed {
        /// The HTTP method that is not allowed.
        method: String,
    },
    /// The entity is too large.
    EntityTooLarge {
        /// The entity that is too large.
        entity: String,
    },
    /// The request uses an unsupported storage class.
    InvalidStorageClass {
        /// The unsupported storage class.
        storage_class: String,
    },
    /// An internal server error occurred.
    InternalError {
        /// Optional details about the internal error.
        details: Option<String>,
    },
    /// An error occurred during XML serialization.
    XmlSerializationError {
        /// Details about the serialization error.
        details: String,
    },
}

impl AwsErrorKind {
    /// Returns the AWS error code string for this error kind.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::NotImplemented => "NotImplemented",
            Self::NoSuchBucket { .. } => "NoSuchBucket",
            Self::NoSuchKey { .. } => "NoSuchKey",
            Self::NoSuchUpload { .. } => "NoSuchUpload",
            Self::InvalidPartNumber { .. } => "InvalidPartNumber",
            Self::InvalidUploadId { .. } => "InvalidUploadId",
            Self::MissingRequestBody => "MissingRequestBody",
            Self::MissingRequestHeader { .. } => "MissingRequestHeader",
            Self::NoSuchVersion { .. } => "NoSuchVersion",
            Self::BucketAlreadyExists { .. } => "BucketAlreadyExists",
            Self::BucketAlreadyOwnedByYou { .. } => "BucketAlreadyOwnedByYou",
            Self::BucketNotEmpty { .. } => "BucketNotEmpty",
            Self::InvalidAccessKeyId => "InvalidAccessKeyId",
            Self::SignatureDoesNotMatch => "SignatureDoesNotMatch",
            Self::MissingAuthentication => "MissingAuthentication",
            Self::MethodNotAllowed { .. } => "MethodNotAllowed",
            Self::EntityTooLarge { .. } => "EntityTooLarge",
            Self::InvalidStorageClass { .. } => "InvalidStorageClass",
            Self::InternalError { .. } => "InternalError",
            Self::XmlSerializationError { .. } => "XmlSerializationError",
        }
    }

    /// Returns the HTTP status code for this error kind.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::NotImplemented => 501,
            Self::NoSuchBucket { .. } => 404,
            Self::NoSuchKey { .. } => 404,
            Self::NoSuchUpload { .. } => 404,
            Self::InvalidPartNumber { .. } => 400,
            Self::InvalidUploadId { .. } => 400,
            Self::MissingRequestBody => 400,
            Self::MissingRequestHeader { .. } => 400,
            Self::NoSuchVersion { .. } => 404,
            Self::BucketAlreadyExists { .. } => 409,
            Self::BucketAlreadyOwnedByYou { .. } => 409,
            Self::BucketNotEmpty { .. } => 409,
            Self::InvalidAccessKeyId => 403,
            Self::SignatureDoesNotMatch => 403,
            Self::MissingAuthentication => 403,
            Self::MethodNotAllowed { .. } => 405,
            Self::EntityTooLarge { .. } => 400,
            Self::InvalidStorageClass { .. } => 400,
            Self::InternalError { .. } => 500,
            Self::XmlSerializationError { .. } => 500,
        }
    }

    /// Returns the human-readable error message for this error kind.
    pub fn message(&self) -> String {
        match self {
            Self::NotImplemented => "The server does not support the operation requested.".to_string(),
            Self::NoSuchBucket { bucket_name } => {
                format!("The specified bucket {} does not exist", bucket_name)
            }
            Self::NoSuchKey {
                bucket_name,
                key,
            } => format!("The specified key {} does not exist in bucket {}", key, bucket_name),
            Self::NoSuchUpload { upload_id } => {
                format!("The specified upload {} does not exist", upload_id)
            }
            Self::InvalidPartNumber { part_number } => {
                format!(
                    "PartNumber must be between 1 and 10000, got {}",
                    part_number
                )
            }
            Self::InvalidUploadId { upload_id } => {
                format!("The upload ID {} is invalid or missing", upload_id)
            }
            Self::MissingRequestBody => "Request body is missing".to_string(),
            Self::MissingRequestHeader { header_name } => {
                format!("Request is missing required header {}", header_name)
            }
            Self::NoSuchVersion { version_id } => {
                format!("The specified version {} does not exist", version_id)
            }
            Self::BucketAlreadyExists { bucket_name } => {
                format!("The requested bucket {} already exists", bucket_name)
            }
            Self::BucketAlreadyOwnedByYou { bucket_name } => {
                format!("The bucket {} you attempted to create already exists, and you own it.", bucket_name)
            }
            Self::BucketNotEmpty { bucket_name } => {
                format!("The bucket {} is not empty", bucket_name)
            }
            Self::InvalidAccessKeyId => {
                "The AWS Access Key ID you provided does not exist in our records.".to_string()
            }
            Self::SignatureDoesNotMatch => {
                "The request signature we calculated does not match the signature you provided. Check your key and signing method.".to_string()
            }
            Self::MissingAuthentication => {
                "Authentication information is missing from the request.".to_string()
            }
            Self::MethodNotAllowed { method } => {
                format!("The method {} is not allowed for this resource.", method)
            }
            Self::EntityTooLarge { entity } => {
                format!("The {} you have provided is too large for the resource.", entity)
            }
            Self::InvalidStorageClass { storage_class } => {
                format!("The storage class {} is not a valid storage class", storage_class)
            }
            Self::InternalError { details } => {
                if let Some(details) = details {
                    format!("We encountered an internal error. Please try again. Details: {}", details)
                } else {
                    "We encountered an internal error. Please try again.".to_string()
                }
            }
            Self::XmlSerializationError { details } => {
                format!("XML serialization failed: {}", details)
            }
        }
    }
}

impl From<AwsErrorKind> for AwsError {
    fn from(kind: AwsErrorKind) -> Self {
        Self::new(kind)
    }
}

impl fmt::Display for AwsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} (Request ID: {:?}, Host ID: {:?})",
            self.error_code(),
            self.message(),
            self.request_id,
            self.host_id
        )
    }
}

impl std::error::Error for AwsError {}

/// Escape XML special characters (&, <, >, ", ') in the given string.
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_kind_no_such_bucket() {
        let error = AwsError::from(AwsErrorKind::NoSuchBucket {
            bucket_name: "my-bucket".to_string(),
        })
        .request_id("test-request-id")
        .host_id("test-host-id");

        assert_eq!(error.error_code(), "NoSuchBucket");
        assert_eq!(error.status_code(), 404);
        assert_eq!(
            error.message(),
            "The specified bucket my-bucket does not exist"
        );
        let xml = error.to_xml();
        assert!(xml.contains("<Code>NoSuchBucket</Code>"));
        assert!(xml.contains(
            "<Message>The specified bucket my-bucket does not exist</Message>"
        ));
        assert!(xml.contains("<RequestId>test-request-id</RequestId>"));
        assert!(xml.contains("<HostId>test-host-id</HostId>"));
    }

    #[test]
    fn test_error_kind_no_such_key() {
        let error = AwsError::from(AwsErrorKind::NoSuchKey {
            bucket_name: "my-bucket".to_string(),
            key: "my-key".to_string(),
        });

        assert_eq!(error.error_code(), "NoSuchKey");
        assert_eq!(error.status_code(), 404);
        assert_eq!(
            error.message(),
            "The specified key my-key does not exist in bucket my-bucket"
        );
    }

    #[test]
    fn test_error_kind_invalid_part_number() {
        let error = AwsError::from(AwsErrorKind::InvalidPartNumber { part_number: 15000 });

        assert_eq!(error.error_code(), "InvalidPartNumber");
        assert_eq!(error.status_code(), 400);
        assert_eq!(
            error.message(),
            "PartNumber must be between 1 and 10000, got 15000"
        );
    }

    #[test]
    fn test_error_kind_internal_error() {
        let error = AwsError::from(AwsErrorKind::InternalError {
            details: Some("test details".to_string()),
        });

        assert_eq!(error.error_code(), "InternalError");
        assert_eq!(error.status_code(), 500);
        assert_eq!(
            error.message(),
            "We encountered an internal error. Please try again. Details: test details"
        );
    }

    #[test]
    fn test_error_kind_xml_serialization_error() {
        let error = AwsError::from(AwsErrorKind::XmlSerializationError {
            details: "failed to serialize".to_string(),
        });

        assert_eq!(error.error_code(), "XmlSerializationError");
        assert_eq!(error.status_code(), 500);
        assert_eq!(error.message(), "XML serialization failed: failed to serialize");
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(escape_xml("test & < > \" '"), "test &amp; &lt; &gt; &quot; &apos;");
        assert_eq!(escape_xml("normal text"), "normal text");
    }

    #[test]
    fn test_to_xml_special_characters() {
        let error = AwsError::from(AwsErrorKind::NoSuchBucket {
            bucket_name: "test & < > \" ' bucket".to_string(),
        });

        let xml = error.to_xml();
        // Check that special characters are escaped
        assert!(xml.contains("test &amp; &lt; &gt; &quot; &apos; bucket"));
        // Check that the structure is correct
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<Code>NoSuchBucket</Code>"));
        assert!(xml.contains("<Message>The specified bucket test &amp; &lt; &gt; &quot; &apos; bucket does not exist</Message>"));
    }
}