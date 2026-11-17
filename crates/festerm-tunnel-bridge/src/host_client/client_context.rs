use super::*;
use serde::{Deserialize, Serialize};

/// Distinct local credential families. Neither variant is a server grant.
pub enum ClientContext {
    EnrolledOwner(Box<ApprovedContext>),
    Ordinary(ordinary::OrdinaryContext),
}
impl From<ApprovedContext> for ClientContext {
    fn from(value: ApprovedContext) -> Self {
        Self::EnrolledOwner(Box::new(value))
    }
}
impl From<ordinary::OrdinaryContext> for ClientContext {
    fn from(value: ordinary::OrdinaryContext) -> Self {
        Self::Ordinary(value)
    }
}

#[derive(Clone, Copy)]
pub enum ClientContextRef<'a> {
    EnrolledOwner(&'a ApprovedContext),
    Ordinary(&'a ordinary::OrdinaryContext),
}
impl<'a> From<&'a ApprovedContext> for ClientContextRef<'a> {
    fn from(value: &'a ApprovedContext) -> Self {
        Self::EnrolledOwner(value)
    }
}
impl<'a> From<&'a ordinary::OrdinaryContext> for ClientContextRef<'a> {
    fn from(value: &'a ordinary::OrdinaryContext) -> Self {
        Self::Ordinary(value)
    }
}
impl<'a> From<&'a ClientContext> for ClientContextRef<'a> {
    fn from(value: &'a ClientContext) -> Self {
        match value {
            ClientContext::EnrolledOwner(c) => Self::EnrolledOwner(c),
            ClientContext::Ordinary(c) => Self::Ordinary(c),
        }
    }
}

/// Legacy serialization remains byte-compatible; ordinary records have a
/// different explicit purpose and cannot deserialize as an OwnerBinding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum Issuer {
    EnrolledOwner(OwnerBinding),
    Ordinary(OrdinaryIssuer),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrdinaryIssuer {
    pub ordinary_purpose: String,
    pub account: String,
    pub installation: festerm_host_protocol::HostId,
    pub principal: festerm_host_protocol::PrincipalId,
    pub route: crate::ordinary_auth::OrdinaryRoute,
}
impl Issuer {
    pub(super) fn host(&self) -> &festerm_host_protocol::HostId {
        match self {
            Self::EnrolledOwner(v) => &v.host.host,
            Self::Ordinary(v) => &v.route.host.host,
        }
    }
    pub(super) fn account(&self) -> &str {
        match self {
            Self::EnrolledOwner(v) => &v.account,
            Self::Ordinary(v) => &v.account,
        }
    }
}
impl<'a> ClientContextRef<'a> {
    pub fn binding(&self) -> &'a Binding {
        match self {
            Self::EnrolledOwner(v) => v.binding(),
            Self::Ordinary(v) => v.binding(),
        }
    }
    pub fn as_enrolled_owner(&self) -> Option<&'a ApprovedContext> {
        match self {
            Self::EnrolledOwner(v) => Some(v),
            Self::Ordinary(_) => None,
        }
    }
    pub(super) fn issuer(&self) -> Issuer {
        match self {
            Self::EnrolledOwner(v) => Issuer::EnrolledOwner(v.approved.clone()),
            Self::Ordinary(v) => Issuer::Ordinary(OrdinaryIssuer {
                ordinary_purpose: ordinary::ORIGINAL_PURPOSE.into(),
                account: v.binding().account().into(),
                installation: v.host_info().installation.clone(),
                principal: v.host_info().principal.clone(),
                route: v.host_info().route.clone(),
            }),
        }
    }
    pub(super) fn reference(&self) -> SecretReference {
        match self {
            Self::EnrolledOwner(v) => v.reference.duplicate_for_transport(),
            Self::Ordinary(v) => v.reference(),
        }
    }
}
impl ClientContext {
    pub fn binding(&self) -> &Binding {
        match self {
            Self::EnrolledOwner(v) => v.binding(),
            Self::Ordinary(v) => v.binding(),
        }
    }
    pub fn port(&self) -> u16 {
        match self {
            Self::EnrolledOwner(v) => v.port,
            Self::Ordinary(v) => v.port(),
        }
    }
    pub fn as_enrolled_owner(&self) -> Option<&ApprovedContext> {
        match self {
            Self::EnrolledOwner(v) => Some(v),
            Self::Ordinary(_) => None,
        }
    }
    pub(super) fn ordinary_guard(&self) -> Option<ordinary::OrdinaryAccountGuard> {
        match self {
            Self::EnrolledOwner(_) => None,
            Self::Ordinary(v) => Some(v.account_guard()),
        }
    }
    pub(super) async fn connect(
        self,
        permit: ConnectionPermit,
        role: StreamRole,
        cancel: &CancellationToken,
    ) -> Result<(relay::ClientConnection, ConnectionPermit, Binding), Failure> {
        match self {
            Self::EnrolledOwner(v) => (*v).connect(permit, role, cancel).await,
            Self::Ordinary(v) => v.connect(permit, role, cancel).await,
        }
    }
}
