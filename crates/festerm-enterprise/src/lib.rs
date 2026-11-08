#![forbid(unsafe_code)]

//! Enterprise identity and Dev Center discovery backend for fesTerm.
//!
//! This crate intentionally provides a pre-GUI, desktop-first foundation:
//! browser-based Entra sign-in with PKCE over a loopback callback, transient
//! in-memory access tokens, and bounded read-only Dev Center discovery.
//!
//! The public operations are synchronous: run them on a blocking worker, not
//! inside an async executor or a GUI frame callback. Each HTTP client owns its
//! current-thread runtime and uses cancellable async DNS and HTTP internally.

mod auth;
mod devcenter;
mod http;

pub use auth::{
    begin_authorization, AccessToken, AuthConfiguration, AuthError, AuthErrorKind,
    AuthorizationSession, EntraTenantId, OperationControl, OperationControlError, PublicClientId,
};
pub use devcenter::{
    AbilitySet, DevBox, DevBoxName, DevCenterClient, DevCenterError, DevCenterErrorKind,
    DevCenterUri, Project, ProjectAbilities, ProjectName, RemoteConnection, SensitiveUri,
    API_VERSION,
};
