//! Canonical document identity (ADR 0034 §1).
//!
//! Two views name the same document when their *origins* are equal, so the
//! registry can answer "is this file already open?" without reading it. A
//! local origin is compared by canonical path; a remote origin is compared by
//! the verified SFTP identity that produced it, never by a display string, so
//! reconnecting to a different host can never silently alias one document onto
//! another.

use std::fmt;
use std::path::{Path, PathBuf};

/// An opaque handle to a document held by the registry.
///
/// Handles are monotonic and never reused, so a view holding a stale handle
/// after its document was released fails to resolve instead of quietly
/// addressing a different file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct DocumentId(u64);

impl DocumentId {
    /// Creates a handle. Only the registry should call this.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "document {}", self.0)
    }
}

/// Where a document's bytes come from.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum DocumentOrigin {
    Local(LocalOrigin),
    Remote(RemoteOrigin),
    Untitled(UntitledOrigin),
}

impl DocumentOrigin {
    /// The comparison key two views must share to be views of one document.
    pub fn key(&self) -> DocumentKey {
        DocumentKey(match self {
            Self::Local(local) => format!("local:{}", local.path().display()),
            Self::Remote(remote) => format!(
                "sftp:{}@{}:{}:{}:{}",
                remote.owner().key(),
                remote.host(),
                remote.port(),
                remote.verified_host_key_fingerprint(),
                remote.path()
            ),
            Self::Untitled(untitled) => format!("untitled:{}", untitled.key()),
        })
    }

    /// The bare file name shown on a tab chip.
    pub fn file_name(&self) -> &str {
        match self {
            Self::Local(local) => local.file_name(),
            Self::Remote(remote) => remote.file_name(),
            Self::Untitled(untitled) => untitled.file_name(),
        }
    }

    /// The directory containing this document, used to disambiguate two open
    /// documents whose file names collide (ADR 0034 §1).
    pub fn parent_label(&self) -> Option<String> {
        match self {
            Self::Local(local) => local
                .path()
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            Self::Remote(remote) => {
                let path = remote.path();
                let trimmed = path.trim_end_matches('/');
                let parent = trimmed.rsplit_once('/').map(|(head, _)| head)?;
                let name = parent.rsplit('/').find(|part| !part.is_empty())?;
                Some(name.to_owned())
            }
            Self::Untitled(_) => None,
        }
    }

    /// The host a colliding file name is disambiguated by, when the documents
    /// come from different origins.
    pub fn host_label(&self) -> Option<&str> {
        match self {
            Self::Local(_) => None,
            Self::Remote(remote) => Some(remote.host()),
            Self::Untitled(_) => None,
        }
    }

    /// The fully qualified origin shown in the path bar and in the dirty-close
    /// prompt, which must never be ambiguous about *which* file is at risk
    /// (ADR 0034 §7).
    pub fn qualified_label(&self) -> String {
        match self {
            Self::Local(local) => local.path().display().to_string(),
            Self::Remote(remote) => format!(
                "{}@{} · {}",
                remote.owner().display(),
                remote.host(),
                remote.path()
            ),
            Self::Untitled(untitled) => untitled.qualified_label().to_owned(),
        }
    }

    pub const fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }
}

impl From<LocalOrigin> for DocumentOrigin {
    fn from(origin: LocalOrigin) -> Self {
        Self::Local(origin)
    }
}

impl From<RemoteOrigin> for DocumentOrigin {
    fn from(origin: RemoteOrigin) -> Self {
        Self::Remote(origin)
    }
}

impl From<UntitledOrigin> for DocumentOrigin {
    fn from(origin: UntitledOrigin) -> Self {
        Self::Untitled(origin)
    }
}

/// The opaque equality key derived from an origin.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct DocumentKey(String);

impl fmt::Display for DocumentKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A validated local file identity.
///
/// The caller supplies the canonical path; this type does no I/O so it stays
/// usable in tests and on a worker thread.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct LocalOrigin {
    path: PathBuf,
    file_name: String,
}

