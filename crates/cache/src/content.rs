//! Содержимое писем формата 6 (ADR 0006 п. 2): письмо целиком по SHA-256 исходных байтов.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// SHA-256 байтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}
