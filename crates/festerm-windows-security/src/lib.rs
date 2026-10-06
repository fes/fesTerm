//! Safe ownership around Win32 object security and local named-pipe operations.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
pub mod named_pipe;

#[cfg(windows)]
mod imp {
    use std::{
        ffi::OsString,
        fs::File,
        io, mem,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::{Path, PathBuf},
        ptr,
    };

    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, LocalFree, SetHandleInformation, ERROR_SUCCESS, GENERIC_ALL,
            GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        },
        Security::{
            AclSizeInformation, AddAccessAllowedAceEx,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            EqualSid, GetAce, GetAclInformation, GetKernelObjectSecurity, GetLengthSid,
            GetSecurityDescriptorControl, GetTokenInformation, InitializeAcl,
            InitializeSecurityDescriptor, SetKernelObjectSecurity, SetSecurityDescriptorControl,
            SetSecurityDescriptorDacl, SetTokenInformation, TokenDefaultDacl, TokenUser,
            ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
            GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
            SE_DACL_PROTECTED, TOKEN_ADJUST_DEFAULT, TOKEN_DEFAULT_DACL, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, FileBasicInfo, FileRenameInfoEx,
            GetFileInformationByHandle, GetFinalPathNameByHandleW, SetFileInformationByHandle,
            BY_HANDLE_FILE_INFORMATION, CREATE_NEW, DELETE, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED, FILE_RENAME_INFO,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, VOLUME_NAME_DOS,
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
    thread_local! {
        static AFTER_PRIVATE_DIRECTORY_CREATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    fn after_private_directory_creation() {
        #[cfg(test)]
        AFTER_PRIVATE_DIRECTORY_CREATION.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook();
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
    }

    const FILE_ATTRIBUTE_VALID_SET_FLAGS: u32 = 0x0000_31A7;

    fn settable_file_attributes(attributes: u32) -> u32 {
        attributes & FILE_ATTRIBUTE_VALID_SET_FLAGS
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
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                (&raw mut descriptor).cast(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if current_user_only_dacl(file, token_user.User.Sid)? {
            Ok(())
        } else {
            Err(io::Error::other(
                "object DACL is not protected current-user-only access",
            ))
        }
    }

    fn current_user_only_dacl(file: &File, user_sid: *mut core::ffi::c_void) -> io::Result<bool> {
        const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
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
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let verified = (|| {
            if dacl.is_null() {
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

    /// Creates a new file beneath `directory` with a protected current-user-only
    /// DACL, before any caller bytes can be written. Because Win32 does not
    /// accept a directory handle as `CreateFileW`'s root, the caller must
    /// re-open `name` through its retained directory capability and compare
    /// file identity before writing.
    pub fn create_current_user_only_file(directory: &File, name: &Path) -> io::Result<File> {
        if name.components().count() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the private file name must be one path component",
            ));
        }

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

        let acl_bytes = mem::size_of::<ACL>()
            + mem::size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()
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
        if unsafe { InitializeSecurityDescriptor((&raw mut descriptor).cast(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, acl, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
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
        let path = directory_child_path(directory, name)?;
        let wide = wide_path(&path);
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_ALL_ACCESS | DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &raw const attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_handle(handle) })
        }
    }

    /// Opens a file beneath a retained directory without traversing a final
    /// reparse point.
    pub fn open_file_no_reparse(directory: &File, name: &Path) -> io::Result<File> {
        open_file_no_reparse_with_access(directory, name, GENERIC_READ)
    }

    /// Opens a no-follow file handle that can be moved into private recovery
    /// storage.
    pub fn open_file_no_reparse_for_rename(directory: &File, name: &Path) -> io::Result<File> {
        open_file_no_reparse_with_access(directory, name, GENERIC_READ | DELETE)
    }

    fn open_file_no_reparse_with_access(
        directory: &File,
        name: &Path,
        access: u32,
    ) -> io::Result<File> {
        let path = wide_path(&directory_child_path(directory, name)?);
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_handle(handle) };
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &raw mut information)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the file is a reparse point",
            ))
        } else {
            Ok(file)
        }
    }

    /// Creates a directory with a protected current-user-only DACL.
    pub fn create_current_user_only_directory(directory: &File, name: &Path) -> io::Result<File> {
        if name.components().count() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the private directory name must be one path component",
            ));
        }
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

        let acl_bytes = mem::size_of::<ACL>()
            + mem::size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()
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
        if unsafe { InitializeSecurityDescriptor((&raw mut descriptor).cast(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, acl, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
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
        let path = wide_path(&directory_child_path(directory, name)?);
        if unsafe { CreateDirectoryW(path.as_ptr(), &raw const attributes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        after_private_directory_creation();
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                FILE_ALL_ACCESS,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_handle(handle) };
        restrict_to_current_user(&file)?;
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
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn directory_child_path(directory: &File, name: &Path) -> io::Result<PathBuf> {
        if name.components().next().is_none()
            || name
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the file name must stay beneath the directory",
            ));
        }
        let handle = directory.as_raw_handle();
        let required = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                ptr::null_mut(),
                0,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if required == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u16; required as usize + 1];
        let written = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if written == 0 || written as usize >= buffer.len() {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(written as usize);
        Ok(PathBuf::from(OsString::from_wide(&buffer)).join(name))
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
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
            time::{SystemTime, UNIX_EPOCH},
        };
        use windows_sys::Win32::{
            Foundation::{LocalFree, ERROR_SUCCESS},
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
        }

        #[test]
        fn substituted_private_directory_is_restricted_before_return() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let staging = directory.0.join("private.stage");
            let stolen = directory.0.join("stolen.stage");
            AFTER_PRIVATE_DIRECTORY_CREATION.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    fs::rename(&staging, &stolen).unwrap();
                    fs::create_dir(&staging).unwrap();
                }));
            });

            let staging =
                create_current_user_only_directory(&directory_handle, Path::new("private.stage"))
                    .unwrap();

            assert_current_user_only_dacl(&staging);
            assert!(directory.0.join("stolen.stage").is_dir());
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
            let _ = unsafe { LocalFree(descriptor) };
        }
    }
}

#[cfg(windows)]
pub use imp::{
    apply_security_metadata, create_current_user_only_directory, create_current_user_only_file,
    disable_std_handle_inheritance, open_file_no_reparse, open_file_no_reparse_for_rename,
    rename_file_noreplace, restrict_default_dacl_to_current_user, restrict_to_current_user,
    security_metadata, security_metadata_matches, DefaultDaclGuard, SecurityMetadata,
};
