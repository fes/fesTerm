//! Trusted selection of the optional bundled Windows ConPTY runtime.
//!
//! The loader accepts only the fixed sidecar path relative to the executable,
//! verifies both native files against the repository manifest, restricts the
//! process DLL search path to System32, and preloads the verified DLL by its
//! absolute path. `portable-pty` subsequently observes that loaded module when
//! it probes `conpty.dll`; otherwise it uses the inbox Kernel32 exports.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod imp {
    use std::{
        ffi::OsString,
        fs::File,
        io::{self, Read},
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::{Path, PathBuf},
        sync::OnceLock,
    };

    use windows_sys::Win32::{
        Foundation::HMODULE,
        Security::Cryptography::{
            CryptAcquireContextW, CryptCreateHash, CryptDestroyHash, CryptGetHashParam,
            CryptHashData, CryptReleaseContext, CALG_SHA_512, CRYPT_VERIFYCONTEXT, HP_HASHVAL,
            PROV_RSA_AES,
        },
        System::LibraryLoader::{
            GetModuleFileNameW, GetModuleHandleW, LoadLibraryExW, SetDefaultDllDirectories,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
        },
    };

    /// Fixed install-relative directory containing a validated ConPTY sidecar.
    pub const BUNDLED_CONPTY_RUNTIME_DIRECTORY: &str = r"runtime\conpty";

    const MANIFEST: &str = include_str!("../../../third_party/conpty/manifest.json");
    const CONPTY_MODULE_NAME: &[u16] = &[
        b'c' as u16,
        b'o' as u16,
        b'n' as u16,
        b'p' as u16,
        b't' as u16,
        b'y' as u16,
        b'.' as u16,
        b'd' as u16,
        b'l' as u16,
        b'l' as u16,
        0,
    ];
    const MODULE_PATH_CAPACITY: usize = 32_768;

    /// The ConPTY implementation selected before `portable-pty` creates its
    /// first pseudoconsole.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum ConptyRuntimeSelection {
        /// A pinned, verified sidecar was loaded by an absolute path.
        Bundled,
        /// No valid sidecar was available, so `portable-pty` will use inbox
        /// Kernel32 ConPTY exports.
        Inbox,
    }

    impl ConptyRuntimeSelection {
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Bundled => "bundled",
                Self::Inbox => "inbox",
            }
        }
    }

    /// A safety failure that prevents selecting an untrusted ConPTY module.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ConptyRuntimeError {
        message: String,
    }

    impl ConptyRuntimeError {
        fn new(message: impl Into<String>) -> Self {
            Self {
                message: message.into(),
            }
        }
    }

    impl std::fmt::Display for ConptyRuntimeError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&self.message)
        }
    }

    impl std::error::Error for ConptyRuntimeError {}

    struct BundledRuntime {
        dll: PathBuf,
        dll_sha512: String,
        host: PathBuf,
        host_sha512: String,
    }

    static PREPARED_RUNTIME: OnceLock<Result<ConptyRuntimeSelection, ConptyRuntimeError>> =
        OnceLock::new();

    /// Configures secure DLL search once and selects the fixed bundled runtime
    /// when its complete sidecar is present and hash-verified.
    ///
    /// This must run before the first `portable-pty` ConPTY allocation. It
    /// deliberately does not inspect configuration, the current directory,
    /// `PATH`, or any user-provided path.
    pub fn prepare_conpty_runtime() -> Result<ConptyRuntimeSelection, ConptyRuntimeError> {
        PREPARED_RUNTIME
            .get_or_init(prepare_conpty_runtime_inner)
            .clone()
    }

    fn prepare_conpty_runtime_inner() -> Result<ConptyRuntimeSelection, ConptyRuntimeError> {
        configure_secure_dll_search()?;

        let existing_module = loaded_conpty_module()?;
        let runtime = match bundled_runtime_from_current_exe()? {
            Some(runtime) if bundled_runtime_is_valid(&runtime) => runtime,
            Some(_) | None => {
                if existing_module.is_some() {
                    return Err(ConptyRuntimeError::new(
                        "an unverified ConPTY module was loaded before inbox fallback",
                    ));
                }
                return Ok(ConptyRuntimeSelection::Inbox);
            }
        };

        if let Some(module) = existing_module {
            ensure_module_is_expected(&module, &runtime.dll)?;
            return Ok(ConptyRuntimeSelection::Bundled);
        }

        match load_verified_runtime(&runtime.dll) {
            Ok(()) => Ok(ConptyRuntimeSelection::Bundled),
            Err(_) => match loaded_conpty_module()? {
                // A failed absolute load with no module leaves the constrained
                // System32 search safe for portable-pty's inbox fallback.
                None => Ok(ConptyRuntimeSelection::Inbox),
                Some(module) => {
                    ensure_module_is_expected(&module, &runtime.dll)?;
                    Ok(ConptyRuntimeSelection::Bundled)
                }
            },
        }
    }

    fn configure_secure_dll_search() -> Result<(), ConptyRuntimeError> {
        // `portable-pty` uses LoadLibraryW("conpty.dll"). Limiting the
        // process default to System32 prevents a sidecar in the executable,
        // current, or PATH directories from winning when no verified module
        // has already been preloaded below.
        let configured = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if configured == 0 {
            Err(last_os_error(
                "could not configure the Windows DLL search path",
            ))
        } else {
            Ok(())
        }
    }

    fn bundled_runtime_from_current_exe() -> Result<Option<BundledRuntime>, ConptyRuntimeError> {
        let executable = match std::env::current_exe() {
            Ok(executable) => executable,
            Err(_) => return Ok(None),
        };
        let executable = match executable.canonicalize() {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        bundled_runtime_from_executable(&executable).map(Some)
    }

    fn bundled_runtime_from_executable(
        executable: &Path,
    ) -> Result<BundledRuntime, ConptyRuntimeError> {
        let base = executable
            .parent()
            .ok_or_else(|| ConptyRuntimeError::new("the executable has no parent directory"))?
            .join(BUNDLED_CONPTY_RUNTIME_DIRECTORY);
        let (runtime_rid, host_architecture) = runtime_architecture();
        let dll_asset = format!("{runtime_rid}/conpty.dll");
        let host_asset = format!("{host_architecture}/OpenConsole.exe");
        Ok(BundledRuntime {
            dll: base.join(runtime_rid).join("conpty.dll"),
            dll_sha512: manifest_file_sha512(&dll_asset)?,
            host: base
                .join(runtime_rid)
                .join(host_architecture)
                .join("OpenConsole.exe"),
            host_sha512: manifest_file_sha512(&host_asset)?,
        })
    }

    #[cfg(target_arch = "x86")]
    const fn runtime_architecture() -> (&'static str, &'static str) {
        ("win-x86", "x86")
    }

    #[cfg(target_arch = "x86_64")]
    const fn runtime_architecture() -> (&'static str, &'static str) {
        ("win-x64", "x64")
    }

    #[cfg(target_arch = "aarch64")]
    const fn runtime_architecture() -> (&'static str, &'static str) {
        ("win-arm64", "arm64")
    }

    fn manifest_file_sha512(asset: &str) -> Result<String, ConptyRuntimeError> {
        let asset_marker = format!("\"{asset}\":");
        let asset_offset = MANIFEST.find(&asset_marker).ok_or_else(|| {
            ConptyRuntimeError::new("the ConPTY manifest lacks a required runtime file hash")
        })?;
        let sha_marker = "\"sha512\": \"";
        let hash_offset = MANIFEST[asset_offset..]
            .find(sha_marker)
            .map(|offset| asset_offset + offset + sha_marker.len())
            .ok_or_else(|| {
                ConptyRuntimeError::new("the ConPTY manifest has an invalid runtime file hash")
            })?;
        let hash = MANIFEST
            .get(hash_offset..hash_offset + 128)
            .filter(|hash| hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| {
                ConptyRuntimeError::new("the ConPTY manifest has an invalid runtime file hash")
            })?;
        Ok(hash.to_owned())
    }

    fn bundled_runtime_is_valid(runtime: &BundledRuntime) -> bool {
        matches!(
            file_hash_matches(&runtime.dll, &runtime.dll_sha512),
            Ok(true)
        ) && matches!(
            file_hash_matches(&runtime.host, &runtime.host_sha512),
            Ok(true)
        )
    }

    fn file_hash_matches(path: &Path, expected: &str) -> Result<bool, ConptyRuntimeError> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(ConptyRuntimeError::new(format!(
                    "could not read a bundled ConPTY runtime file: {error}"
                )));
            }
        };
        let mut hasher = Sha512Hasher::new()?;
        let mut buffer = [0_u8; 16_384];
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                ConptyRuntimeError::new(format!(
                    "could not hash a bundled ConPTY runtime file: {error}"
                ))
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read])?;
        }
        Ok(hex_encode(&hasher.finish()?) == expected)
    }

    struct Sha512Hasher {
        provider: usize,
        hash: usize,
    }

    impl Sha512Hasher {
        fn new() -> Result<Self, ConptyRuntimeError> {
            let mut provider = 0;
            let acquired = unsafe {
                CryptAcquireContextW(
                    &mut provider,
                    std::ptr::null(),
                    std::ptr::null(),
                    PROV_RSA_AES,
                    CRYPT_VERIFYCONTEXT,
                )
            };
            if acquired == 0 {
                return Err(last_os_error(
                    "could not acquire the Windows SHA-512 provider",
                ));
            }

            let mut hash = 0;
            let created = unsafe { CryptCreateHash(provider, CALG_SHA_512, 0, 0, &mut hash) };
            if created == 0 {
                unsafe {
                    CryptReleaseContext(provider, 0);
                }
                return Err(last_os_error("could not create a Windows SHA-512 hash"));
            }
            Ok(Self { provider, hash })
        }

        fn update(&mut self, bytes: &[u8]) -> Result<(), ConptyRuntimeError> {
            let byte_count =
                u32::try_from(bytes.len()).expect("fixed hash buffer length fits in u32");
            let updated = unsafe { CryptHashData(self.hash, bytes.as_ptr(), byte_count, 0) };
            if updated == 0 {
                Err(last_os_error("could not update a Windows SHA-512 hash"))
            } else {
                Ok(())
            }
        }

        fn finish(&self) -> Result<[u8; 64], ConptyRuntimeError> {
            let mut digest = [0_u8; 64];
            let mut digest_length =
                u32::try_from(digest.len()).expect("SHA-512 digest length fits in u32");
            let finished = unsafe {
                CryptGetHashParam(
                    self.hash,
                    HP_HASHVAL,
                    digest.as_mut_ptr(),
                    &mut digest_length,
                    0,
                )
            };
            if finished == 0 || digest_length != digest.len() as u32 {
                Err(last_os_error("could not finish a Windows SHA-512 hash"))
            } else {
                Ok(digest)
            }
        }
    }

    impl Drop for Sha512Hasher {
        fn drop(&mut self) {
            unsafe {
                CryptDestroyHash(self.hash);
                CryptReleaseContext(self.provider, 0);
            }
        }
    }

    fn hex_encode(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            encoded.push(HEX[usize::from(byte >> 4)] as char);
            encoded.push(HEX[usize::from(byte & 0x0f)] as char);
        }
        encoded
    }

    fn load_verified_runtime(expected: &Path) -> Result<(), ConptyRuntimeError> {
        let wide_path = wide_path(expected);
        let module = unsafe {
            LoadLibraryExW(
                wide_path.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(last_os_error(
                "could not load the verified bundled ConPTY runtime",
            ));
        }
        let loaded = module_path(module)?;
        ensure_module_is_expected(&loaded, expected)
    }

    fn loaded_conpty_module() -> Result<Option<PathBuf>, ConptyRuntimeError> {
        let module = unsafe { GetModuleHandleW(CONPTY_MODULE_NAME.as_ptr()) };
        if module.is_null() {
            Ok(None)
        } else {
            module_path(module).map(Some)
        }
    }

    fn ensure_module_is_expected(loaded: &Path, expected: &Path) -> Result<(), ConptyRuntimeError> {
        let loaded = loaded.canonicalize().map_err(|error| {
            ConptyRuntimeError::new(format!(
                "could not canonicalize the selected ConPTY module: {error}"
            ))
        })?;
        let expected = expected.canonicalize().map_err(|error| {
            ConptyRuntimeError::new(format!(
                "could not canonicalize the verified ConPTY module: {error}"
            ))
        })?;
        if loaded == expected {
            Ok(())
        } else {
            Err(ConptyRuntimeError::new(
                "a ConPTY module outside the verified runtime path is already loaded",
            ))
        }
    }

    fn module_path(module: HMODULE) -> Result<PathBuf, ConptyRuntimeError> {
        let mut buffer = vec![0_u16; MODULE_PATH_CAPACITY];
        let length = unsafe {
            GetModuleFileNameW(
                module,
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).expect("module path buffer fits in u32"),
            )
        };
        if length == 0 || usize::try_from(length).ok() == Some(buffer.len()) {
            return Err(last_os_error(
                "could not resolve the selected ConPTY module path",
            ));
        }
        buffer.truncate(length as usize);
        Ok(PathBuf::from(OsString::from_wide(&buffer)))
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn last_os_error(context: &str) -> ConptyRuntimeError {
        ConptyRuntimeError::new(format!("{context}: {}", io::Error::last_os_error()))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn manifest_supplies_hashes_for_every_supported_runtime_file() {
            for key in [
                "win-x86/conpty.dll",
                "x86/OpenConsole.exe",
                "win-x64/conpty.dll",
                "x64/OpenConsole.exe",
                "win-arm64/conpty.dll",
                "arm64/OpenConsole.exe",
            ] {
                assert_eq!(
                    manifest_file_sha512(key).map(|hash| hash.len()).ok(),
                    Some(128),
                    "missing SHA-512 for {key}"
                );
            }
        }

        #[test]
        fn runtime_sidecar_path_is_fixed_relative_to_the_executable() {
            let executable = Path::new(r"C:\Program Files\fesTerm\fesTerm.exe");
            let runtime = bundled_runtime_from_executable(executable)
                .expect("manifest provides this architecture");
            assert!(runtime.dll.ends_with(
                Path::new(BUNDLED_CONPTY_RUNTIME_DIRECTORY)
                    .join(runtime_architecture().0)
                    .join("conpty.dll")
            ));
            assert!(runtime.host.ends_with(
                Path::new(BUNDLED_CONPTY_RUNTIME_DIRECTORY)
                    .join(runtime_architecture().0)
                    .join(runtime_architecture().1)
                    .join("OpenConsole.exe")
            ));
        }
    }
}

#[cfg(windows)]
pub use imp::*;
