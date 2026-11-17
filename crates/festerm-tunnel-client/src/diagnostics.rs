//! Content-free HOST request diagnostics. Response bytes are classified and discarded;
//! none of the types here can retain provider text, identities, URLs, or credentials.
use crate::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Placement,
    Observation,
    Creation,
    HostToken,
}
impl Operation {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Placement => "Host placement",
            Self::Observation => "Host observation",
            Self::Creation => "Host creation",
            Self::HostToken => "Host token",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Category {
    Registration,
    Cancelled,
    Expired,
    Denied,
    Network,
    Response,
    AccountChanged,
    IdentityChanged,
    TunnelNotFound,
    Tunnel,
    Ambiguous,
    Store,
    Busy,
    RateLimited,
    ProviderRejected,
    UnsupportedUserAcl,
    UnsupportedProtocol,
}
impl Category {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Registration => "Sign-in application unavailable",
            Self::Cancelled => "Cancelled",
            Self::Expired => "Sign-in expired",
            Self::Denied => "Access denied",
            Self::Network => "Connection failed or outcome uncertain",
            Self::Response => "Unexpected service response",
            Self::AccountChanged => "Account changed",
            Self::IdentityChanged => "Identity or host generation changed",
            Self::TunnelNotFound => "Host resource not found",
            Self::Tunnel => "Host bridge unavailable",
            Self::Ambiguous => "Multiple host resources",
            Self::Store => "Secure storage unavailable",
            Self::Busy => "Operation already in progress",
            Self::RateLimited => "Service rate limit",
            Self::ProviderRejected => "Service rejected the request",
            Self::UnsupportedUserAcl => "Service does not support individual-user access entries",
            Self::UnsupportedProtocol => "Service does not support the requested port protocol",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostFailure {
    pub operation: Operation,
    pub http_status: Option<u16>,
    pub category: Category,
}
impl HostFailure {
    /// Preserve the caller's semantic decision, including uncertainty. HTTP
    /// metadata never changes whether creation was rejected or may have been sent.
    pub const fn from_original(operation: Operation, error: Error, status: Option<u16>) -> Self {
        let category = match error {
            Error::Registration => Category::Registration,
            Error::Cancelled => Category::Cancelled,
            Error::Expired => Category::Expired,
            Error::Denied => Category::Denied,
            Error::Network => Category::Network,
            Error::Response => Category::Response,
            Error::AccountChanged => Category::AccountChanged,
            Error::IdentityChanged => Category::IdentityChanged,
            Error::TunnelNotFound => Category::TunnelNotFound,
            Error::Tunnel => Category::Tunnel,
            Error::Ambiguous => Category::Ambiguous,
            Error::Store => Category::Store,
            Error::Busy => Category::Busy,
            Error::RateLimited => Category::RateLimited,
            Error::HostRequest(previous) => {
                return Self {
                    operation,
                    http_status: match status {
                        Some(value) => Some(value),
                        None => previous.http_status,
                    },
                    category: previous.category,
                };
            }
        };
        Self {
            operation,
            http_status: status,
            category,
        }
    }

    /// Classifies an HTTP rejection without retaining any response content.
    /// Use `from_original` when the transport has already decided uncertainty.
    pub fn from_http(operation: Operation, status: u16, body: &[u8]) -> Self {
        let known = |needle: &[u8]| {
            body.len() <= crate::RESPONSE_LIMIT
                && body
                    .windows(needle.len())
                    .any(|part| part.eq_ignore_ascii_case(needle))
        };
        let category = match status {
            400 if known(
                b"Adding individual users to an access control entry is currently disabled.",
            ) =>
            {
                Category::UnsupportedUserAcl
            }
            400 if known(b"Tunnel port protocol is not supported")
                || known(b"Tunnel protocol is not supported") =>
            {
                Category::UnsupportedProtocol
            }
            401 | 403 => Category::Denied,
            404 => Category::TunnelNotFound,
            429 => Category::RateLimited,
            408 | 425 | 500..=599 => Category::Network,
            400..=499 => Category::ProviderRejected,
            _ => Category::Response,
        };
        Self {
            operation,
            http_status: Some(status),
            category,
        }
    }

    pub const fn original_error(self) -> Error {
        match self.category {
            Category::Registration => Error::Registration,
            Category::Cancelled => Error::Cancelled,
            Category::Expired => Error::Expired,
            Category::Denied => Error::Denied,
            Category::Network => Error::Network,
            Category::Response
            | Category::ProviderRejected
            | Category::UnsupportedUserAcl
            | Category::UnsupportedProtocol => Error::Response,
            Category::AccountChanged => Error::AccountChanged,
            Category::IdentityChanged => Error::IdentityChanged,
            Category::TunnelNotFound => Error::TunnelNotFound,
            Category::Tunnel => Error::Tunnel,
            Category::Ambiguous => Error::Ambiguous,
            Category::Store => Error::Store,
            Category::Busy => Error::Busy,
            Category::RateLimited => Error::RateLimited,
        }
    }
}
impl fmt::Display for HostFailure {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.category.label())
    }
}
impl std::error::Error for HostFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_retains_no_provider_content() {
        let body = b"ghp_SECRET https://private.example/account Adding individual users to an access control entry is currently disabled.";
        let failure = HostFailure::from_http(Operation::Creation, 400, body);
        assert_eq!(failure.category, Category::UnsupportedUserAcl);
        let text = format!("{failure:?} {failure}");
        for secret in ["SECRET", "private.example", "account"] {
            assert!(!text.contains(secret));
        }
        assert_eq!(failure.original_error(), Error::Response);
        assert_eq!(
            HostFailure::from_http(
                Operation::Creation,
                400,
                &vec![b'x'; crate::RESPONSE_LIMIT + 1]
            )
            .category,
            Category::ProviderRejected
        );
        assert_eq!(
            HostFailure::from_http(Operation::Creation, 400, b"unknown provider detail").category,
            Category::ProviderRejected
        );
        assert_eq!(
            HostFailure::from_http(
                Operation::Creation,
                400,
                b"Tunnel port protocol is not supported"
            )
            .category,
            Category::UnsupportedProtocol
        );
    }
    #[test]
    fn metadata_preserves_all_original_semantics_and_uncertainty() {
        for original in [
            Error::Registration,
            Error::Cancelled,
            Error::Expired,
            Error::Denied,
            Error::Network,
            Error::Response,
            Error::AccountChanged,
            Error::IdentityChanged,
            Error::TunnelNotFound,
            Error::Tunnel,
            Error::Ambiguous,
            Error::Store,
            Error::Busy,
            Error::RateLimited,
        ] {
            let failure = HostFailure::from_original(Operation::Creation, original, Some(429));
            let wrapped = Error::HostRequest(failure);
            assert_eq!(wrapped.original_error(), original);
            assert_eq!(wrapped.to_string(), original.to_string());
        }
        let old = HostFailure::from_http(
            Operation::Creation,
            400,
            b"Adding individual users to an access control entry is currently disabled.",
        );
        let again =
            HostFailure::from_original(Operation::Observation, Error::HostRequest(old), None);
        assert_eq!(again.category, old.category);
        assert_eq!(again.http_status, Some(400));
        assert_eq!(again.operation, Operation::Observation);
    }
}
