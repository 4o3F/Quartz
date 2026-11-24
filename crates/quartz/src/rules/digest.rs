//! Domain-separated, length-prefixed digests for volatile preview confirmations.

use quartz_domain::rules::registry::RuleSnapshot;
use quartz_protocol::{ErrorKind, ProtocolError};
use sha2::{Digest, Sha256};

pub(super) enum DigestDomain {
    Mutation,
    Revisions,
}
pub(crate) struct ReceiptDigest(Sha256);
impl ReceiptDigest {
    pub(super) fn new(domain: DigestDomain) -> Self {
        let mut digest = Sha256::new();
        digest.update(match domain {
            DigestDomain::Mutation => b"quartz.receipt.mutation.v1\0".as_slice(),
            DigestDomain::Revisions => b"quartz.receipt.revisions.v1\0".as_slice(),
        });
        Self(digest)
    }
    pub(super) fn text(&mut self, text: &str) -> Result<(), ProtocolError> {
        let length = u64::try_from(text.len()).map_err(|_| {
            tracing::error!("receipt digest length exceeds supported range");
            ProtocolError::new(ErrorKind::Internal)
        })?;
        self.0.update(length.to_le_bytes());
        self.0.update(text.as_bytes());
        Ok(())
    }
    pub(super) fn revision(&mut self, value: u32) {
        self.0.update(value.to_le_bytes());
    }
    pub(super) fn finish(self) -> String {
        quartz_domain::text::encode_hex(&self.0.finalize())
    }
    pub(crate) fn revisions(snapshot: &RuleSnapshot) -> Result<String, ProtocolError> {
        let mut digest = Self::new(DigestDomain::Revisions);
        for (key, entry) in &snapshot.state().sets {
            digest.text(key.as_str())?;
            digest.revision(entry.current.version);
        }
        Ok(digest.finish())
    }
}
