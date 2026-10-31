//! Portable frame I/O and read-only bridge discovery shared by the desktop
//! persistent-session client and the mobile client.
use std::{io, time::Duration};

use festerm_session::TerminalSize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::*;

pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// Read-only bridge discovery. The relay endpoint key is verified before this
/// operation; it never opens daemon IPC or reserves an attachment.
pub async fn discover(
    endpoint: &tunnels::contracts::TunnelEndpoint,
    port: u16,
    token: &str,
) -> io::Result<Description> {
    let mut connection = crate::relay::connect_endpoint(endpoint, port, token).await?;
    let result = discover_stream(&mut connection.stream).await;
    let closed = connection.close().await;
    let description = result?;
    closed?;
    Ok(description)
}

pub(crate) async fn discover_stream(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> io::Result<Description> {
    write_frame(stream, DISCOVER, &VERSION.to_be_bytes()).await?;
    let (kind, bytes) = tokio::time::timeout(DEADLINE, read_frame(stream))
        .await
        .map_err(|_| invalid())??;
    if kind != DESCRIPTION {
        return Err(invalid());
    }
    let description: Description = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if description.version != VERSION
        || description.target.pid == 0
        || description.target.name.is_empty()
        || description.target.name.len() > 128
        || description.target.name.chars().any(char::is_control)
        || description.target.generation.parse::<u128>().is_err()
    {
        return Err(invalid());
    }
    Ok(description)
}

pub async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<(u8, Vec<u8>)> {
    read_frame_inner(reader, false).await
}

#[cfg(feature = "relay")]
pub(crate) async fn read_client_frame(
    reader: &mut (impl AsyncRead + Unpin),
) -> io::Result<(u8, Vec<u8>)> {
    read_frame_inner(reader, true).await
}

pub(crate) async fn read_frame_inner(
    reader: &mut (impl AsyncRead + Unpin),
    from_client: bool,
) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0; 5];
    reader.read_exact(&mut header).await?;
    let length = u32::from_be_bytes(header[1..].try_into().map_err(|_| invalid())?) as usize;
    validate_length(header[0], length)?;
    if from_client
        && !matches!(
            header[0],
            HELLO | DISCOVER | ADOPTED | INPUT | RESIZE | DETACH | SYNC | FILE_READ
        )
    {
        return Err(invalid());
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok((header[0], bytes))
}

pub async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    kind: u8,
    bytes: &[u8],
) -> io::Result<()> {
    validate_length(kind, bytes.len())?;
    tokio::time::timeout(DEADLINE, async {
        writer.write_all(&[kind]).await?;
        writer
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .await?;
        writer.write_all(bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| invalid())?
}

pub(crate) fn decode_size(bytes: &[u8]) -> io::Result<TerminalSize> {
    if bytes.len() != 8 {
        return Err(invalid());
    }
    let values: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_be_bytes([p[0], p[1]]))
        .collect();
    if values[0] > 256 || values[1] > 128 {
        return Err(invalid());
    }
    TerminalSize::with_pixels(values[0], values[1], values[2], values[3]).map_err(|_| invalid())
}

pub(crate) fn encode_size(size: TerminalSize) -> Vec<u8> {
    [
        size.columns(),
        size.rows(),
        size.pixel_width().unwrap_or(0),
        size.pixel_height().unwrap_or(0),
    ]
    .into_iter()
    .flat_map(u16::to_be_bytes)
    .collect()
}
