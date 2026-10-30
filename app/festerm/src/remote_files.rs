//! A live filesystem capability, never a connection recipe. The owning
//! session supplies the authenticated requestor and immutable origin binding.
use festerm_ssh::{LiveRemoteFileRequestor, RemoteFileReadError, SftpPath};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RemoteFileProvider {
    #[default]
    Sftp,
    DevTunnelPosix,
    DevTunnelWindows,
}

#[derive(Clone, Debug)]
pub(crate) enum RemoteFileCapability {
    Sftp(LiveRemoteFileRequestor),
    #[cfg(feature = "experimental-dev-tunnels")]
    DevTunnel(festerm_tunnel_bridge::files::FileRequestor),
    #[cfg(test)]
    Test(std::sync::Arc<TestFiles>),
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestFiles {
    pub provider: RemoteFileProvider,
    pub generation: u64,
    pub endpoint: String,
    pub connected: std::sync::atomic::AtomicBool,
    pub paths: std::sync::Mutex<Vec<String>>,
    pub bytes: Vec<u8>,
    pub gate: Option<std::sync::Arc<std::sync::Barrier>>,
}
impl From<LiveRemoteFileRequestor> for RemoteFileCapability {
    fn from(files: LiveRemoteFileRequestor) -> Self {
        Self::Sftp(files)
    }
}

pub(crate) enum ReadError {
    Sftp(RemoteFileReadError),
    Refused,
}
pub(crate) struct Snapshot {
    pub path: String,
    pub bytes: Vec<u8>,
}
impl Snapshot {
    pub fn into_parts(self) -> (String, Vec<u8>) {
        (self.path, self.bytes)
    }
}

impl RemoteFileCapability {
    pub fn binding_key(&self) -> String {
        match self {
            Self::Sftp(files) => format!(
                "sftp:{}:{}",
                files.session_id(),
                files.transport_generation()
            ),
            #[cfg(feature = "experimental-dev-tunnels")]
            Self::DevTunnel(files) => format!(
                "dev-tunnel:{}:{}",
                files.endpoint_identity, files.generation
            ),
            #[cfg(test)]
            Self::Test(files) => format!(
                "test:{:?}:{}:{}",
                files.provider, files.endpoint, files.generation
            ),
        }
    }
    pub fn provider(&self) -> RemoteFileProvider {
        match self {
            #[cfg(test)]
            Self::Test(files) => files.provider,
            Self::Sftp(_) => RemoteFileProvider::Sftp,
            #[cfg(feature = "experimental-dev-tunnels")]
            Self::DevTunnel(files) => match files.path_style {
                festerm_tunnel_bridge::files::PathStyle::Windows => {
                    RemoteFileProvider::DevTunnelWindows
                }
                festerm_tunnel_bridge::files::PathStyle::Posix => {
                    RemoteFileProvider::DevTunnelPosix
                }
            },
        }
    }
    pub fn transport_generation(&self) -> u64 {
        match self {
            #[cfg(test)]
            Self::Test(files) => files.generation,
            Self::Sftp(files) => files.transport_generation(),
            #[cfg(feature = "experimental-dev-tunnels")]
            Self::DevTunnel(files) => files.generation,
        }
    }
    pub fn verified_endpoint_identity(&self) -> Option<String> {
        match self {
            #[cfg(test)]
            Self::Test(files) => files
                .connected
                .load(std::sync::atomic::Ordering::Acquire)
                .then(|| files.endpoint.clone()),
            Self::Sftp(files) => files.verified_host_key_fingerprint(),
            #[cfg(feature = "experimental-dev-tunnels")]
            Self::DevTunnel(files) => files.available().then(|| files.endpoint_identity.clone()),
        }
    }
    pub fn read_remote_file_snapshot(
        &self,
        path: &str,
        maximum: usize,
    ) -> Result<Snapshot, ReadError> {
        match self {
            #[cfg(test)]
            Self::Test(files) => {
                files.paths.lock().unwrap().push(path.to_owned());
                if let Some(gate) = &files.gate {
                    gate.wait();
                }
                if files.bytes.len() > maximum
                    || !files.connected.load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(ReadError::Refused);
                }
                Ok(Snapshot {
                    path: path.into(),
                    bytes: files.bytes.clone(),
                })
            }
            Self::Sftp(files) => {
                let (metadata, bytes) = files
                    .read_remote_file_snapshot(path, maximum)
                    .map_err(ReadError::Sftp)?
                    .into_parts();
                let SftpPath::Remote(path) = metadata.path else {
                    return Err(ReadError::Refused);
                };
                Ok(Snapshot { path, bytes })
            }
            #[cfg(feature = "experimental-dev-tunnels")]
            Self::DevTunnel(files) => {
                let bytes = files.read(path, maximum).map_err(|_| ReadError::Refused)?;
                Ok(Snapshot {
                    path: path.replace('\\', "/"),
                    bytes,
                })
            }
        }
    }
}