impl LocalOrigin {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, OriginError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(OriginError::EmptyPath);
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or(OriginError::NoFileName)?
            .to_owned();
        Ok(Self { path, file_name })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

/// A validated SFTP file identity, pinned to one verified origin.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct RemoteOrigin {
    host: String,
    port: u16,
    owner: RemoteOwner,
    verified_host_key_fingerprint: String,
    path: String,
    file_name: String,
    lifecycle_generation: u64,
}

/// An application-created document that has bytes and editor state but no
/// backing path yet.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct UntitledOrigin {
    key: String,
    file_name: String,
    qualified_label: String,
}

impl UntitledOrigin {
    pub fn new(
        key: impl Into<String>,
        file_name: impl Into<String>,
        qualified_label: impl Into<String>,
    ) -> Result<Self, OriginError> {
        Ok(Self {
            key: non_empty(key, OriginError::EmptyPath)?,
            file_name: non_empty(file_name, OriginError::NoFileName)?,
            qualified_label: non_empty(qualified_label, OriginError::EmptyLabel)?,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn qualified_label(&self) -> &str {
        &self.qualified_label
    }
}

impl RemoteOrigin {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        owner: RemoteOwner,
        verified_host_key_fingerprint: impl Into<String>,
        path: impl Into<String>,
        lifecycle_generation: u64,
    ) -> Result<Self, OriginError> {
        let host = host.into().trim().to_ascii_lowercase();
        if host.is_empty() {
            return Err(OriginError::EmptyHost);
        }
        if host.chars().any(char::is_whitespace) {
            return Err(OriginError::WhitespaceHost);
        }
        if port == 0 {
            return Err(OriginError::ZeroPort);
        }
        let verified_host_key_fingerprint = verified_host_key_fingerprint.into().trim().to_owned();
        if verified_host_key_fingerprint.is_empty() {
            return Err(OriginError::EmptyFingerprint);
        }
        let path = path.into().trim().to_owned();
        if path.is_empty() {
            return Err(OriginError::EmptyPath);
        }
        let file_name = path
            .trim_end_matches('/')
            .rsplit('/')
            .find(|part| !part.is_empty())
            .ok_or(OriginError::NoFileName)?
            .to_owned();
        Ok(Self {
            host,
            port,
            owner,
            verified_host_key_fingerprint,
            path,
            file_name,
            lifecycle_generation,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub const fn owner(&self) -> &RemoteOwner {
        &self.owner
    }

    pub fn verified_host_key_fingerprint(&self) -> &str {
        &self.verified_host_key_fingerprint
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub const fn lifecycle_generation(&self) -> u64 {
        self.lifecycle_generation
    }

    /// Returns this origin retargeted at a sibling path on the same verified
    /// host, which is what Save As to a remote destination produces.
    pub fn with_path(&self, path: impl Into<String>) -> Result<Self, OriginError> {
        Self::new(
            self.host.clone(),
            self.port,
            self.owner.clone(),
            self.verified_host_key_fingerprint.clone(),
            path,
            self.lifecycle_generation,
        )
    }
}

/// Who a remote document is read and written as.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum RemoteOwner {
    Username(String),
    ProfileIdentifier(String),
    UsernameAndProfile {
        username: String,
        profile_identifier: String,
    },
}

impl RemoteOwner {
    pub fn username(username: impl Into<String>) -> Result<Self, OriginError> {
        Ok(Self::Username(non_empty(
            username,
            OriginError::EmptyOwner,
        )?))
    }

    pub fn profile_identifier(identifier: impl Into<String>) -> Result<Self, OriginError> {
        Ok(Self::ProfileIdentifier(non_empty(
            identifier,
            OriginError::EmptyOwner,
        )?))
    }

    pub fn username_and_profile(
        username: impl Into<String>,
        profile_identifier: impl Into<String>,
    ) -> Result<Self, OriginError> {
        Ok(Self::UsernameAndProfile {
            username: non_empty(username, OriginError::EmptyOwner)?,
            profile_identifier: non_empty(profile_identifier, OriginError::EmptyOwner)?,
        })
    }

    /// The identity component of a document key. Both halves are included so
    /// one username under two profiles cannot collapse into one document.
    fn key(&self) -> String {
        match self {
            Self::Username(username) => format!("u:{username}"),
            Self::ProfileIdentifier(profile) => format!("p:{profile}"),
            Self::UsernameAndProfile {
                username,
                profile_identifier,
            } => format!("u:{username}/p:{profile_identifier}"),
        }
    }

    /// The user-facing name in a path bar, which is the username when there is
    /// one and the profile otherwise.
    pub fn display(&self) -> &str {
        match self {
            Self::Username(username) => username,
            Self::ProfileIdentifier(profile) => profile,
            Self::UsernameAndProfile { username, .. } => username,
        }
    }
}

fn non_empty(value: impl Into<String>, error: OriginError) -> Result<String, OriginError> {
    let value = value.into().trim().to_owned();
    if value.is_empty() {
        Err(error)
    } else {
        Ok(value)
    }
}

/// Why an origin could not be built.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OriginError {
    EmptyPath,
    NoFileName,
    EmptyLabel,
    EmptyHost,
    WhitespaceHost,
    ZeroPort,
    EmptyFingerprint,
    EmptyOwner,
}

impl fmt::Display for OriginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyPath => "the path is empty",
            Self::NoFileName => "the path does not name a file",
            Self::EmptyLabel => "the label is empty",
            Self::EmptyHost => "the host is empty",
            Self::WhitespaceHost => "the host contains whitespace",
            Self::ZeroPort => "the port is zero",
            Self::EmptyFingerprint => "the verified host key fingerprint is empty",
            Self::EmptyOwner => "the remote owner is empty",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for OriginError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(path: &str) -> RemoteOrigin {
        RemoteOrigin::new(
            "Web-1.Staging.Example.com",
            22,
            RemoteOwner::username("devuser").unwrap(),
            "SHA256:abc",
            path,
            7,
        )
        .unwrap()
    }

    #[test]
    fn two_local_origins_with_the_same_path_are_one_document() {
        let first = LocalOrigin::new("/home/fes/NOTES.md").unwrap();
        let second = LocalOrigin::new("/home/fes/NOTES.md").unwrap();
        assert_eq!(
            DocumentOrigin::from(first).key(),
            DocumentOrigin::from(second).key()
        );
    }

    #[test]
    fn a_local_and_a_remote_path_that_read_alike_are_different_documents() {
        let local = DocumentOrigin::from(LocalOrigin::new("/srv/NOTES.md").unwrap());
        let remote = DocumentOrigin::from(remote("/srv/NOTES.md"));
        assert_ne!(local.key(), remote.key());
    }

    #[test]
    fn a_remote_document_is_pinned_to_its_verified_host_key() {
        let trusted = DocumentOrigin::from(remote("/srv/NOTES.md"));
        let impostor = DocumentOrigin::from(
            RemoteOrigin::new(
                "web-1.staging.example.com",
                22,
                RemoteOwner::username("devuser").unwrap(),
                "SHA256:different",
                "/srv/NOTES.md",
                7,
            )
            .unwrap(),
        );
        assert_ne!(trusted.key(), impostor.key());
    }

    #[test]
    fn one_username_under_two_profiles_is_two_documents() {
        let first = RemoteOwner::username_and_profile("devuser", "staging").unwrap();
        let second = RemoteOwner::username_and_profile("devuser", "production").unwrap();
        assert_ne!(first.key(), second.key());
    }

    #[test]
    fn a_host_is_compared_case_insensitively() {
        let upper = remote("/srv/NOTES.md");
        let lower = RemoteOrigin::new(
            "web-1.staging.example.com",
            22,
            RemoteOwner::username("devuser").unwrap(),
            "SHA256:abc",
            "/srv/NOTES.md",
            7,
        )
        .unwrap();
        assert_eq!(
            DocumentOrigin::from(upper).key(),
            DocumentOrigin::from(lower).key()
        );
    }

    #[test]
    fn file_names_and_parents_come_from_both_origin_kinds() {
        let local = DocumentOrigin::from(LocalOrigin::new("/home/fes/projects/NOTES.md").unwrap());
        assert_eq!(local.file_name(), "NOTES.md");
        assert_eq!(local.parent_label().as_deref(), Some("projects"));
        assert_eq!(local.host_label(), None);

        let remote = DocumentOrigin::from(remote("/home/devuser/nimbus-relay/NOTES.md"));
        assert_eq!(remote.file_name(), "NOTES.md");
        assert_eq!(remote.parent_label().as_deref(), Some("nimbus-relay"));
        assert_eq!(remote.host_label(), Some("web-1.staging.example.com"));

        let untitled = DocumentOrigin::from(
            UntitledOrigin::new(
                "terminal-history-1",
                "terminal-history-1.txt",
                "Terminal history snapshot",
            )
            .unwrap(),
        );
        assert_eq!(untitled.file_name(), "terminal-history-1.txt");
        assert_eq!(untitled.parent_label(), None);
        assert_eq!(untitled.host_label(), None);
    }

    #[test]
    fn a_qualified_label_names_the_host_and_the_path() {
        let origin = DocumentOrigin::from(remote("/home/devuser/NOTES.md"));
        assert_eq!(
            origin.qualified_label(),
            "devuser@web-1.staging.example.com · /home/devuser/NOTES.md"
        );
    }

    #[test]
    fn untitled_documents_never_alias_one_another() {
        let first = DocumentOrigin::from(
            UntitledOrigin::new(
                "terminal-history-1",
                "terminal-history-1.txt",
                "Terminal history snapshot",
            )
            .unwrap(),
        );
        let second = DocumentOrigin::from(
            UntitledOrigin::new(
                "terminal-history-2",
                "terminal-history-2.txt",
                "Terminal history snapshot",
            )
            .unwrap(),
        );
        assert_ne!(first.key(), second.key());
    }

    #[test]
    fn empty_and_malformed_identities_are_refused() {
        assert_eq!(LocalOrigin::new(""), Err(OriginError::EmptyPath));
        assert_eq!(LocalOrigin::new("/"), Err(OriginError::NoFileName));
        assert_eq!(
            UntitledOrigin::new("snapshot", "terminal-history.txt", "   "),
            Err(OriginError::EmptyLabel)
        );
        assert_eq!(
            RemoteOrigin::new(
                "",
                22,
                RemoteOwner::username("devuser").unwrap(),
                "SHA256:abc",
                "/a",
                0
            ),
            Err(OriginError::EmptyHost)
        );
        assert_eq!(
            RemoteOrigin::new(
                "host",
                0,
                RemoteOwner::username("devuser").unwrap(),
                "SHA256:abc",
                "/a",
                0
            ),
            Err(OriginError::ZeroPort)
        );
        assert_eq!(
            RemoteOrigin::new(
                "host",
                22,
                RemoteOwner::username("devuser").unwrap(),
                "   ",
                "/a",
                0
            ),
            Err(OriginError::EmptyFingerprint)
        );
        assert_eq!(RemoteOwner::username("  "), Err(OriginError::EmptyOwner));
    }

    #[test]
    fn save_as_to_a_sibling_path_keeps_the_verified_origin() {
        let origin = remote("/srv/NOTES.md");
        let sibling = origin.with_path("/srv/OTHER.md").unwrap();
        assert_eq!(sibling.host(), origin.host());
        assert_eq!(
            sibling.verified_host_key_fingerprint(),
            origin.verified_host_key_fingerprint()
        );
        assert_eq!(sibling.file_name(), "OTHER.md");
        assert_ne!(
            DocumentOrigin::from(origin).key(),
            DocumentOrigin::from(sibling).key()
        );
    }

    #[test]
    fn handles_are_distinct_and_printable() {
        let first = DocumentId::from_raw(1);
        let second = DocumentId::from_raw(2);
        assert_ne!(first, second);
        assert_eq!(first.raw(), 1);
        assert_eq!(first.to_string(), "document 1");
    }
}
