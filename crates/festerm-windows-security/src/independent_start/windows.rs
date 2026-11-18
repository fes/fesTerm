use super::{command_line, HOP_ENV, MAX_UNITS};
use std::{
    env,
    ffi::OsStr,
    fs::File,
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    ptr,
    time::{Duration, Instant},
};
use windows::{
    core::{w, IUnknown, Interface, BSTR},
    Win32::System::{
        Com::{
            CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize,
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
            RPC_C_IMP_LEVEL_IMPERSONATE,
        },
        Ole::{SafeArrayCreateVector, SafeArrayPutElement},
        Variant::{VARIANT, VT_ARRAY, VT_BSTR},
        Wmi::{IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_GENERIC_FLAG_TYPE},
    },
};
use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE},
    Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
    System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess,
        QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    },
};

fn refused() -> io::Error {
    io::Error::other("independent Windows startup context unavailable")
}
fn map(error: windows::core::Error) -> io::Error {
    // Keep only the numeric HRESULT for the existing bounded startup ring.
    // Never retain the COM message, arguments, paths or environment.
    io::Error::from_raw_os_error(error.code().0)
}
fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let v: Vec<_> = value.encode_wide().collect();
    if v.contains(&0) || v.len() > MAX_UNITS {
        Err(refused())
    } else {
        Ok(v)
    }
}
fn birth(handle: HANDLE) -> io::Result<u64> {
    let mut values = [FILETIME::default(); 4];
    if unsafe {
        GetProcessTimes(
            handle,
            &raw mut values[0],
            &raw mut values[1],
            &raw mut values[2],
            &raw mut values[3],
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(values[0].dwHighDateTime) << 32) | u64::from(values[0].dwLowDateTime))
}
fn open(pid: u32) -> io::Result<OwnedHandle> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | 0x0010_0000, 0, pid) };
    if h.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(h) })
    }
}
fn image(handle: HANDLE) -> io::Result<File> {
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &raw mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    use std::os::windows::ffi::OsStringExt;
    File::open(PathBuf::from(std::ffi::OsString::from_wide(
        &buf[..len as usize],
    )))
}
fn same_context(handle: HANDLE) -> io::Result<bool> {
    Ok(crate::imp::same_start_context(handle)?
        && crate::file_identity(&image(handle)?)?
            == crate::file_identity(&File::open(env::current_exe()?)?)?)
}
fn matching_context(handle: HANDLE) -> io::Result<()> {
    if !same_context(handle)? {
        return Err(refused());
    }
    Ok(())
}

/// A one-hop routing marker is not creation authority. Existing generation,
/// protected launch request, spawn evidence and host lease are still required.
pub fn verify_successor() -> io::Result<bool> {
    let Some(marker) = env::var_os(HOP_ENV) else {
        return Ok(false);
    };
    let value = marker.to_str().ok_or_else(refused)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| refused())?
        .as_millis();
    let (pid, expected, remaining) = super::marker(value, now)?;
    if pid == std::process::id() {
        return Err(refused());
    }
    let parent = open(pid)?;
    let h = parent.as_raw_handle();
    if birth(h)? != expected || unsafe { WaitForSingleObject(h, 0) } != 258 {
        return Err(refused());
    }
    matching_context(h)?;
    // This short-lived successor may survive a lost WMI reply/parent timeout.
    // Never transfer this timer to the persistent daemon.
    std::thread::Builder::new()
        .name("starter-deadline".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(remaining.min(6000)));
            std::process::exit(124);
        })?;
    Ok(true)
}

unsafe fn object(service: &IWbemServices, name: &str) -> io::Result<IWbemClassObject> {
    let mut value = None;
    unsafe {
        service.GetObject(
            &BSTR::from(name),
            WBEM_GENERIC_FLAG_TYPE(0),
            None,
            Some(&raw mut value),
            None,
        )
    }
    .map_err(map)?;
    value.ok_or_else(refused)
}
unsafe fn environment_variant(entries: &[Vec<u16>]) -> io::Result<VARIANT> {
    let array = unsafe { SafeArrayCreateVector(VT_BSTR, 0, entries.len() as u32) };
    if array.is_null() {
        return Err(refused());
    }
    let mut value = VARIANT::default();
    unsafe {
        (*value.Anonymous.Anonymous).vt = VT_ARRAY | VT_BSTR;
        (*value.Anonymous.Anonymous).Anonymous.parray = array;
    }
    for (i, entry) in entries.iter().enumerate() {
        let b = BSTR::from_wide(entry);
        let index = i as i32;
        unsafe { SafeArrayPutElement(array, &index, b.as_ptr().cast()) }.map_err(map)?;
    }
    Ok(value)
}

