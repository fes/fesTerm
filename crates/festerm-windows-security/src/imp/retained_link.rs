use super::{file_identity, file_information, open_relative, qualified_data_volume};
use std::{
    fs::File,
    io,
    mem::size_of,
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::{
    Wdk::{
        Storage::FileSystem::{FileFsDeviceInformation, NtQueryVolumeInformationFile, FILE_OPEN},
        System::SystemServices::{FILE_FS_DEVICE_INFORMATION, FILE_REMOTE_DEVICE},
    },
    Win32::{
        Foundation::{RtlNtStatusToDosError, HANDLE, STATUS_SUCCESS},
        Storage::FileSystem::{
            FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_DEVICE_DISK, FILE_EXECUTE,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        },
        System::{
            Ioctl::FSCTL_GET_REPARSE_POINT,
            SystemServices::{IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK},
            IO::{DeviceIoControl, IO_STATUS_BLOCK},
        },
    },
};

const REPARSE_BYTES: usize = 16 * 1024;
const TARGET_BYTES: usize = 4096;

#[cfg(test)]
mod tests;

/// Parsed substitute path from the captured NTFS reparse buffer. It confers no
/// authority to open the target. An absolute path needs an admitted root and
/// a relative path must resolve from the retained parent.
#[derive(PartialEq, Eq)]
pub enum WindowsLinkTarget {
    ParentRelative(PathBuf),
    AbsoluteDos(PathBuf),
}

/// Retains the no-follow name and the exact reparse object used for a snapshot.
/// Write/delete sharing is denied on the link, but attribute-only I/O can
/// still change reparse data. Revalidation refuses changes it observes.
pub struct RetainedDataLink {
    parent: File,
    parent_identity: (u32, u64),
    name: PathBuf,
    link: File,
    payload: Box<[u8]>,
    target: WindowsLinkTarget,
}

impl RetainedDataLink {
    /// `parent` must come from a current, no-follow directory chain held
    /// without delete sharing. The caller retains and fences that chain and
    /// its configured root. Only the parent and one leaf are examined here.
    pub fn open(parent: &File, name: &Path) -> io::Result<Self> {
        if !name
            .as_os_str()
            .to_str()
            .is_some_and(|name| component(name, false))
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let parent = parent.try_clone()?;
        let parent_identity = directory_identity(&parent)?;
        qualified_data_volume(&parent)?;
        let link = open_relative(
            &parent,
            name,
            FILE_READ_ATTRIBUTES | FILE_EXECUTE,
            FILE_SHARE_READ,
            FILE_OPEN,
            0,
            ptr::null(),
        )?;
        if file_identity(&link)?.0 != parent_identity.0 {
            return Err(io::ErrorKind::CrossesDevices.into());
        }
        qualify_local_disk(&link)?;
        let payload = read_payload(&link)?;
        let target = parse_payload(&payload)?;
        let snapshot = Self {
            parent,
            parent_identity,
            name: name.into(),
            link,
            payload,
            target,
        };
        snapshot.revalidate()?;
        Ok(snapshot)
    }

    pub fn target(&self) -> &WindowsLinkTarget {
        &self.target
    }

    /// Compare the original payload on the pinned object and on the current
    /// no-follow name in its held parent. Success gives an observed snapshot
    /// match; a concurrent edit can occur between these checks or afterward.
    pub fn revalidate(&self) -> io::Result<()> {
        if directory_identity(&self.parent)? != self.parent_identity
            || read_payload(&self.link)? != self.payload
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let named = open_relative(
            &self.parent,
            &self.name,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            0,
            ptr::null(),
        )?;
        if file_identity(&named)? != file_identity(&self.link)?
            || read_payload(&named)? != self.payload
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
}

fn directory_identity(file: &File) -> io::Result<(u32, u64)> {
    let info = file_information(file)?;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
        != FILE_ATTRIBUTE_DIRECTORY
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok((
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

fn qualify_local_disk(file: &File) -> io::Result<()> {
    let mut result = FILE_FS_DEVICE_INFORMATION::default();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: open_relative returns a synchronous handle, and both result
    // buffers are live and sized for the requested information class.
    let code = unsafe {
        NtQueryVolumeInformationFile(
            file.as_raw_handle() as HANDLE,
            &raw mut status,
            (&raw mut result).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    if code != STATUS_SUCCESS {
        // SAFETY: translates the status value returned by the call above.
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(code) } as i32,
        ));
    }
    if status.Information != size_of::<FILE_FS_DEVICE_INFORMATION>()
        || result.DeviceType != FILE_DEVICE_DISK
        || result.Characteristics & FILE_REMOTE_DEVICE != 0
    {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(())
}

fn read_payload(link: &File) -> io::Result<Box<[u8]>> {
    let handle = link.as_raw_handle() as HANDLE;
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: the handle is live and the buffer is sized for FileAttributeTagInfo.
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            (&raw mut tag).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let supported = match tag.ReparseTag {
        IO_REPARSE_TAG_SYMLINK => true,
        IO_REPARSE_TAG_MOUNT_POINT => tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
        _ => false,
    };
    if !supported || tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut bytes = vec![0u8; REPARSE_BYTES];
    let mut returned = 0;
    // SAFETY: both handles are synchronous no-follow opens. The buffer and
    // returned byte count live across the call and no overlapped I/O is used.
    if unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_REPARSE_POINT,
            ptr::null(),
            0,
            bytes.as_mut_ptr().cast(),
            REPARSE_BYTES as u32,
            &raw mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let returned = returned as usize;
    if !(8..=REPARSE_BYTES).contains(&returned) || bytes[..4] != tag.ReparseTag.to_le_bytes() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    bytes.truncate(returned);
    Ok(bytes.into_boxed_slice())
}

fn parse_payload(bytes: &[u8]) -> io::Result<WindowsLinkTarget> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    if !(8..=REPARSE_BYTES).contains(&bytes.len()) {
        return Err(invalid());
    }
    let tag = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let length = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if length != bytes.len() - 8 || bytes[6..8] != [0, 0] {
        return Err(invalid());
    }
    let header_len = match tag {
        IO_REPARSE_TAG_MOUNT_POINT => 8,
        IO_REPARSE_TAG_SYMLINK => 12,
        _ => return Err(io::ErrorKind::Unsupported.into()),
    };
    let (header, paths) = bytes[8..]
        .split_at_checked(header_len)
        .ok_or_else(invalid)?;
    let flags = if header_len == 12 {
        u32::from_le_bytes([header[8], header[9], header[10], header[11]])
    } else {
        0
    };
    let read_short = |index| u16::from_le_bytes([header[index], header[index + 1]]) as usize;
    if flags > 1 || !paths.len().is_multiple_of(2) {
        return Err(invalid());
    }
    let substitute = name_range(read_short(0), read_short(2), paths.len())?;
    let print = name_range(read_short(4), read_short(6), paths.len())?;
    if substitute.is_empty()
        || (!print.is_empty() && substitute.start < print.end && print.start < substitute.end)
    {
        return Err(invalid());
    }
    decode_name(&paths[print])?;
    let substitute = decode_name(&paths[substitute])?;
    if flags == 1 {
        if !substitute.split('\\').all(|name| component(name, true)) {
            return Err(invalid());
        }
        return Ok(WindowsLinkTarget::ParentRelative(PathBuf::from(substitute)));
    }
    let dos = substitute.strip_prefix(r"\??\").ok_or_else(invalid)?;
    if !matches!(dos.as_bytes().get(..3), Some([drive, b':', b'\\']) if drive.is_ascii_alphabetic())
        || (dos.len() != 3 && !dos[3..].split('\\').all(|name| component(name, false)))
    {
        return Err(invalid());
    }
    Ok(WindowsLinkTarget::AbsoluteDos(PathBuf::from(dos)))
}

fn name_range(offset: usize, length: usize, limit: usize) -> io::Result<std::ops::Range<usize>> {
    let end = offset + length; // Both values originated as u16.
    if !offset.is_multiple_of(2) || !length.is_multiple_of(2) || end > limit {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(offset..end)
}

fn decode_name(bytes: &[u8]) -> io::Result<String> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    let (units, trailing) = bytes.as_chunks::<2>();
    if !trailing.is_empty() || bytes.len() > TARGET_BYTES * 2 {
        return Err(invalid());
    }
    let units = units.iter().copied().map(u16::from_le_bytes);
    let name = char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|_| invalid())?;
    if name.len() > TARGET_BYTES || name.contains('\0') {
        return Err(invalid());
    }
    Ok(name)
}

fn component(name: &str, allow_dot: bool) -> bool {
    if matches!(name, "." | "..") {
        return allow_dot;
    }
    if name.is_empty()
        || name.encode_utf16().take(256).count() > 255
        || name.ends_with([' ', '.'])
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !["COM", "LPT"].into_iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}
