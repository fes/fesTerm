//! Safe ownership around Win32 object security and local named-pipe operations.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
pub mod named_pipe;

#[cfg(windows)]
mod imp {
    use std::{
        fs::File,
        io, mem,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::Path,
        ptr,
    };

    use windows_sys::Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FileStreamInformation, NtCreateFile, NtQueryInformationFile, FILE_CREATE,
            FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_STREAM_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, LocalFree, RtlNtStatusToDosError, SetHandleInformation,
            ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_SUCCESS, GENERIC_ALL, GENERIC_READ,
            HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE,
            STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL, STATUS_INFO_LENGTH_MISMATCH,
            STATUS_SUCCESS, UNICODE_STRING,
        },
        Security::{
            AclSizeInformation, AddAccessAllowedAceEx,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            EqualSid, GetAce, GetAclInformation, GetKernelObjectSecurity, GetLengthSid,
            GetSecurityDescriptorControl, GetTokenInformation, InitializeAcl,
            InitializeSecurityDescriptor, SetKernelObjectSecurity, SetSecurityDescriptorControl,
            SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, SetTokenInformation,
            TokenDefaultDacl, TokenUser, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION,
            ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
            SECURITY_DESCRIPTOR, SE_DACL_PROTECTED, TOKEN_ADJUST_DEFAULT, TOKEN_DEFAULT_DACL,
            TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{
            FileBasicInfo, FileRenameInfoEx, GetFileInformationByHandle,
            SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ALL_ACCESS,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
            READ_CONTROL, SYNCHRONIZE, WRITE_DAC, WRITE_OWNER,
        },
        System::{
            Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
            Threading::{
                GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
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

    /// Access-control and attribute metadata that replacement must preserve.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct SecurityMetadata {
        descriptor: Vec<u8>,
        attributes: u32,
        encrypted: bool,
        has_named_streams: bool,
    }

    const FILE_ATTRIBUTE_VALID_SET_FLAGS: u32 = 0x0000_31A7;

    fn settable_file_attributes(attributes: u32) -> u32 {
        attributes & FILE_ATTRIBUTE_VALID_SET_FLAGS
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

    /// Snapshots owner, group, DACL, and file attributes through an open handle.
    pub fn security_metadata(file: &File) -> io::Result<SecurityMetadata> {
        const INFORMATION: u32 =
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        const MAX_DESCRIPTOR_SIZE: u32 = 8 * 1024 * 1024;

        let handle = file.as_raw_handle() as HANDLE;
        let mut required = 0;
        let _ = unsafe {
            GetKernelObjectSecurity(handle, INFORMATION, ptr::null_mut(), 0, &raw mut required)
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
                INFORMATION,
                descriptor.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        descriptor.truncate(required as usize);

        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle, &raw mut information) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SecurityMetadata {
            descriptor,
            attributes: settable_file_attributes(information.dwFileAttributes),
            encrypted: file_is_encrypted(information.dwFileAttributes),
            has_named_streams: file_has_named_streams(file)?,
        })
    }

    /// Reports whether a handle still has the captured security metadata.
    pub fn security_metadata_matches(file: &File, expected: &SecurityMetadata) -> io::Result<bool> {
        security_metadata(file).map(|current| current == *expected)
    }

    /// Applies captured owner, group, DACL, and file attributes to a prepared
    /// replacement before it can become visible.
    pub fn apply_security_metadata(file: &File, metadata: &SecurityMetadata) -> io::Result<()> {
        const INFORMATION: u32 =
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        if metadata.encrypted {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "NTFS EFS encryption cannot be preserved through basic file metadata",
            ));
        }
        if metadata.has_named_streams {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "NTFS alternate data streams cannot be preserved through basic file metadata",
            ));
        }
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
        if security_metadata_matches(file, metadata)? {
            Ok(())
        } else {
            Err(io::Error::other(
                "prepared Windows security metadata did not match its source",
            ))
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

    /// Retains a no-follow file for generation, content, and security checks
    /// without holding delete access; a separate exact move handle can coexist.
    pub fn open_file_no_reparse_for_capture(directory: &File, name: &Path) -> io::Result<File> {
        open_file_no_reparse_with_access(
            directory,
            name,
            GENERIC_READ | FILE_WRITE_ATTRIBUTES | WRITE_DAC | WRITE_OWNER,
        )
    }

    /// Opens an exact staged file with only the access needed to move it.
    pub fn open_file_no_reparse_for_move(directory: &File, name: &Path) -> io::Result<File> {
        open_file_no_reparse_with_access(directory, name, FILE_READ_ATTRIBUTES | DELETE)
    }

    /// Opens a pathname for identity and security verification without
    /// requesting data access from an exclusively retained save payload.
    pub fn open_file_no_reparse_for_verification(
        directory: &File,
        name: &Path,
    ) -> io::Result<File> {
        open_file_no_reparse_with_access_and_sharing(
            directory,
            name,
            FILE_READ_ATTRIBUTES | READ_CONTROL,
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
        let header = mem::offset_of!(FILE_RENAME_INFO, FileName);
        let name_bytes = target
            .len()
            .checked_mul(mem::size_of::<u16>())
            .and_then(|length| u32::try_from(length).ok())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let size = header
            .checked_add(name_bytes as usize)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut storage = vec![0usize; size.div_ceil(mem::size_of::<usize>())];
        let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*information).Anonymous.Flags = 0;
            (*information).RootDirectory = target_directory.as_raw_handle() as HANDLE;
            (*information).FileNameLength = name_bytes;
            ptr::copy_nonoverlapping(
                target.as_ptr(),
                ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
                target.len(),
            );
        }
        let renamed = unsafe {
            SetFileInformationByHandle(
                source.as_raw_handle() as HANDLE,
                FileRenameInfoEx,
                information.cast(),
                size as u32,
            )
        };
        if renamed == 0 {
            let error = io::Error::last_os_error();
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
            time::{SystemTime, UNIX_EPOCH},
        };
        use windows_sys::Win32::{
            Foundation::{LocalFree, ERROR_SHARING_VIOLATION, ERROR_SUCCESS},
            Security::{
                AclSizeInformation,
                Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
                GetAce, GetAclInformation, GetSecurityDescriptorControl, ACCESS_ALLOWED_ACE,
                ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
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
        }

        #[test]
        fn encrypted_security_metadata_is_refused_instead_of_applied_as_plaintext() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let file =
                create_current_user_only_file(&directory_handle, Path::new("target.md")).unwrap();
            let encrypted = SecurityMetadata {
                descriptor: Vec::new(),
                attributes: FILE_ATTRIBUTE_NORMAL,
                encrypted: true,
                has_named_streams: false,
            };

            let error = apply_security_metadata(&file, &encrypted).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
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
        fn private_creation_and_conditional_publication_keep_the_current_user_only_dacl() {
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
                create_current_user_only_file(&directory_handle, Path::new("target.md")).unwrap();
            target.write_all(b"before").unwrap();
            target.sync_all().unwrap();
            assert_current_user_only_dacl(&target);
            let target_security = security_metadata(&target).unwrap();

            let mut replacement =
                create_current_user_only_file(&staging_handle, Path::new("replacement.tmp"))
                    .unwrap();
            replacement.write_all(b"after").unwrap();
            replacement.sync_all().unwrap();
            apply_security_metadata(&replacement, &target_security).unwrap();
            assert!(security_metadata_matches(&replacement, &target_security).unwrap());
            rename_file_noreplace(&target, &staging_handle, Path::new("original")).unwrap();
            rename_file_noreplace(&replacement, &directory_handle, Path::new("target.md")).unwrap();

            let target = File::open(directory.0.join("target.md")).unwrap();
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
    create_current_user_only_file_exclusive, disable_std_handle_inheritance, is_current_user_only,
    open_file_no_reparse, open_file_no_reparse_for_capture, open_file_no_reparse_for_move,
    open_file_no_reparse_for_verification, rename_file_noreplace,
    restrict_default_dacl_to_current_user, restrict_to_current_user, same_file_identity,
    security_metadata, security_metadata_matches, DefaultDaclGuard, SecurityMetadata,
};