/// Called only inside the existing short-lived, eight-second owned start
/// helper. No remote namespace, credentials, privilege changes or shell shim.
/// A lost COM/PID/exit result is uncertain; this function never launches twice.
pub fn relaunch_current_starter(timeout: Duration) -> io::Result<Option<bool>> {
    if env::var_os(HOP_ENV).is_some() {
        return Err(refused());
    }
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().and_then(|v| v.to_str()) != Some("start") {
        return Err(refused());
    }
    matching_context(unsafe { GetCurrentProcess() })?;
    let exe = env::current_exe()?;
    let exe_file = File::open(&exe)?;
    let mut argv = vec![wide(exe.as_os_str())?];
    for a in args {
        argv.push(wide(&a)?);
    }
    let line = command_line(&argv)?;
    let cwd = wide(env::current_dir()?.as_os_str())?;
    let mut nonce = [0u8; 32];
    if unsafe {
        BCryptGenRandom(
            ptr::null_mut(),
            nonce.as_mut_ptr(),
            nonce.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    } < 0
    {
        return Err(refused());
    }
    let nonce = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| refused())?
        .as_millis()
        + timeout.as_millis().min(7000);
    let marker = format!(
        "1:{}:{}:{nonce}:{expiry}",
        std::process::id(),
        birth(unsafe { GetCurrentProcess() })?
    );
    let mut entries = Vec::new();
    let mut units = 0usize;
    for (key, value) in env::vars_os().chain(std::iter::once((HOP_ENV.into(), marker.into()))) {
        let mut item = wide(&key)?;
        item.push(61);
        item.extend(wide(&value)?);
        units += item.len() + 1;
        if units > 1024 * 1024 {
            return Err(refused());
        }
        entries.push(item);
    }
    let deadline = Instant::now() + timeout;
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(map)?;
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let _apartment = Apartment;
    let pid = unsafe {
        let locator: IWbemLocator =
            CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).map_err(map)?;
        let empty = BSTR::default();
        let service = locator
            .ConnectServer(
                &BSTR::from("ROOT\\CIMV2"),
                &empty,
                &empty,
                &empty,
                0,
                &empty,
                None,
            )
            .map_err(map)?;
        CoSetProxyBlanket(
            &service,
            10,
            0,
            None,
            RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
        .map_err(map)?;
        let startup = object(&service, "Win32_ProcessStartup")?
            .SpawnInstance(0)
            .map_err(map)?;
        startup
            .Put(w!("CreateFlags"), 0, &VARIANT::from(0x0100_0608i32), 0)
            .map_err(map)?;
        startup
            .Put(w!("ShowWindow"), 0, &VARIANT::from(0i32), 0)
            .map_err(map)?;
        startup
            .Put(
                w!("EnvironmentVariables"),
                0,
                &environment_variant(&entries)?,
                0,
            )
            .map_err(map)?;
        let process = object(&service, "Win32_Process")?;
        let mut signature = None;
        process
            .GetMethod(w!("Create"), 0, &raw mut signature, ptr::null_mut())
            .map_err(map)?;
        let input = signature
            .ok_or_else(refused)?
            .SpawnInstance(0)
            .map_err(map)?;
        input
            .Put(
                w!("CommandLine"),
                0,
                &VARIANT::from(BSTR::from_wide(&line)),
                0,
            )
            .map_err(map)?;
        input
            .Put(
                w!("CurrentDirectory"),
                0,
                &VARIANT::from(BSTR::from_wide(&cwd)),
                0,
            )
            .map_err(map)?;
        let unknown: IUnknown = startup.cast().map_err(map)?;
        input
            .Put(
                w!("ProcessStartupInformation"),
                0,
                &VARIANT::from(unknown),
                0,
            )
            .map_err(map)?;
        let mut output = None;
        service
            .ExecMethod(
                &BSTR::from("Win32_Process"),
                &BSTR::from("Create"),
                WBEM_GENERIC_FLAG_TYPE(0),
                None,
                &input,
                Some(&raw mut output),
                None,
            )
            .map_err(map)?;
        let output = output.ok_or_else(refused)?;
        let mut value = VARIANT::default();
        output
            .Get(w!("ReturnValue"), 0, &raw mut value, None, None)
            .map_err(map)?;
        if u32::try_from(&value).map_err(map)? != 0 {
            return Err(refused());
        }
        let mut value = VARIANT::default();
        output
            .Get(w!("ProcessId"), 0, &raw mut value, None, None)
            .map_err(map)?;
        u32::try_from(&value).map_err(map)?
    };
    if pid == 0 {
        return Err(refused());
    }
    let child = match open(pid) {
        Ok(child) => child,
        Err(error) if matches!(error.raw_os_error(), Some(87) | Some(1168)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let h = child.as_raw_handle();
    match same_context(h) {
        Ok(true) => {}
        Ok(false) => return Err(refused()),
        Err(_) if unsafe { WaitForSingleObject(h, 0) } == 0 => return Ok(None),
        Err(error) => return Err(error),
    }
    if crate::file_identity(&image(h)?)? != crate::file_identity(&exe_file)? {
        return Err(refused());
    }
    let _child_birth = birth(h)?;
    let millis = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(u128::from(u32::MAX)) as u32;
    if unsafe { WaitForSingleObject(h, millis) } != 0 {
        // WMI returns a PID, not an owned creation handle. Never terminate a
        // possibly reused PID; the validated successor has its own deadline.
        return Err(refused());
    }
    let mut code = 0;
    if unsafe { GetExitCodeProcess(h, &raw mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(code == 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn com_startup_error_retains_only_numeric_hresult() {
        let code = 0x80070005u32 as i32;
        let error = map(windows::core::Error::from_hresult(windows::core::HRESULT(
            code,
        )));
        assert_eq!(error.raw_os_error(), Some(code));
        assert!(error.get_ref().is_none());
    }
    #[test]
    fn native_windows_starter_command_line_roundtrips_utf16_arguments() {
        let mut args = vec![
            "C:\\fixed path\\helper.exe"
                .encode_utf16()
                .collect::<Vec<_>>(),
            Vec::new(),
            "space value".encode_utf16().collect(),
            "quote\"value".encode_utf16().collect(),
            "tail\\".encode_utf16().collect(),
            "日本語 🙂".encode_utf16().collect(),
        ];
        args.push(vec![0xd800]);
        let mut line = command_line(&args).unwrap();
        line.push(0);
        let mut count = 0;
        let values = unsafe {
            windows_sys::Win32::UI::Shell::CommandLineToArgvW(line.as_ptr(), &raw mut count)
        };
        assert!(!values.is_null());
        struct Arguments(*mut windows_sys::core::PWSTR);
        impl Drop for Arguments {
            fn drop(&mut self) {
                unsafe { windows_sys::Win32::Foundation::LocalFree(self.0.cast()) };
            }
        }
        let _owned = Arguments(values);
        assert_eq!(count as usize, args.len());
        for (index, expected) in args.iter().enumerate() {
            let p = unsafe { *values.add(index) };
            let mut length = 0;
            while unsafe { *p.add(length) } != 0 {
                length += 1;
            }
            assert_eq!(unsafe { std::slice::from_raw_parts(p, length) }, expected);
        }
    }
    #[test]
    fn unicode_environment_safearray_preserves_drive_entries_and_wide_values() {
        let entries = vec![
            "=C:=C:\\fixed".encode_utf16().collect::<Vec<_>>(),
            "UNICODE=日本語 🙂".encode_utf16().collect(),
            vec![88, 61, 0xd800],
        ];
        let value = unsafe { environment_variant(&entries) }.unwrap();
        let array = unsafe { value.Anonymous.Anonymous.Anonymous.parray };
        for (index, entry) in entries.iter().enumerate() {
            let mut actual = BSTR::default();
            unsafe {
                windows::Win32::System::Ole::SafeArrayGetElement(
                    array,
                    &(index as i32),
                    (&raw mut actual).cast(),
                )
            }
            .unwrap();
            assert_eq!(
                unsafe { std::slice::from_raw_parts(actual.as_ptr(), actual.len()) },
                entry
            );
        }
    }
}
