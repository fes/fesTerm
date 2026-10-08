//! Safe ownership around Win32 object security and local named-pipe operations.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
pub mod named_pipe;

#[cfg(windows)]
mod imp {
    use std::{
        fs::{File, OpenOptions},
        io, mem,
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::Path,
        ptr,
    };

    use windows_sys::Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FileRenameInformation, FileStreamInformation, NtCreateFile, NtQueryEaFile,
            NtQueryInformationFile, NtSetInformationFile, FILE_CREATE, FILE_DIRECTORY_FILE,
            FILE_FULL_EA_INFORMATION, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_RENAME_INFORMATION, FILE_STREAM_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, LocalFree, RtlNtStatusToDosError, SetHandleInformation,
            SetLastError, ERROR_INVALID_PARAMETER, ERROR_NOT_ALL_ASSIGNED, ERROR_NOT_SUPPORTED,
            ERROR_NO_TOKEN, ERROR_SUCCESS, GENERIC_ALL, GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT,
            INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, STATUS_BUFFER_OVERFLOW,
            STATUS_BUFFER_TOO_SMALL, STATUS_INFO_LENGTH_MISMATCH, STATUS_NO_EAS_ON_FILE,
            STATUS_NO_MORE_EAS, STATUS_SUCCESS, UNICODE_STRING,
        },
        Security::{
            AclSizeInformation, AddAccessAllowedAceEx, AdjustTokenPrivileges,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            CreateWellKnownSid, DuplicateTokenEx, EqualSid, GetAce, GetAclInformation,
            GetKernelObjectSecurity, GetLengthSid, GetSecurityDescriptorControl,
            GetSecurityDescriptorDacl, GetSecurityDescriptorGroup, GetSecurityDescriptorOwner,
            GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
            SecurityImpersonation, SetKernelObjectSecurity, SetSecurityDescriptorControl,
            SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, SetTokenInformation,
            TokenDefaultDacl, TokenImpersonation, TokenUser, WinBuiltinAdministratorsSid,
            WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION,
            ACL_SIZE_INFORMATION, ATTRIBUTE_SECURITY_INFORMATION, DACL_SECURITY_INFORMATION,
            GROUP_SECURITY_INFORMATION, INHERIT_ONLY_ACE, LABEL_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
            SACL_SECURITY_INFORMATION, SCOPE_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
            SECURITY_DESCRIPTOR, SE_DACL_PROTECTED, SE_PRIVILEGE_ENABLED, SE_SECURITY_NAME,
            TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_PRIVILEGES, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE,
            TOKEN_IMPERSONATE, TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{
            FileBasicInfo, FileDispositionInfo, GetFileInformationByHandle,
            GetFileInformationByHandleEx, ReOpenFile, SetFileInformationByHandle,
            BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_BASIC_INFO, FILE_DELETE_CHILD, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_READ_EA, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, READ_CONTROL, SYNCHRONIZE,
            WRITE_DAC, WRITE_OWNER,
        },
        System::{
            Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
            SystemServices::ACCESS_SYSTEM_SECURITY,
            Threading::{
                GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken,
                OpenThreadToken, SetThreadToken, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };

    #[cfg(test)]
    type PrivateVerificationHook = Box<dyn FnOnce(&File)>;

    #[cfg(test)]
    thread_local! {
        static BEFORE_PRIVATE_OBJECT_VERIFICATION: std::cell::RefCell<Option<PrivateVerificationHook>> =
            const { std::cell::RefCell::new(None) };
    }

    fn before_private_object_verification(_file: &File) {
        #[cfg(test)]
        BEFORE_PRIVATE_OBJECT_VERIFICATION.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook(_file);
            }
        });
    }

    /// Restores the process token's original default DACL and closes the token.
    pub struct DefaultDaclGuard {
        token: HANDLE,
        original: Vec<usize>,
        restored: bool,
    }

    /// Restores the saving thread's prior impersonation token.
    pub struct SecurityPrivilegeGuard {
        _token: OwnedHandle,
        previous: Option<OwnedHandle>,
    }

    impl Drop for SecurityPrivilegeGuard {
        fn drop(&mut self) {
            let previous = self
                .previous
                .as_ref()
                .map_or(ptr::null_mut(), |token| token.as_raw_handle());
            if unsafe { SetThreadToken(ptr::null(), previous) } == 0 {
                let restore_error = io::Error::last_os_error();
                let removed = unsafe { SetThreadToken(ptr::null(), ptr::null_mut()) } != 0;
                tracing::error!(
                    error = %restore_error,
                    privileged_token_removed = removed,
                    "the saving thread's prior Windows security token could not be restored; aborting before privileged execution can continue"
                );
                std::process::abort();
            }
        }
    }

    /// Enables the audit-policy privilege needed to capture and reproduce a
    /// file's SACL on a saving-thread impersonation token. Existing-file
    /// replacement is refused when the process token does not hold it.
    pub fn enable_security_privilege() -> io::Result<SecurityPrivilegeGuard> {
        let mut process_token = ptr::null_mut();
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE,
                &raw mut process_token,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let process_token = unsafe { OwnedHandle::from_raw_handle(process_token) };
        let mut token = ptr::null_mut();
        if unsafe {
            DuplicateTokenEx(
                process_token.as_raw_handle(),
                TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES | TOKEN_IMPERSONATE,
                ptr::null(),
                SecurityImpersonation,
                TokenImpersonation,
                &raw mut token,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        (|| {
            let mut luid = windows_sys::Win32::Foundation::LUID::default();
            if unsafe {
                windows_sys::Win32::Security::LookupPrivilegeValueW(
                    ptr::null(),
                    SE_SECURITY_NAME,
                    &raw mut luid,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let requested = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            unsafe { SetLastError(ERROR_SUCCESS) };
            if unsafe {
                AdjustTokenPrivileges(
                    token.as_raw_handle(),
                    0,
                    &raw const requested,
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "SeSecurityPrivilege is unavailable",
                ));
            }
            let mut previous = ptr::null_mut();
            let previous = if unsafe {
                OpenThreadToken(
                    GetCurrentThread(),
                    TOKEN_QUERY | TOKEN_IMPERSONATE,
                    1,
                    &raw mut previous,
                )
            } != 0
            {
                Some(unsafe { OwnedHandle::from_raw_handle(previous) })
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                    return Err(error);
                }
                None
            };
            if unsafe { SetThreadToken(ptr::null(), token.as_raw_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(SecurityPrivilegeGuard {
                _token: token,
                previous,
            })
        })()
    }

    /// Access-control and attribute metadata that replacement must preserve.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct SecurityMetadata {
        descriptor: Vec<u8>,
        audit_sacl: Option<Vec<u8>>,
        mandatory_label: Vec<u8>,
        resource_attributes: Vec<u8>,
        scoped_policy: Vec<u8>,
        attributes: u32,
        unsupported_integrity_attributes: u32,
        encrypted: bool,
        has_named_streams: bool,
        has_extended_attributes: bool,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct StagingParentSecurity {
        descriptor: Vec<u8>,
    }

    const FILE_ATTRIBUTE_VALID_SET_FLAGS: u32 = 0x0000_31A7;
    const FILE_ATTRIBUTE_INTEGRITY_STREAM_FLAG: u32 = 0x0000_8000;
    const FILE_ATTRIBUTE_NO_SCRUB_DATA_FLAG: u32 = 0x0002_0000;
    const UNSUPPORTED_INTEGRITY_ATTRIBUTES: u32 =
        FILE_ATTRIBUTE_INTEGRITY_STREAM_FLAG | FILE_ATTRIBUTE_NO_SCRUB_DATA_FLAG;
    type ExtendedAttributeProbe = [u32; 4];
    const _: () = {
        assert!(
            mem::size_of::<ExtendedAttributeProbe>() >= mem::size_of::<FILE_FULL_EA_INFORMATION>()
        );
        assert!(
            mem::align_of::<ExtendedAttributeProbe>()
                >= mem::align_of::<FILE_FULL_EA_INFORMATION>()
        );
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum UnsupportedSecurityMetadata {
        EfsEncryption,
        NamedStreams,
        IntegrityPolicy,
        ExtendedAttributes,
    }

    fn settable_file_attributes(attributes: u32) -> u32 {
        attributes & FILE_ATTRIBUTE_VALID_SET_FLAGS
    }

    fn unsupported_integrity_attributes(attributes: u32) -> u32 {
        attributes & UNSUPPORTED_INTEGRITY_ATTRIBUTES
    }

    fn file_is_encrypted(attributes: u32) -> bool {
        attributes & FILE_ATTRIBUTE_ENCRYPTED != 0
    }

    impl SecurityMetadata {
        /// EFS encryption cannot be reproduced with `FILE_BASIC_INFO`.
        pub const fn is_encrypted(&self) -> bool {
            self.encrypted
        }

        /// Named streams, including Mark-of-the-Web, require exact copying.
        pub const fn has_named_streams(&self) -> bool {
            self.has_named_streams
        }

        /// Integrity-stream and no-scrub policy need dedicated filesystem
        /// controls; basic attributes cannot reproduce them exactly.
        pub const fn has_unsupported_integrity_attributes(&self) -> bool {
            self.unsupported_integrity_attributes != 0
        }

        pub const fn unsupported_reason(&self) -> Option<UnsupportedSecurityMetadata> {
            if self.encrypted {
                Some(UnsupportedSecurityMetadata::EfsEncryption)
            } else if self.has_named_streams {
                Some(UnsupportedSecurityMetadata::NamedStreams)
            } else if self.unsupported_integrity_attributes != 0 {
                Some(UnsupportedSecurityMetadata::IntegrityPolicy)
            } else if self.has_extended_attributes {
                Some(UnsupportedSecurityMetadata::ExtendedAttributes)
            } else {
                None
            }
        }
    }

    fn file_has_extended_attributes(file: &File) -> io::Result<bool> {
        let mut status = windows_sys::Win32::System::IO::IO_STATUS_BLOCK::default();
        let mut probe = ExtendedAttributeProbe::default();
        let result = unsafe {
            NtQueryEaFile(
                file.as_raw_handle() as HANDLE,
                &raw mut status,
                probe.as_mut_ptr().cast(),
                mem::size_of_val(&probe) as u32,
                false,
                ptr::null(),
                0,
                ptr::null(),
                true,
            )
        };
        match extended_attribute_query_result(result, status.Information) {
            Ok(has_attributes) => Ok(has_attributes),
            Err(status) => Err(io::Error::from_raw_os_error(unsafe {
                RtlNtStatusToDosError(status) as i32
            })),
        }
    }

    fn extended_attribute_query_result(status: i32, returned: usize) -> Result<bool, i32> {
        match status {
            STATUS_NO_EAS_ON_FILE | STATUS_NO_MORE_EAS => Ok(false),
            STATUS_SUCCESS => Ok(returned != 0),
            STATUS_BUFFER_OVERFLOW | STATUS_BUFFER_TOO_SMALL => Ok(true),
            _ => Err(status),
        }
    }

    fn file_has_named_streams(file: &File) -> io::Result<bool> {
        const INITIAL_BUFFER_SIZE: usize = 4 * 1024;
        const MAX_BUFFER_SIZE: usize = 8 * 1024 * 1024;
        let mut buffer_size = INITIAL_BUFFER_SIZE;
        loop {
            let mut storage = vec![0usize; buffer_size.div_ceil(mem::size_of::<usize>())];
            let mut status = windows_sys::Win32::System::IO::IO_STATUS_BLOCK::default();
            let result = unsafe {
                NtQueryInformationFile(
                    file.as_raw_handle() as HANDLE,
                    &raw mut status,
                    storage.as_mut_ptr().cast(),
                    buffer_size as u32,
                    FileStreamInformation,
                )
            };
            if matches!(
                result,
                STATUS_BUFFER_OVERFLOW | STATUS_BUFFER_TOO_SMALL | STATUS_INFO_LENGTH_MISMATCH
            ) {
                if buffer_size >= MAX_BUFFER_SIZE {
                    return Err(io::Error::from(io::ErrorKind::InvalidData));
                }
                buffer_size = (buffer_size * 2).min(MAX_BUFFER_SIZE);
                continue;
            }
            if result != STATUS_SUCCESS {
                return Err(io::Error::from_raw_os_error(unsafe {
                    RtlNtStatusToDosError(result) as i32
                }));
            }
            let returned = status.Information;
            if returned > buffer_size {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            return stream_buffer_has_named_streams(&storage, returned);
        }
    }

    fn stream_buffer_has_named_streams(storage: &[usize], returned: usize) -> io::Result<bool> {
        const DEFAULT_DATA_STREAM: [u16; 7] = [
            b':' as u16,
            b':' as u16,
            b'$' as u16,
            b'D' as u16,
            b'A' as u16,
            b'T' as u16,
            b'A' as u16,
        ];
        let header = mem::offset_of!(FILE_STREAM_INFORMATION, StreamName);
        if returned == 0 {
            return Ok(false);
        }
        let mut offset = 0usize;
        loop {
            if returned.saturating_sub(offset) < header {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            let entry = unsafe {
                &*storage
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset)
                    .cast::<FILE_STREAM_INFORMATION>()
            };
            let name_bytes = entry.StreamNameLength as usize;
            if !name_bytes.is_multiple_of(mem::size_of::<u16>())
                || name_bytes > returned.saturating_sub(offset + header)
            {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            let name = unsafe {
                std::slice::from_raw_parts(
                    ptr::addr_of!(entry.StreamName).cast::<u16>(),
                    name_bytes / mem::size_of::<u16>(),
                )
            };
            if name != DEFAULT_DATA_STREAM {
                return Ok(true);
            }
            let next = entry.NextEntryOffset as usize;
            if next == 0 {
                return Ok(false);
            }
            if next < header || next > returned.saturating_sub(offset) {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            offset += next;
        }
    }

    fn security_descriptor(file: &File, information: u32) -> io::Result<Vec<u8>> {
        const MAX_DESCRIPTOR_SIZE: u32 = 8 * 1024 * 1024;

        let handle = file.as_raw_handle() as HANDLE;
        let mut required = 0;
        let _ = unsafe {
            GetKernelObjectSecurity(handle, information, ptr::null_mut(), 0, &raw mut required)
        };
        if required == 0 || required > MAX_DESCRIPTOR_SIZE {
            return Err(if required == 0 {
                io::Error::last_os_error()
            } else {
                io::Error::from(io::ErrorKind::InvalidData)
            });
        }

        let mut descriptor = vec![0u8; required as usize];
        if unsafe {
            GetKernelObjectSecurity(
                handle,
                information,
                descriptor.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        descriptor.truncate(required as usize);
        Ok(descriptor)
    }

    fn well_known_sid(kind: i32) -> io::Result<Vec<u8>> {
        const SECURITY_MAX_SID_SIZE: usize = 68;
        let mut sid = vec![0u8; SECURITY_MAX_SID_SIZE];
        let mut length = sid.len() as u32;
        if unsafe {
            CreateWellKnownSid(
                kind,
                ptr::null_mut(),
                sid.as_mut_ptr().cast(),
                &raw mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        sid.truncate(length as usize);
        Ok(sid)
    }

    fn trusted_parent_sid(
        sid: *mut core::ffi::c_void,
        user: *mut core::ffi::c_void,
        system: &[u8],
        administrators: &[u8],
    ) -> bool {
        (unsafe { EqualSid(sid, user) }) != 0
            || (unsafe { EqualSid(sid, system.as_ptr().cast_mut().cast()) }) != 0
            || (unsafe { EqualSid(sid, administrators.as_ptr().cast_mut().cast()) }) != 0
    }

    fn staging_parent_security(directory: &File) -> io::Result<Option<StagingParentSecurity>> {
        const ACCESS_ALLOWED_OBJECT_ACE_TYPE: u8 = 5;
        const ACCESS_ALLOWED_CALLBACK_ACE_TYPE: u8 = 9;
        const ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE: u8 = 11;
        const DANGEROUS: u32 = FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;

        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let user = token_information(token.as_raw_handle(), TokenUser)?;
        let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
        let system = well_known_sid(WinLocalSystemSid)?;
        let administrators = well_known_sid(WinBuiltinAdministratorsSid)?;

        let descriptor = security_descriptor(
            directory,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        )?;
        let mut owner = ptr::null_mut();
        let mut owner_defaulted = 0;
        if unsafe {
            GetSecurityDescriptorOwner(
                descriptor.as_ptr().cast_mut().cast(),
                &raw mut owner,
                &raw mut owner_defaulted,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut dacl = ptr::null_mut();
        let mut dacl_present = 0;
        let mut dacl_defaulted = 0;
        if unsafe {
            GetSecurityDescriptorDacl(
                descriptor.as_ptr().cast_mut().cast(),
                &raw mut dacl_present,
                &raw mut dacl,
                &raw mut dacl_defaulted,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }

        if owner.is_null()
            || dacl_present == 0
            || dacl.is_null()
            || !trusted_parent_sid(owner, user.User.Sid, &system, &administrators)
        {
            return Ok(None);
        }
        let mut information = ACL_SIZE_INFORMATION::default();
        if unsafe {
            GetAclInformation(
                dacl,
                (&raw mut information).cast(),
                mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        for index in 0..information.AceCount {
            let mut ace = ptr::null_mut();
            if unsafe { GetAce(dacl, index, &raw mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            if header.AceFlags & INHERIT_ONLY_ACE as u8 != 0
                || ![
                    0,
                    ACCESS_ALLOWED_OBJECT_ACE_TYPE,
                    ACCESS_ALLOWED_CALLBACK_ACE_TYPE,
                    ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE,
                ]
                .contains(&header.AceType)
                || usize::from(header.AceSize)
                    < mem::size_of::<ACE_HEADER>() + mem::size_of::<u32>()
            {
                continue;
            }
            let mask = unsafe {
                *ace.cast::<u8>()
                    .add(mem::size_of::<ACE_HEADER>())
                    .cast::<u32>()
            };
            if mask & DANGEROUS == 0 {
                continue;
            }
            if header.AceType != 0
                || usize::from(header.AceSize) < mem::size_of::<ACCESS_ALLOWED_ACE>()
            {
                return Ok(None);
            }
            let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            if !trusted_parent_sid(
                (&raw const allowed.SidStart).cast_mut().cast(),
                user.User.Sid,
                &system,
                &administrators,
            ) {
                return Ok(None);
            }
        }

        Ok(Some(StagingParentSecurity { descriptor }))
    }

    /// Returns the coherent owner/DACL snapshot when the parent is trusted,
    /// or `None` when its current access policy permits untrusted mutation.
    pub fn secure_staging_parent(directory: &File) -> io::Result<Option<StagingParentSecurity>> {
        staging_parent_security(directory)
    }

    pub fn staging_parent_matches(
        directory: &File,
        expected: &StagingParentSecurity,
    ) -> io::Result<bool> {
        Ok(secure_staging_parent(directory)?
            .as_ref()
            .is_some_and(|current| current == expected))
    }

    /// Snapshots access-control metadata and file attributes through an open handle.
    pub fn security_metadata(file: &File) -> io::Result<SecurityMetadata> {
        let descriptor = security_descriptor(
            file,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        )?;
        let audit_sacl = match security_descriptor(file, SACL_SECURITY_INFORMATION) {
            Ok(descriptor) => Some(descriptor),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => None,
            Err(error) => return Err(error),
        };
        let mandatory_label = security_descriptor(file, LABEL_SECURITY_INFORMATION)?;
        let resource_attributes = security_descriptor(file, ATTRIBUTE_SECURITY_INFORMATION)?;
        let scoped_policy = security_descriptor(file, SCOPE_SECURITY_INFORMATION)?;

        let handle = file.as_raw_handle() as HANDLE;
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle, &raw mut information) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SecurityMetadata {
            descriptor,
            audit_sacl,
            mandatory_label,
            resource_attributes,
            scoped_policy,
            attributes: settable_file_attributes(information.dwFileAttributes),
            unsupported_integrity_attributes: unsupported_integrity_attributes(
                information.dwFileAttributes,
            ),
            encrypted: file_is_encrypted(information.dwFileAttributes),
            has_named_streams: file_has_named_streams(file)?,
            has_extended_attributes: file_has_extended_attributes(file)?,
        })
    }

    /// Reports whether a handle still has the captured security metadata.
    pub fn security_metadata_matches(file: &File, expected: &SecurityMetadata) -> io::Result<bool> {
        security_metadata(file)
            .map(|current| security_metadata_mismatch(&current, expected).is_none())
    }

    fn security_metadata_mismatch(
        current: &SecurityMetadata,
        expected: &SecurityMetadata,
    ) -> Option<&'static str> {
        if current.descriptor != expected.descriptor {
            Some(security_descriptor_mismatch(
                &current.descriptor,
                &expected.descriptor,
            ))
        } else if current.audit_sacl != expected.audit_sacl {
            Some("audit SACL")
        } else if current.mandatory_label != expected.mandatory_label {
            Some("mandatory label")
        } else if current.resource_attributes != expected.resource_attributes {
            Some("resource attributes")
        } else if current.scoped_policy != expected.scoped_policy {
            Some("central access policy")
        } else if current.attributes != expected.attributes {
            Some("file attributes")
        } else if current.unsupported_integrity_attributes
            != expected.unsupported_integrity_attributes
        {
            Some("integrity attributes")
        } else if current.encrypted != expected.encrypted {
            Some("EFS encryption state")
        } else if current.has_named_streams != expected.has_named_streams {
            Some("named streams")
        } else if current.has_extended_attributes != expected.has_extended_attributes {
            Some("extended attributes")
        } else {
            None
        }
    }

    fn security_descriptor_mismatch(current: &[u8], expected: &[u8]) -> &'static str {
        let current = current.as_ptr().cast_mut().cast();
        let expected = expected.as_ptr().cast_mut().cast();
        let mut current_owner = ptr::null_mut();
        let mut expected_owner = ptr::null_mut();
        let mut ignored = 0;
        if unsafe { GetSecurityDescriptorOwner(current, &raw mut current_owner, &raw mut ignored) }
            == 0
            || unsafe {
                GetSecurityDescriptorOwner(expected, &raw mut expected_owner, &raw mut ignored)
            } == 0
            || current_owner.is_null() != expected_owner.is_null()
            || (!current_owner.is_null() && unsafe { EqualSid(current_owner, expected_owner) } == 0)
        {
            return "owner";
        }

        let mut current_group = ptr::null_mut();
        let mut expected_group = ptr::null_mut();
        if unsafe { GetSecurityDescriptorGroup(current, &raw mut current_group, &raw mut ignored) }
            == 0
            || unsafe {
                GetSecurityDescriptorGroup(expected, &raw mut expected_group, &raw mut ignored)
            } == 0
            || current_group.is_null() != expected_group.is_null()
            || (!current_group.is_null() && unsafe { EqualSid(current_group, expected_group) } == 0)
        {
            return "group";
        }

        let mut current_dacl_present = 0;
        let mut expected_dacl_present = 0;
        let mut current_dacl = ptr::null_mut();
        let mut expected_dacl = ptr::null_mut();
        if unsafe {
            GetSecurityDescriptorDacl(
                current,
                &raw mut current_dacl_present,
                &raw mut current_dacl,
                &raw mut ignored,
            )
        } == 0
            || unsafe {
                GetSecurityDescriptorDacl(
                    expected,
                    &raw mut expected_dacl_present,
                    &raw mut expected_dacl,
                    &raw mut ignored,
                )
            } == 0
            || current_dacl_present != expected_dacl_present
            || current_dacl.is_null() != expected_dacl.is_null()
        {
            return "DACL presence";
        }
        if !current_dacl.is_null() {
            // SAFETY: the validated security descriptors own both ACLs, whose
            // headers declare the byte ranges returned by Windows.
            let (current_acl, expected_acl) = unsafe {
                (
                    std::slice::from_raw_parts(
                        current_dacl.cast::<u8>(),
                        usize::from((*current_dacl).AclSize),
                    ),
                    std::slice::from_raw_parts(
                        expected_dacl.cast::<u8>(),
                        usize::from((*expected_dacl).AclSize),
                    ),
                )
            };
            if current_acl != expected_acl {
                return "DACL";
            }
        }

        let mut current_control = 0;
        let mut expected_control = 0;
        let mut revision = 0;
        if unsafe {
            GetSecurityDescriptorControl(current, &raw mut current_control, &raw mut revision)
        } == 0
            || unsafe {
                GetSecurityDescriptorControl(expected, &raw mut expected_control, &raw mut revision)
            } == 0
            || current_control != expected_control
        {
            return "DACL control";
        }
        "owner, group, or DACL serialization"
    }

    fn apply_security_descriptor_if_changed(
        file: &File,
        information: u32,
        expected: &[u8],
    ) -> io::Result<()> {
        if security_descriptor(file, information)? == expected {
            return Ok(());
        }
        if information == SCOPE_SECURITY_INFORMATION {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the file's central access policy cannot be reproduced safely",
            ));
        }
        if unsafe {
            SetKernelObjectSecurity(
                file.as_raw_handle() as HANDLE,
                information,
                expected.as_ptr().cast_mut().cast(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if security_descriptor(file, information)? == expected {
            Ok(())
        } else {
            Err(io::Error::other(
                "prepared Windows access-control metadata did not match its source",
            ))
        }
    }

    /// Applies captured access-control metadata and file attributes to a
    /// prepared replacement before it can become visible.
    pub fn apply_security_metadata(file: &File, metadata: &SecurityMetadata) -> io::Result<()> {
        const INFORMATION: u32 =
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        if let Some(reason) = metadata.unsupported_reason() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                match reason {
                    UnsupportedSecurityMetadata::EfsEncryption => {
                        "NTFS EFS encryption cannot be preserved through basic file metadata"
                    }
                    UnsupportedSecurityMetadata::NamedStreams => {
                        "NTFS alternate data streams cannot be preserved through basic file metadata"
                    }
                    UnsupportedSecurityMetadata::IntegrityPolicy => {
                        "filesystem integrity or scrub attributes cannot be preserved through basic file metadata"
                    }
                    UnsupportedSecurityMetadata::ExtendedAttributes => {
                        "NTFS extended attributes cannot be preserved through basic file metadata"
                    }
                },
            ));
        }
        let audit_sacl = metadata.audit_sacl.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the file's audit SACL was not captured",
            )
        })?;
        let handle = file.as_raw_handle() as HANDLE;
        if unsafe {
            SetKernelObjectSecurity(
                handle,
                INFORMATION,
                metadata.descriptor.as_ptr().cast_mut().cast(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        apply_security_descriptor_if_changed(file, SACL_SECURITY_INFORMATION, audit_sacl)?;
        apply_security_descriptor_if_changed(
            file,
            LABEL_SECURITY_INFORMATION,
            &metadata.mandatory_label,
        )?;
        apply_security_descriptor_if_changed(
            file,
            ATTRIBUTE_SECURITY_INFORMATION,
            &metadata.resource_attributes,
        )?;
        apply_security_descriptor_if_changed(
            file,
            SCOPE_SECURITY_INFORMATION,
            &metadata.scoped_policy,
        )?;
        let basic = FILE_BASIC_INFO {
            FileAttributes: metadata.attributes,
            ..FILE_BASIC_INFO::default()
        };
        if unsafe {
            SetFileInformationByHandle(
                handle,
                FileBasicInfo,
                (&raw const basic).cast(),
                mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let current = security_metadata(file)?;
        match security_metadata_mismatch(&current, metadata) {
            None => Ok(()),
            Some(field) => Err(io::Error::other(format!(
                "prepared Windows {field} metadata did not match its source"
            ))),
        }
    }

    /// Replaces an object's DACL with one protected full-access ACE for the
    /// current user and verifies the exact result through the same handle.
    pub fn restrict_to_current_user(file: &File) -> io::Result<()> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let user = token_information(token.as_raw_handle(), TokenUser)?;
        let token_user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
        let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
        if sid_length == 0 {
            return Err(io::Error::last_os_error());
        }

        let acl_bytes = mem::size_of::<ACL>() + mem::size_of::<ACCESS_ALLOWED_ACE>()
            - mem::size_of::<u32>()
            + sid_length as usize;
        let mut acl_storage = vec![0usize; acl_bytes.div_ceil(mem::size_of::<usize>())];
        let acl = acl_storage.as_mut_ptr().cast::<ACL>();
        if unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, token_user.User.Sid)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }

        let mut descriptor = SECURITY_DESCRIPTOR::default();
        if unsafe { InitializeSecurityDescriptor((&raw mut descriptor).cast(), 1) } == 0
            || unsafe {
                SetSecurityDescriptorOwner((&raw mut descriptor).cast(), token_user.User.Sid, 0)
            } == 0
            || unsafe { SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, acl, 0) } == 0
            || unsafe {
                SetSecurityDescriptorControl(
                    (&raw mut descriptor).cast(),
                    SE_DACL_PROTECTED,
                    SE_DACL_PROTECTED,
                )
            } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            SetKernelObjectSecurity(
                file.as_raw_handle() as HANDLE,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                (&raw mut descriptor).cast(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if current_user_only_security(file, token_user.User.Sid)? {
            Ok(())
        } else {
            Err(io::Error::other(
                "object owner and DACL are not protected current-user-only access",
            ))
        }
    }

    /// Reports whether an exact object is owned by the current user and has a
    /// protected DACL containing only one full-access ACE for that user.
    pub fn is_current_user_only(file: &File) -> io::Result<bool> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let user = token_information(token.as_raw_handle(), TokenUser)?;
        let token_user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
        current_user_only_security(file, token_user.User.Sid)
    }

    fn current_user_only_security(
        file: &File,
        user_sid: *mut core::ffi::c_void,
    ) -> io::Result<bool> {
        const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
        let mut owner = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut owner,
                ptr::null_mut(),
                &raw mut dacl,
                ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let verified = (|| {
            if owner.is_null() || dacl.is_null() || unsafe { EqualSid(owner, user_sid) } == 0 {
                return false;
            }
            let mut control = 0;
            let mut revision = 0;
            if unsafe {
                GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision)
            } == 0
                || control & SE_DACL_PROTECTED == 0
            {
                return false;
            }
            let mut information = ACL_SIZE_INFORMATION::default();
            if unsafe {
                GetAclInformation(
                    dacl,
                    (&raw mut information).cast(),
                    mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            } == 0
                || information.AceCount != 1
            {
                return false;
            }
            let mut ace = ptr::null_mut();
            if unsafe { GetAce(dacl, 0, &raw mut ace) } == 0 {
                return false;
            }
            let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            ace.Header.AceType == ACCESS_ALLOWED_ACE_TYPE
                && ace.Mask == FILE_ALL_ACCESS
                && unsafe { EqualSid((&raw const ace.SidStart).cast_mut().cast(), user_sid) } != 0
        })();
        let _ = unsafe { LocalFree(descriptor) };
        Ok(verified)
    }

    impl DefaultDaclGuard {
        /// Restores the token before launching any child process that could
        /// otherwise inherit the temporary current-user-only default DACL.
        pub fn restore(mut self) -> io::Result<()> {
            let result = self.restore_inner();
            self.restored = result.is_ok();
            result
        }

        fn restore_inner(&mut self) -> io::Result<()> {
            set_token_default_dacl(self.token, self.original.as_ptr().cast())
        }
    }

    impl Drop for DefaultDaclGuard {
        fn drop(&mut self) {
            if !self.restored {
                let _ = self.restore_inner();
            }
            let _ = unsafe { CloseHandle(self.token) };
        }
    }

    /// Temporarily changes this process token's default DACL to one ACE that
    /// grants the current user full access.
    ///
    /// Win32's named-pipe API uses the token default DACL when passed null
    /// security attributes. Callers create the pipe while this guard is live,
    /// then call [`DefaultDaclGuard::restore`] before launching the shell.
    pub fn restrict_default_dacl_to_current_user() -> io::Result<DefaultDaclGuard> {
        let mut token = ptr::null_mut();
        let opened = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
                &raw mut token,
            )
        };
        if opened == 0 {
            return Err(io::Error::last_os_error());
        }

        let result = (|| {
            let original = token_information(token, TokenDefaultDacl)?;
            let user = token_information(token, TokenUser)?;
            let token_user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
            let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
            if sid_length == 0 {
                return Err(io::Error::last_os_error());
            }

            let acl_bytes = mem::size_of::<ACL>()
                + mem::size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()
                - mem::size_of::<u32>()
                + sid_length as usize;
            let mut acl_storage = vec![0usize; acl_bytes.div_ceil(mem::size_of::<usize>())];
            let acl = acl_storage.as_mut_ptr().cast::<ACL>();
            let initialized = unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) };
            if initialized == 0 {
                return Err(io::Error::last_os_error());
            }
            let added = unsafe {
                AddAccessAllowedAceEx(acl, ACL_REVISION, 0, GENERIC_ALL, token_user.User.Sid)
            };
            if added == 0 {
                return Err(io::Error::last_os_error());
            }

            let restricted = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
            set_token_default_dacl(token, (&raw const restricted).cast())?;
            Ok(DefaultDaclGuard {
                token,
                original,
                restored: false,
            })
        })();

        if result.is_err() {
            let _ = unsafe { CloseHandle(token) };
        }
        result
    }

    fn with_current_user_only_security<T>(
        create: impl FnOnce(*const SECURITY_ATTRIBUTES) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let user = token_information(token.as_raw_handle(), TokenUser)?;
        let token_user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
        let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
        if sid_length == 0 {
            return Err(io::Error::last_os_error());
        }

        let acl_bytes = mem::size_of::<ACL>() + mem::size_of::<ACCESS_ALLOWED_ACE>()
            - mem::size_of::<u32>()
            + sid_length as usize;
        let mut acl_storage = vec![0usize; acl_bytes.div_ceil(mem::size_of::<usize>())];
        let acl = acl_storage.as_mut_ptr().cast::<ACL>();
        if unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) } == 0
            || unsafe {
                AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, token_user.User.Sid)
            } == 0
        {
            return Err(io::Error::last_os_error());
        }

        let mut descriptor = SECURITY_DESCRIPTOR::default();
        if unsafe { InitializeSecurityDescriptor((&raw mut descriptor).cast(), 1) } == 0
            || unsafe {
                SetSecurityDescriptorOwner((&raw mut descriptor).cast(), token_user.User.Sid, 0)
            } == 0
            || unsafe { SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, acl, 0) } == 0
            || unsafe {
                SetSecurityDescriptorControl(
                    (&raw mut descriptor).cast(),
                    SE_DACL_PROTECTED,
                    SE_DACL_PROTECTED,
                )
            } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&raw mut descriptor).cast(),
            bInheritHandle: 0,
        };
        create(&raw const attributes)
    }

    fn relative_object_name(name: &Path) -> io::Result<(Vec<u16>, UNICODE_STRING)> {
        let mut components = name.components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the object name must be one path component",
            ));
        }
        let encoded: Vec<u16> = name.as_os_str().encode_wide().collect();
        let bytes = encoded
            .len()
            .checked_mul(mem::size_of::<u16>())
            .and_then(|length| u16::try_from(length).ok())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let name = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: encoded.as_ptr().cast_mut(),
        };
        Ok((encoded, name))
    }

    fn open_relative(
        directory: &File,
        name: &Path,
        desired_access: u32,
        share_access: u32,
        disposition: u32,
        create_options: u32,
        security_attributes: *const SECURITY_ATTRIBUTES,
    ) -> io::Result<File> {
        let (_encoded, object_name) = relative_object_name(name)?;
        let mut attributes = OBJECT_ATTRIBUTES {
            Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: directory.as_raw_handle() as HANDLE,
            ObjectName: &raw const object_name,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: ptr::null(),
            SecurityQualityOfService: ptr::null(),
        };
        if !security_attributes.is_null() {
            attributes.SecurityDescriptor =
                unsafe { (*security_attributes).lpSecurityDescriptor.cast() };
        }
        let mut handle = INVALID_HANDLE_VALUE;
        let mut status = windows_sys::Win32::System::IO::IO_STATUS_BLOCK::default();
        let result = unsafe {
            NtCreateFile(
                &raw mut handle,
                desired_access | SYNCHRONIZE,
                &raw const attributes,
                &raw mut status,
                ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                share_access,
                disposition,
                create_options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                ptr::null(),
                0,
            )
        };
        if result == windows_sys::Win32::Foundation::STATUS_FILE_IS_A_DIRECTORY {
            return Err(io::Error::from(io::ErrorKind::IsADirectory));
        }
        if result != STATUS_SUCCESS {
            return Err(io::Error::from_raw_os_error(unsafe {
                RtlNtStatusToDosError(result) as i32
            }));
        }
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn file_information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &raw mut information)
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(information)
        }
    }

    fn verify_private_object(
        object: &File,
        directory: &File,
        expect_directory: bool,
    ) -> io::Result<()> {
        let object_information = file_information(object)?;
        let directory_information = file_information(directory)?;
        let is_directory = object_information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        if object_information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || is_directory != expect_directory
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the created private object has the wrong type or is a reparse point",
            ));
        }
        if object_information.dwVolumeSerialNumber != directory_information.dwVolumeSerialNumber {
            return Err(io::Error::new(
                io::ErrorKind::CrossesDevices,
                "the created private object is not on the expected volume",
            ));
        }

        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let user = token_information(token.as_raw_handle(), TokenUser)?;
        let token_user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
        if !current_user_only_security(object, token_user.User.Sid)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the created private object is not current-user-owned and current-user-only",
            ));
        }
        Ok(())
    }

    /// Creates a new file relative to the exact `directory` handle with a
    /// protected current-user-only DACL, then verifies its exact handle before
    /// returning it to a caller that may write document bytes.
    pub fn create_current_user_only_file(directory: &File, name: &Path) -> io::Result<File> {
        create_current_user_only_file_with_sharing(
            directory,
            name,
            FILE_ALL_ACCESS | DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )
    }

    /// Creates a movable private file with access to apply and verify an audit
    /// SACL while SeSecurityPrivilege is enabled.
    pub fn create_current_user_only_file_with_audit(
        directory: &File,
        name: &Path,
    ) -> io::Result<File> {
        create_current_user_only_file_with_sharing(
            directory,
            name,
            FILE_ALL_ACCESS | DELETE | ACCESS_SYSTEM_SECURITY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )
    }

    /// Creates a private file whose retained handle permits a separate
    /// delete-only move handle but prevents every other data reader or writer.
    pub fn create_current_user_only_file_exclusive(
        directory: &File,
        name: &Path,
    ) -> io::Result<File> {
        create_current_user_only_file_with_sharing(
            directory,
            name,
            FILE_ALL_ACCESS & !DELETE,
            FILE_SHARE_DELETE,
        )
    }

    /// Creates the retained save payload with the additional system-security
    /// access needed to reproduce and verify an existing target's audit SACL.
    pub fn create_current_user_only_file_exclusive_with_audit(
        directory: &File,
        name: &Path,
    ) -> io::Result<File> {
        create_current_user_only_file_with_sharing(
            directory,
            name,
            (FILE_ALL_ACCESS & !DELETE) | ACCESS_SYSTEM_SECURITY,
            FILE_SHARE_DELETE,
        )
    }

    fn create_current_user_only_file_with_sharing(
        directory: &File,
        name: &Path,
        desired_access: u32,
        share_access: u32,
    ) -> io::Result<File> {
        let file = with_current_user_only_security(|security| {
            open_relative(
                directory,
                name,
                desired_access,
                share_access,
                FILE_CREATE,
                FILE_NON_DIRECTORY_FILE,
                security,
            )
        })?;
        before_private_object_verification(&file);
        verify_private_object(&file, directory, false)?;
        Ok(file)
    }

    /// Opens a file beneath a retained directory without traversing a final
    /// reparse point.
    pub fn open_file_no_reparse(directory: &File, name: &Path) -> io::Result<File> {
        open_file_no_reparse_with_access(directory, name, GENERIC_READ)
    }

    /// Retains the exact file object for identity comparisons without blocking
    /// later rename or deletion of either the file or its parent directory.
    pub fn reopen_file_for_identity(file: &File) -> io::Result<File> {
        // SAFETY: `file` owns a live file handle. A successful call returns one
        // newly owned handle to the same file object without resolving a path.
        let reopened = unsafe {
            ReOpenFile(
                file.as_raw_handle(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if reopened == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful `ReOpenFile` returned one newly owned handle.
        Ok(unsafe { File::from_raw_handle(reopened) })
    }

    /// Retains a no-follow file for generation, content, and security checks
    /// without holding delete access; a separate exact move handle can coexist.
    pub fn open_file_no_reparse_for_capture(directory: &File, name: &Path) -> io::Result<File> {
        open_file_no_reparse_for_capture_with_access(directory, name, 0)
    }

    /// Retains a save target with access to capture and later restore its
    /// audit SACL as well as its ordinary access metadata.
    pub fn open_file_no_reparse_for_security_capture(
        directory: &File,
        name: &Path,
    ) -> io::Result<File> {
        open_file_no_reparse_for_capture_with_access(directory, name, ACCESS_SYSTEM_SECURITY)
    }

    fn open_file_no_reparse_for_capture_with_access(
        directory: &File,
        name: &Path,
        security_access: u32,
    ) -> io::Result<File> {
        open_file_no_reparse_with_access(
            directory,
            name,
            GENERIC_READ | FILE_WRITE_ATTRIBUTES | WRITE_DAC | WRITE_OWNER | security_access,
        )
    }

    /// Opens an exact staged file with only the access needed to move it.
    pub fn open_file_no_reparse_for_move(directory: &File, name: &Path) -> io::Result<File> {
        open_file_no_reparse_with_access(directory, name, FILE_READ_ATTRIBUTES | DELETE)
    }

    /// Opens a pathname for identity and security verification without
    /// requesting data access from an exclusively retained save payload.
    /// Delete access without delete sharing pins its name; attribute-only
    /// handles do not participate in delete-sharing checks.
    pub fn open_file_no_reparse_for_verification(
        directory: &File,
        name: &Path,
    ) -> io::Result<File> {
        open_file_no_reparse_with_access_and_sharing(
            directory,
            name,
            FILE_READ_ATTRIBUTES | FILE_READ_EA | READ_CONTROL | DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )
    }

    /// Pins a published pathname while verifying its complete access policy,
    /// including the audit SACL.
    pub fn open_file_no_reparse_for_security_verification(
        directory: &File,
        name: &Path,
    ) -> io::Result<File> {
        open_file_no_reparse_with_access_and_sharing(
            directory,
            name,
            FILE_READ_ATTRIBUTES | FILE_READ_EA | READ_CONTROL | ACCESS_SYSTEM_SECURITY | DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )
    }

    fn open_file_no_reparse_with_access(
        directory: &File,
        name: &Path,
        access: u32,
    ) -> io::Result<File> {
        open_file_no_reparse_with_access_and_sharing(
            directory,
            name,
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )
    }

    fn open_file_no_reparse_with_access_and_sharing(
        directory: &File,
        name: &Path,
        access: u32,
        share_access: u32,
    ) -> io::Result<File> {
        let file = open_relative(
            directory,
            name,
            access,
            share_access,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            ptr::null(),
        )?;
        let information = file_information(&file)?;
        if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the file is a reparse point",
            ))
        } else if information.dwVolumeSerialNumber
            != file_information(directory)?.dwVolumeSerialNumber
        {
            Err(io::Error::new(
                io::ErrorKind::CrossesDevices,
                "the file is not on the retained directory volume",
            ))
        } else {
            Ok(file)
        }
    }

    /// Reports whether two retained handles identify the same filesystem object.
    pub fn same_file_identity(left: &File, right: &File) -> io::Result<bool> {
        let left = file_information(left)?;
        let right = file_information(right)?;
        Ok(left.dwVolumeSerialNumber == right.dwVolumeSerialNumber
            && left.nFileIndexHigh == right.nFileIndexHigh
            && left.nFileIndexLow == right.nFileIndexLow)
    }

    /// Pins an exact directory object against rename or deletion while a save
    /// publishes through a separate capability handle.
    pub fn lock_directory_path_without_delete_sharing(path: &Path) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let information = file_information(&file)?;
        if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the locked parent is not a regular directory",
            ))
        } else {
            Ok(file)
        }
    }

    /// Returns the filesystem-maintained change time for an exact file handle.
    pub fn file_change_time(file: &File) -> io::Result<i64> {
        let mut information = FILE_BASIC_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as HANDLE,
                FileBasicInfo,
                (&raw mut information).cast(),
                mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(information.ChangeTime)
        }
    }

    /// Creates a directory relative to the exact `directory` handle with a
    /// protected current-user-only DACL and verifies the returned handle.
    pub fn create_current_user_only_directory(directory: &File, name: &Path) -> io::Result<File> {
        let file = with_current_user_only_security(|security| {
            open_relative(
                directory,
                name,
                FILE_ALL_ACCESS | DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_CREATE,
                FILE_DIRECTORY_FILE,
                security,
            )
        })?;
        before_private_object_verification(&file);
        verify_private_object(&file, directory, true)?;
        Ok(file)
    }

    /// Marks the exact retained directory object for deletion after its final
    /// handle closes, without resolving its parent pathname again.
    pub fn delete_directory_by_handle(directory: &File) -> io::Result<()> {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        if unsafe {
            SetFileInformationByHandle(
                directory.as_raw_handle() as HANDLE,
                FileDispositionInfo,
                (&raw const disposition).cast(),
                mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Atomically publishes an open file under a target directory without
    /// replacing an existing target.
    pub fn rename_file_noreplace(
        source: &File,
        target_directory: &File,
        target: &Path,
    ) -> io::Result<()> {
        if target.components().next().is_none()
            || target
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the file name must stay beneath the directory",
            ));
        }
        let target: Vec<u16> = target.as_os_str().encode_wide().collect();
        let header = mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
        let name_bytes = target
            .len()
            .checked_mul(mem::size_of::<u16>())
            .and_then(|length| u32::try_from(length).ok())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let size = header
            .checked_add(name_bytes as usize)
            .map(|size| size.max(mem::size_of::<FILE_RENAME_INFORMATION>()))
            .and_then(|size| u32::try_from(size).ok())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut storage = vec![0usize; (size as usize).div_ceil(mem::size_of::<usize>())];
        let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        unsafe {
            (*information).Anonymous.ReplaceIfExists = false;
            (*information).RootDirectory = target_directory.as_raw_handle() as HANDLE;
            (*information).FileNameLength = name_bytes;
            ptr::copy_nonoverlapping(
                target.as_ptr(),
                ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
                target.len(),
            );
        }
        let mut status = windows_sys::Win32::System::IO::IO_STATUS_BLOCK::default();
        let renamed = unsafe {
            NtSetInformationFile(
                source.as_raw_handle() as HANDLE,
                &raw mut status,
                information.cast(),
                size,
                FileRenameInformation,
            )
        };
        if renamed != STATUS_SUCCESS {
            let error =
                io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(renamed) as i32 });
            if matches!(
                error.raw_os_error(),
                Some(code)
                    if code == ERROR_INVALID_PARAMETER as i32
                        || code == ERROR_NOT_SUPPORTED as i32
            ) {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the filesystem cannot perform a no-overwrite rename",
                ))
            } else {
                Err(error)
            }
        } else {
            Ok(())
        }
    }

    fn token_information(token: HANDLE, information_class: i32) -> io::Result<Vec<usize>> {
        let mut required = 0;
        let _ = unsafe {
            GetTokenInformation(
                token,
                information_class,
                ptr::null_mut(),
                0,
                &raw mut required,
            )
        };
        if required == 0 {
            return Err(io::Error::from_raw_os_error(unsafe {
                GetLastError() as i32
            }));
        }
        let mut storage = vec![0usize; (required as usize).div_ceil(mem::size_of::<usize>())];
        let loaded = unsafe {
            GetTokenInformation(
                token,
                information_class,
                storage.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        };
        if loaded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(storage)
        }
    }

    pub(super) fn process_belongs_to_current_user(process_id: u32) -> io::Result<bool> {
        // SAFETY: querying a PID does not transfer or inherit any caller handle.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcess returned one owned valid handle.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let server_user = process_user(process.as_raw_handle())?;
        // SAFETY: this pseudo-handle is borrowed, never closed.
        let own_user = process_user(unsafe { GetCurrentProcess() })?;
        // SAFETY: both aligned buffers contain TOKEN_USER records returned by
        // GetTokenInformation and remain alive throughout the SID comparison.
        let same_user = unsafe {
            let server = &*server_user.as_ptr().cast::<TOKEN_USER>();
            let own = &*own_user.as_ptr().cast::<TOKEN_USER>();
            EqualSid(server.User.Sid, own.User.Sid) != 0
        };
        Ok(same_user)
    }

    fn process_user(process: HANDLE) -> io::Result<Vec<usize>> {
        let mut token = ptr::null_mut();
        // SAFETY: process is a live borrowed process handle and token is writable.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcessToken returned one owned valid handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        token_information(token.as_raw_handle(), TokenUser)
    }

    fn set_token_default_dacl(
        token: HANDLE,
        information: *const core::ffi::c_void,
    ) -> io::Result<()> {
        let updated = unsafe {
            SetTokenInformation(
                token,
                TokenDefaultDacl,
                information,
                mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
            )
        };
        if updated == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Clears `HANDLE_FLAG_INHERIT` on this process's own stdin/stdout/stderr
    /// handles, if any are set.
    ///
    /// Spawning a child process that redirects its own stdio (even to NUL)
    /// forces Windows to create it with `bInheritHandles = TRUE`, which
    /// duplicates *every* inheritable handle in this process into the
    /// child — not just the three explicitly redirected ones. If this
    /// process's own stdout or stderr was itself piped by its caller (as
    /// `festerm-sessiond start`'s is by fesTerm, which reads the pipe via
    /// `Command::output()`), that pipe's write end is inheritable by
    /// default. A long-lived, deliberately detached grandchild (fesTerm's
    /// persistence daemon) would otherwise inherit a duplicate write
    /// handle to it; since the daemon never exits, that duplicate handle
    /// would keep the pipe open forever and hang the caller's blocking
    /// read. Call this immediately before spawning such a child.
    pub fn disable_std_handle_inheritance() {
        const STD_HANDLES: [STD_HANDLE; 3] =
            [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
        for which in STD_HANDLES {
            let handle = unsafe { GetStdHandle(which) };
            if handle.is_null() || handle as isize == -1 {
                continue;
            }
            let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            fs::{self, OpenOptions},
            io::Write,
            os::windows::fs::OpenOptionsExt,
            path::PathBuf,
            process::Command,
            time::{SystemTime, UNIX_EPOCH},
        };
        use windows_sys::Win32::{
            Foundation::{LocalFree, ERROR_SHARING_VIOLATION, ERROR_SUCCESS},
            Security::{
                AclSizeInformation, AddAuditAccessAceEx,
                Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
                GetAce, GetAclInformation, GetSecurityDescriptorControl, SetSecurityDescriptorSacl,
                WinNetworkServiceSid, ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION,
                DACL_SECURITY_INFORMATION, PROTECTED_SACL_SECURITY_INFORMATION,
                SACL_SECURITY_INFORMATION, SE_DACL_PROTECTED, SE_SACL_PROTECTED, SYSTEM_AUDIT_ACE,
            },
            Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS,
        };

        struct TemporaryDirectory(PathBuf);

        impl TemporaryDirectory {
            fn new() -> Self {
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!(
                    "festerm-windows-security-{}-{nonce}",
                    std::process::id()
                ));
                fs::create_dir(&path).unwrap();
                Self(path)
            }

            fn handle(&self) -> File {
                OpenOptions::new()
                    .read(true)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                    .open(&self.0)
                    .unwrap()
            }
        }

        impl Drop for TemporaryDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn security_snapshot_keeps_only_settable_file_attributes() {
            assert_eq!(
                settable_file_attributes(u32::MAX),
                FILE_ATTRIBUTE_VALID_SET_FLAGS
            );
            assert_eq!(settable_file_attributes(0x0000_0E10), 0);
            assert!(file_is_encrypted(FILE_ATTRIBUTE_ENCRYPTED));
            assert!(!file_is_encrypted(FILE_ATTRIBUTE_NORMAL));
            assert_eq!(
                unsupported_integrity_attributes(
                    FILE_ATTRIBUTE_INTEGRITY_STREAM_FLAG | FILE_ATTRIBUTE_NO_SCRUB_DATA_FLAG
                ),
                UNSUPPORTED_INTEGRITY_ATTRIBUTES
            );
        }

        #[test]
        fn extended_attribute_probe_distinguishes_absent_present_and_failed_queries() {
            assert_eq!(
                extended_attribute_query_result(STATUS_NO_EAS_ON_FILE, 0),
                Ok(false)
            );
            assert_eq!(
                extended_attribute_query_result(STATUS_NO_MORE_EAS, 0),
                Ok(false)
            );
            assert_eq!(
                extended_attribute_query_result(STATUS_SUCCESS, 0),
                Ok(false)
            );
            assert_eq!(extended_attribute_query_result(STATUS_SUCCESS, 1), Ok(true));
            assert_eq!(
                extended_attribute_query_result(STATUS_BUFFER_OVERFLOW, 0),
                Ok(true)
            );
            assert_eq!(
                extended_attribute_query_result(STATUS_BUFFER_TOO_SMALL, 0),
                Ok(true)
            );
            assert!(extended_attribute_query_result(STATUS_INFO_LENGTH_MISMATCH, 0).is_err());
        }

        #[test]
        fn ordinary_temp_file_has_no_extended_attributes() {
            let directory = TemporaryDirectory::new();
            let path = directory.0.join("ordinary.txt");
            fs::write(&path, b"ordinary").unwrap();
            let file = File::open(path).unwrap();

            assert!(!file_has_extended_attributes(&file).unwrap());
        }

        fn unsupported_metadata(reason: UnsupportedSecurityMetadata) -> SecurityMetadata {
            SecurityMetadata {
                descriptor: Vec::new(),
                audit_sacl: None,
                mandatory_label: Vec::new(),
                resource_attributes: Vec::new(),
                scoped_policy: Vec::new(),
                attributes: FILE_ATTRIBUTE_NORMAL,
                unsupported_integrity_attributes: if reason
                    == UnsupportedSecurityMetadata::IntegrityPolicy
                {
                    FILE_ATTRIBUTE_INTEGRITY_STREAM_FLAG
                } else {
                    0
                },
                encrypted: reason == UnsupportedSecurityMetadata::EfsEncryption,
                has_named_streams: reason == UnsupportedSecurityMetadata::NamedStreams,
                has_extended_attributes: reason == UnsupportedSecurityMetadata::ExtendedAttributes,
            }
        }

        #[test]
        fn unsupported_security_metadata_is_refused_before_application() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let file =
                create_current_user_only_file(&directory_handle, Path::new("target.md")).unwrap();
            for reason in [
                UnsupportedSecurityMetadata::EfsEncryption,
                UnsupportedSecurityMetadata::NamedStreams,
                UnsupportedSecurityMetadata::IntegrityPolicy,
                UnsupportedSecurityMetadata::ExtendedAttributes,
            ] {
                let metadata = unsupported_metadata(reason);
                assert_eq!(metadata.unsupported_reason(), Some(reason));
                let error = apply_security_metadata(&file, &metadata).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            }
        }

        #[test]
        fn security_privilege_is_scoped_to_the_saving_thread() {
            let directory = TemporaryDirectory::new();
            fs::write(directory.0.join("source.md"), b"source").unwrap();
            let directory_handle = directory.handle();
            let other_directory = directory_handle.try_clone().unwrap();
            let _privilege = enable_security_privilege().unwrap();

            open_file_no_reparse_for_security_capture(&directory_handle, Path::new("source.md"))
                .unwrap();
            let error = std::thread::spawn(move || {
                open_file_no_reparse_for_security_capture(&other_directory, Path::new("source.md"))
                    .unwrap_err()
            })
            .join()
            .unwrap();

            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }

        #[test]
        fn security_copy_preserves_an_explicit_mandatory_integrity_label() {
            let _privilege = enable_security_privilege().unwrap();
            let directory = TemporaryDirectory::new();
            let source_path = directory.0.join("source.md");
            fs::write(&source_path, b"source").unwrap();
            let status = Command::new("icacls")
                .arg(&source_path)
                .args(["/setintegritylevel", "L"])
                .status()
                .unwrap();
            assert!(status.success());
            let directory_handle = directory.handle();
            let source = open_file_no_reparse_for_security_capture(
                &directory_handle,
                Path::new("source.md"),
            )
            .unwrap();
            let target = create_current_user_only_file_exclusive_with_audit(
                &directory_handle,
                Path::new("target.md"),
            )
            .unwrap();
            let metadata = security_metadata(&source).unwrap();

            apply_security_metadata(&target, &metadata).unwrap();

            assert!(security_metadata_matches(&target, &metadata).unwrap());
        }

        #[test]
        fn security_copy_preserves_an_explicit_audit_sacl() {
            let _privilege = enable_security_privilege().unwrap();
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let source = create_current_user_only_file_exclusive_with_audit(
                &directory_handle,
                Path::new("source.md"),
            )
            .unwrap();
            set_current_user_audit_sacl(&source);
            let target = create_current_user_only_file_exclusive_with_audit(
                &directory_handle,
                Path::new("target.md"),
            )
            .unwrap();
            let metadata = security_metadata(&source).unwrap();

            apply_security_metadata(&target, &metadata).unwrap();

            assert!(security_metadata_matches(&target, &metadata).unwrap());
        }

        fn set_current_user_audit_sacl(file: &File) {
            let mut token = ptr::null_mut();
            assert_ne!(
                unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) },
                0
            );
            let token = unsafe { OwnedHandle::from_raw_handle(token) };
            let user = token_information(token.as_raw_handle(), TokenUser).unwrap();
            let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
            let sid_length = unsafe { GetLengthSid(user.User.Sid) };
            assert_ne!(sid_length, 0);
            let acl_bytes = mem::size_of::<ACL>() + mem::size_of::<SYSTEM_AUDIT_ACE>()
                - mem::size_of::<u32>()
                + sid_length as usize;
            let mut acl_storage = vec![0usize; acl_bytes.div_ceil(mem::size_of::<usize>())];
            let acl = acl_storage.as_mut_ptr().cast::<ACL>();
            assert_ne!(
                unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) },
                0
            );
            assert_ne!(
                unsafe {
                    AddAuditAccessAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, user.User.Sid, 1, 1)
                },
                0
            );
            let mut descriptor = SECURITY_DESCRIPTOR::default();
            assert_ne!(
                unsafe { InitializeSecurityDescriptor((&raw mut descriptor).cast(), 1) },
                0
            );
            assert_ne!(
                unsafe { SetSecurityDescriptorSacl((&raw mut descriptor).cast(), 1, acl, 0) },
                0
            );
            assert_ne!(
                unsafe {
                    SetSecurityDescriptorControl(
                        (&raw mut descriptor).cast(),
                        SE_SACL_PROTECTED,
                        SE_SACL_PROTECTED,
                    )
                },
                0
            );
            assert_ne!(
                unsafe {
                    SetKernelObjectSecurity(
                        file.as_raw_handle() as HANDLE,
                        SACL_SECURITY_INFORMATION | PROTECTED_SACL_SECURITY_INFORMATION,
                        (&raw mut descriptor).cast(),
                    )
                },
                0
            );
        }

        #[test]
        fn alternate_data_streams_are_detected_and_refused() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let mut target =
                create_current_user_only_file(&directory_handle, Path::new("target.md")).unwrap();
            target.write_all(b"content").unwrap();
            target.sync_all().unwrap();
            fs::write(
                directory.0.join("target.md:Zone.Identifier"),
                b"[ZoneTransfer]\r\nZoneId=3\r\n",
            )
            .unwrap();
            let metadata = security_metadata(&target).unwrap();
            assert!(metadata.has_named_streams());
            let replacement =
                create_current_user_only_file(&directory_handle, Path::new("replacement.md"))
                    .unwrap();

            let error = apply_security_metadata(&replacement, &metadata).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        }

        #[test]
        fn staging_parent_rejects_a_dacl_that_allows_child_substitution() {
            let directory = TemporaryDirectory::new();
            let directory_handle = OpenOptions::new()
                .access_mode(GENERIC_READ | WRITE_DAC)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(&directory.0)
                .unwrap();
            assert!(secure_staging_parent(&directory_handle).unwrap().is_some());

            make_unprotected(&directory_handle);

            assert!(secure_staging_parent(&directory_handle).unwrap().is_none());
        }

        #[test]
        fn staging_parent_trust_excludes_other_well_known_service_owners() {
            let mut token = ptr::null_mut();
            assert_ne!(
                unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) },
                0
            );
            let token = unsafe { OwnedHandle::from_raw_handle(token) };
            let user = token_information(token.as_raw_handle(), TokenUser).unwrap();
            let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
            let system = well_known_sid(WinLocalSystemSid).unwrap();
            let administrators = well_known_sid(WinBuiltinAdministratorsSid).unwrap();
            let network_service = well_known_sid(WinNetworkServiceSid).unwrap();

            assert!(!trusted_parent_sid(
                network_service.as_ptr().cast_mut().cast(),
                user.User.Sid,
                &system,
                &administrators,
            ));
        }

        fn make_unprotected(file: &File) {
            let mut descriptor = SECURITY_DESCRIPTOR::default();
            assert_ne!(
                unsafe { InitializeSecurityDescriptor((&raw mut descriptor).cast(), 1) },
                0
            );
            assert_ne!(
                unsafe {
                    SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, ptr::null_mut(), 0)
                },
                0
            );
            assert_ne!(
                unsafe {
                    SetKernelObjectSecurity(
                        file.as_raw_handle() as HANDLE,
                        DACL_SECURITY_INFORMATION,
                        (&raw mut descriptor).cast(),
                    )
                },
                0
            );
        }

        #[test]
        fn pathname_substitution_cannot_replace_the_native_created_directory_handle() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let staging = directory.0.join("private.stage");
            let stolen = directory.0.join("stolen.stage");
            let staging_handle =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();
            fs::rename(&staging, &stolen).unwrap();
            fs::create_dir(&staging).unwrap();

            let child =
                create_current_user_only_file(&staging_handle, Path::new("unwritten.tmp")).unwrap();

            assert_current_user_only_dacl(&staging_handle);
            assert_current_user_only_dacl(&child);
            assert_eq!(child.metadata().unwrap().len(), 0);
            assert!(stolen.join("unwritten.tmp").is_file());
            assert!(!staging.join("unwritten.tmp").exists());
        }

        #[test]
        fn handle_bound_deletion_removes_only_the_retained_directory_object() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let staging = directory.0.join("private.stage");
            let moved = directory.0.join("moved.stage");
            let staging_handle =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();
            fs::rename(&staging, &moved).unwrap();
            fs::create_dir(&staging).unwrap();

            delete_directory_by_handle(&staging_handle).unwrap();
            drop(staging_handle);

            assert!(!moved.exists());
            assert!(staging.is_dir());
        }

        #[test]
        fn directory_path_lock_prevents_parent_rename_until_release() {
            let directory = TemporaryDirectory::new();
            let parent = directory.0.join("parent");
            let moved = directory.0.join("moved");
            fs::create_dir(&parent).unwrap();
            let lock = lock_directory_path_without_delete_sharing(&parent).unwrap();

            let error = fs::rename(&parent, &moved).unwrap_err();
            assert!(matches!(
                error.raw_os_error(),
                Some(code) if code == ERROR_SHARING_VIOLATION as i32
            ));

            drop(lock);
            fs::rename(&parent, &moved).unwrap();
        }

        #[test]
        fn unprotected_created_directory_is_rejected_before_child_creation() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            BEFORE_PRIVATE_OBJECT_VERIFICATION.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(make_unprotected));
            });

            let result =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"));

            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            assert!(
                fs::read_dir(directory.0.join("private.stage"))
                    .unwrap()
                    .next()
                    .is_none(),
                "a rejected staging handle must never be used to create a child"
            );
        }

        #[test]
        fn unprotected_created_child_is_rejected_before_caller_bytes_can_be_written() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let staging_handle =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();
            BEFORE_PRIVATE_OBJECT_VERIFICATION.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(make_unprotected));
            });

            let result = create_current_user_only_file(&staging_handle, Path::new("unwritten.tmp"));

            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                fs::metadata(directory.0.join("private.stage/unwritten.tmp"))
                    .unwrap()
                    .len(),
                0,
                "verification must fail before a caller can receive a writable handle"
            );
        }

        #[test]
        fn exclusive_private_file_blocks_other_readers_after_its_dacl_is_broadened() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let staging_handle =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();
            let staged =
                create_current_user_only_file_exclusive(&staging_handle, Path::new("payload"))
                    .unwrap();
            make_unprotected(&staged);

            let error = File::open(directory.0.join("private.stage/payload")).unwrap_err();

            assert_eq!(
                error.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION as i32),
                "the staged payload must remain unreadable through a second handle"
            );
            restrict_to_current_user(&staged).unwrap();
        }

        #[test]
        fn exclusive_private_file_uses_a_transient_move_and_blocks_later_delete_access() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let staging_handle =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();
            let staged =
                create_current_user_only_file_exclusive(&staging_handle, Path::new("payload"))
                    .unwrap();
            let expected = security_metadata(&staged).unwrap();
            let mover =
                open_file_no_reparse_for_move(&staging_handle, Path::new("payload")).unwrap();
            assert!(same_file_identity(&staged, &mover).unwrap());
            rename_file_noreplace(&mover, &directory_handle, Path::new("published.md")).unwrap();
            drop(mover);

            let verification =
                open_file_no_reparse_for_verification(&directory_handle, Path::new("published.md"))
                    .unwrap();

            assert!(same_file_identity(&staged, &verification).unwrap());
            assert!(security_metadata_matches(&verification, &expected).unwrap());
            let error = open_file_no_reparse_for_move(&directory_handle, Path::new("published.md"))
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        }

        #[test]
        fn conditional_publication_follows_the_retained_directory_and_refuses_collisions() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let staging =
                create_current_user_only_directory(&directory_handle, Path::new("stage")).unwrap();
            let destination =
                create_current_user_only_directory(&directory_handle, Path::new("destination"))
                    .unwrap();
            let retained_path = directory.0.join("retained");
            fs::rename(directory.0.join("destination"), &retained_path).unwrap();
            fs::create_dir(directory.0.join("destination")).unwrap();
            fs::write(directory.0.join("stage/payload"), b"published").unwrap();
            let mover = open_file_no_reparse_for_move(&staging, Path::new("payload")).unwrap();

            rename_file_noreplace(&mover, &destination, Path::new("x")).unwrap();

            assert_eq!(fs::read(retained_path.join("x")).unwrap(), b"published");
            assert!(!directory.0.join("destination/x").exists());
            fs::write(directory.0.join("stage/collision"), b"later").unwrap();
            let collision =
                open_file_no_reparse_for_move(&staging, Path::new("collision")).unwrap();
            let error =
                rename_file_noreplace(&collision, &destination, Path::new("x")).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(retained_path.join("x")).unwrap(), b"published");
            assert_eq!(
                fs::read(directory.0.join("stage/collision")).unwrap(),
                b"later"
            );
            assert!(!directory.0.join("destination/x").exists());
        }

        #[test]
        fn captured_file_uses_transient_moves_and_a_final_pathname_lock() {
            let directory = TemporaryDirectory::new();
            fs::write(directory.0.join("original.md"), b"original").unwrap();
            let directory_handle = directory.handle();
            let captured =
                open_file_no_reparse_for_capture(&directory_handle, Path::new("original.md"))
                    .unwrap();
            let mover =
                open_file_no_reparse_for_move(&directory_handle, Path::new("original.md")).unwrap();
            assert!(same_file_identity(&captured, &mover).unwrap());

            let staging_handle =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();
            rename_file_noreplace(&mover, &staging_handle, Path::new("displaced")).unwrap();
            drop(mover);
            restrict_to_current_user(&captured).unwrap();

            let restorer =
                open_file_no_reparse_for_move(&staging_handle, Path::new("displaced")).unwrap();
            assert!(same_file_identity(&captured, &restorer).unwrap());
            rename_file_noreplace(&restorer, &directory_handle, Path::new("original.md")).unwrap();
            drop(restorer);
            let lock =
                open_file_no_reparse_for_verification(&directory_handle, Path::new("original.md"))
                    .unwrap();
            assert!(same_file_identity(&captured, &lock).unwrap());
            let error = open_file_no_reparse_for_move(&directory_handle, Path::new("original.md"))
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        }

        #[test]
        fn security_capture_can_privatize_and_restore_a_displaced_original() {
            let directory = TemporaryDirectory::new();
            fs::write(directory.0.join("original.md"), b"original").unwrap();
            let directory_handle = directory.handle();
            let captured = open_file_no_reparse_for_capture_with_access(
                &directory_handle,
                Path::new("original.md"),
                0,
            )
            .unwrap();
            let information =
                OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
            let original_descriptor = security_descriptor(&captured, information).unwrap();
            let original_attributes = file_information(&captured).unwrap().dwFileAttributes;
            let staging =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();
            let mover =
                open_file_no_reparse_for_move(&directory_handle, Path::new("original.md")).unwrap();
            assert!(same_file_identity(&captured, &mover).unwrap());
            rename_file_noreplace(&mover, &staging, Path::new("displaced")).unwrap();
            drop(mover);

            restrict_to_current_user(&captured).unwrap();
            assert!(is_current_user_only(&captured).unwrap());
            if unsafe {
                SetKernelObjectSecurity(
                    captured.as_raw_handle() as HANDLE,
                    information,
                    original_descriptor.as_ptr().cast_mut().cast(),
                )
            } == 0
            {
                panic!(
                    "original access restoration failed: {}",
                    io::Error::last_os_error()
                );
            }
            assert_eq!(
                security_descriptor(&captured, information).unwrap(),
                original_descriptor
            );
            let attributes = FILE_BASIC_INFO {
                FileAttributes: settable_file_attributes(original_attributes),
                ..FILE_BASIC_INFO::default()
            };
            if unsafe {
                SetFileInformationByHandle(
                    captured.as_raw_handle() as HANDLE,
                    FileBasicInfo,
                    (&raw const attributes).cast(),
                    mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            } == 0
            {
                panic!(
                    "original attributes restoration failed: {}",
                    io::Error::last_os_error()
                );
            }
            assert_eq!(
                settable_file_attributes(file_information(&captured).unwrap().dwFileAttributes),
                settable_file_attributes(original_attributes)
            );

            let restorer = open_file_no_reparse_for_move(&staging, Path::new("displaced")).unwrap();
            assert!(same_file_identity(&captured, &restorer).unwrap());
            rename_file_noreplace(&restorer, &directory_handle, Path::new("original.md")).unwrap();
            drop(restorer);
            let locked =
                open_file_no_reparse_for_verification(&directory_handle, Path::new("original.md"))
                    .unwrap();
            assert!(same_file_identity(&captured, &locked).unwrap());
            let error = open_file_no_reparse_for_move(&directory_handle, Path::new("original.md"))
                .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
            assert_eq!(
                fs::read(directory.0.join("original.md")).unwrap(),
                b"original"
            );
        }

        #[test]
        fn private_creation_and_conditional_publication_keep_the_current_user_only_dacl() {
            let _privilege = enable_security_privilege().unwrap();
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let staging_handle =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();
            let staging = TemporaryDirectory(directory.0.join("private.stage"));
            assert_current_user_only_dacl(&staging_handle);
            restrict_to_current_user(&staging_handle).unwrap();
            let mut unpublished =
                create_current_user_only_file(&staging_handle, Path::new("unpublished.tmp"))
                    .unwrap();
            unpublished.write_all(b"new").unwrap();
            unpublished.sync_all().unwrap();
            rename_file_noreplace(&unpublished, &directory_handle, Path::new("published.md"))
                .unwrap();
            assert_eq!(fs::read(directory.0.join("published.md")).unwrap(), b"new");

            let collision =
                create_current_user_only_file(&staging_handle, Path::new("collision.tmp")).unwrap();
            assert!(rename_file_noreplace(
                &collision,
                &directory_handle,
                Path::new("published.md")
            )
            .is_err());
            assert_eq!(fs::read(directory.0.join("published.md")).unwrap(), b"new");

            let mut target =
                create_current_user_only_file_with_audit(&directory_handle, Path::new("target.md"))
                    .unwrap();
            target.write_all(b"before").unwrap();
            target.sync_all().unwrap();
            assert_current_user_only_dacl(&target);
            let target_security = security_metadata(&target).unwrap();

            let mut replacement = create_current_user_only_file_with_audit(
                &staging_handle,
                Path::new("replacement.tmp"),
            )
            .unwrap();
            replacement.write_all(b"after").unwrap();
            replacement.sync_all().unwrap();
            apply_security_metadata(&replacement, &target_security).unwrap();
            assert!(security_metadata_matches(&replacement, &target_security).unwrap());
            rename_file_noreplace(&target, &staging_handle, Path::new("original")).unwrap();
            rename_file_noreplace(&replacement, &directory_handle, Path::new("target.md")).unwrap();
            drop(replacement);

            let target = open_file_no_reparse_for_security_verification(
                &directory_handle,
                Path::new("target.md"),
            )
            .unwrap();
            assert_current_user_only_dacl(&target);
            assert!(security_metadata_matches(&target, &target_security).unwrap());
            assert!(open_file_no_reparse(&directory_handle, Path::new("target.md")).is_ok());
            assert_eq!(fs::read(directory.0.join("target.md")).unwrap(), b"after");
            assert_eq!(fs::read(staging.0.join("original")).unwrap(), b"before");
        }

        fn assert_current_user_only_dacl(file: &File) {
            let mut dacl = ptr::null_mut();
            let mut descriptor = ptr::null_mut();
            let status = unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &raw mut dacl,
                    ptr::null_mut(),
                    &raw mut descriptor,
                )
            };
            assert_eq!(status, ERROR_SUCCESS);

            let mut control = 0;
            let mut revision = 0;
            assert_ne!(
                unsafe {
                    GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision)
                },
                0
            );
            assert_ne!(control & SE_DACL_PROTECTED, 0);

            let mut information = ACL_SIZE_INFORMATION::default();
            assert_ne!(
                unsafe {
                    GetAclInformation(
                        dacl,
                        (&raw mut information).cast(),
                        mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                        AclSizeInformation,
                    )
                },
                0
            );
            assert_eq!(information.AceCount, 1);

            let mut ace = ptr::null_mut();
            assert_ne!(unsafe { GetAce(dacl, 0, &raw mut ace) }, 0);
            let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };

            let mut token = ptr::null_mut();
            assert_ne!(
                unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) },
                0
            );
            let token = unsafe { OwnedHandle::from_raw_handle(token) };
            let user = token_information(token.as_raw_handle(), TokenUser).unwrap();
            let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
            assert_ne!(
                unsafe { EqualSid((&raw const ace.SidStart).cast_mut().cast(), user.User.Sid) },
                0
            );
            assert!(current_user_only_security(file, user.User.Sid).unwrap());
            let _ = unsafe { LocalFree(descriptor) };
        }
    }
}

#[cfg(windows)]
pub use imp::{
    apply_security_metadata, create_current_user_only_directory, create_current_user_only_file,
    create_current_user_only_file_exclusive, create_current_user_only_file_exclusive_with_audit,
    create_current_user_only_file_with_audit, delete_directory_by_handle,
    disable_std_handle_inheritance, enable_security_privilege, file_change_time,
    is_current_user_only, lock_directory_path_without_delete_sharing, open_file_no_reparse,
    open_file_no_reparse_for_capture, open_file_no_reparse_for_move,
    open_file_no_reparse_for_security_capture, open_file_no_reparse_for_security_verification,
    open_file_no_reparse_for_verification, rename_file_noreplace, reopen_file_for_identity,
    restrict_default_dacl_to_current_user, restrict_to_current_user, same_file_identity,
    secure_staging_parent, security_metadata, security_metadata_matches, staging_parent_matches,
    DefaultDaclGuard, SecurityMetadata, SecurityPrivilegeGuard, StagingParentSecurity,
    UnsupportedSecurityMetadata,
};
