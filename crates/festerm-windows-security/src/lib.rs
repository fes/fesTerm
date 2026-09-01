//! Safe ownership around the Win32 token DACL operations used for named pipes.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod imp {
    use std::{io, mem, ptr};

    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, SetHandleInformation, GENERIC_ALL, HANDLE,
            HANDLE_FLAG_INHERIT,
        },
        Security::{
            AddAccessAllowedAceEx, GetLengthSid, GetTokenInformation, InitializeAcl,
            SetTokenInformation, TokenDefaultDacl, TokenUser, ACL, ACL_REVISION,
            TOKEN_ADJUST_DEFAULT, TOKEN_DEFAULT_DACL, TOKEN_QUERY, TOKEN_USER,
        },
        System::{
            Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };

    /// Restores the process token's original default DACL and closes the token.
    pub struct DefaultDaclGuard {
        token: HANDLE,
        original: Vec<usize>,
        restored: bool,
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
}

#[cfg(windows)]
pub use imp::{
    disable_std_handle_inheritance, restrict_default_dacl_to_current_user, DefaultDaclGuard,
};
