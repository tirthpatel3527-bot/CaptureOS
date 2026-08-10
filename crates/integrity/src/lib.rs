//! Future non-destructive integrity verification contract.

use media_model::{FileInstanceId, MediaFingerprint};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationRequest {
    pub source: FileInstanceId,
    pub expected: MediaFingerprint,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationResult {
    pub matched: bool,
    pub detail: String,
}
#[derive(Debug, Error)]
pub enum VerificationError {
    #[error("verification unavailable: {0}")]
    Unavailable(String),
}
pub trait IntegrityVerifier: Send + Sync {
    fn verify(
        &self,
        request: &VerificationRequest,
    ) -> Result<VerificationResult, VerificationError>;
}
