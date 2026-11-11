//! A client-bound checkpoint, never a portable profile or standalone authority.
use super::*;
use serde::Deserialize;
use zeroize::Zeroizing;

const DOMAIN: &str = "fesTerm HOST client original creation checkpoint v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    domain: String,
    host: HostId,
    account: String,
    endpoint: String,
    operation: IssuedOperation,
    request: Request,
}

/// Opaque capture of an actually issued creation ticket and its exact body.
/// Only a protected native-store adapter may persist these bytes.
pub struct OriginalCheckpoint(Zeroizing<Vec<u8>>);

impl OriginalCheckpoint {
    pub fn copy_for_protected_store(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(self.0.to_vec())
    }
}

impl IssuedControlOperation {
    pub fn protected_checkpoint(&self, request: &Request) -> Result<OriginalCheckpoint, Error> {
        if !matches!(request, Request::Create { .. } | Request::Ensure { .. }) {
            return Err(ContractError::Denied.into());
        }
        request.validate()?;
        let record = Record {
            domain: DOMAIN.into(),
            host: self.binding.host.clone(),
            account: self.binding.account.clone(),
            endpoint: self.binding.endpoint.clone(),
            operation: self.operation.clone(),
            request: request.clone(),
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| ContractError::Malformed)?;
        if bytes.len() > CONTROL_FRAME_BYTES {
            return Err(ContractError::Limit.into());
        }
        Ok(OriginalCheckpoint(Zeroizing::new(bytes)))
    }
}

/// Implement only at the protected native-store composition boundary. The
/// adapter must authenticate its local anchor, current owner/key/namespace and
/// purpose before giving the factory a native-vault payload. Ordinary imported
/// or caller-deserialized metadata is not an implementation of this contract.
pub trait ProtectedOriginalCheckpoint {
    type Error: From<Error>;

    fn reopen(
        &self,
        factory: ProtectedOriginalReopener<'_>,
    ) -> Result<IssuedControlOperation, Self::Error>;
}

/// Issued only by a freshly handshaken control client, not a raw epoch/binding
/// constructor. Decoding still checks domain, destination and the entire body.
pub struct ProtectedOriginalReopener<'a> {
    binding: &'a Binding,
    request: &'a Request,
}

impl ProtectedOriginalReopener<'_> {
    pub fn from_native_payload(self, bytes: &[u8]) -> Result<IssuedControlOperation, Error> {
        if bytes.len() > CONTROL_FRAME_BYTES {
            return Err(ContractError::Limit.into());
        }
        let record: Record = serde_json::from_slice(bytes).map_err(|_| ContractError::Malformed)?;
        let binding = Binding::new(record.host, record.account, record.endpoint)?;
        record.request.validate()?;
        if record.domain != DOMAIN
            || binding != *self.binding
            || !matches!(
                record.request,
                Request::Create { .. } | Request::Ensure { .. }
            )
            || serde_json::to_vec(&record.request).map_err(|_| ContractError::Malformed)?
                != serde_json::to_vec(self.request).map_err(|_| ContractError::Malformed)?
        {
            return Err(ContractError::Stale.into());
        }
        Ok(IssuedControlOperation {
            binding,
            operation: record.operation,
        })
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> ControlClient<S> {
    pub fn reopen_protected_original<P: ProtectedOriginalCheckpoint>(
        &self,
        request: &Request,
        protected: &P,
    ) -> Result<IssuedControlOperation, P::Error> {
        if !self.usable || self.capabilities.is_none() {
            return Err(Error::from(ContractError::Denied).into());
        }
        protected.reopen(ProtectedOriginalReopener {
            binding: &self.binding,
            request,
        })
    }
}
