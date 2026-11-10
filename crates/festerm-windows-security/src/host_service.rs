//! Same-user Task Scheduler and handle-bound Authenticode checks.
use std::{
    fs::File,
    io,
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    path::Path,
};
use windows::{
    core::BSTR,
    Win32::System::{
        Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_MULTITHREADED,
        },
        TaskScheduler::{ITaskService, TaskScheduler, TASK_CREATE, TASK_LOGON_INTERACTIVE_TOKEN},
        Variant::VARIANT,
    },
};

#[derive(Clone, Debug)]
pub enum TaskAction {
    Query,
    Create(String),
    Start,
    Stop,
    Remove,
}

#[derive(Clone, Debug)]
pub struct TaskObservation {
    pub xml: Option<String>,
    pub security: Option<String>,
    pub state: i32,
    pub last_exit: i32,
}

/// Local interactive-token tasks only; no passwords, remote server or elevation.
pub fn task(name: &str, action: TaskAction) -> io::Result<TaskObservation> {
    if !name.starts_with("fesTerm-Host-")
        || name.len() > 96
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if matches!(&action, TaskAction::Create(xml) if xml.len() > 64 * 1024) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let (sid, _) = crate::current_user_service_context()?;
    let map = |_| io::Error::other("host.service_task_scheduler_unavailable");
    // The caller runs this on a bounded dedicated native-operation thread.
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
    let empty = VARIANT::default();
    // All COM objects outlive their calls and are released before the apartment.
    unsafe {
        let service: ITaskService =
            CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER).map_err(map)?;
        service
            .Connect(&empty, &empty, &empty, &empty)
            .map_err(map)?;
        let folder = service.GetFolder(&BSTR::from("\\")).map_err(map)?;
        let name = BSTR::from(name);
        let task = match &action {
            TaskAction::Create(xml) => folder
                .RegisterTask(
                    &name,
                    &BSTR::from(xml.as_str()),
                    TASK_CREATE.0,
                    &VARIANT::from(sid.as_str()),
                    &empty,
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &VARIANT::from(format!("O:{sid}D:P(A;;GA;;;SY)(A;;GA;;;{sid})").as_str()),
                )
                .map_err(map)?,
            _ => match folder.GetTask(&name) {
                Ok(task) => task,
                Err(error) if error.code().0 as u32 == 0x80070002 => {
                    if !matches!(action, TaskAction::Query) {
                        return Err(io::Error::other("host.service_task_absent"));
                    }
                    return Ok(TaskObservation {
                        xml: None,
                        security: None,
                        state: 0,
                        last_exit: 0,
                    });
                }
                Err(error) => return Err(map(error)),
            },
        };
        match action {
            TaskAction::Start => {
                task.Run(&empty).map_err(map)?;
            }
            TaskAction::Stop => task.Stop(0).map_err(map)?,
            TaskAction::Remove => {
                folder.DeleteTask(&name, 0).map_err(map)?;
                return Ok(TaskObservation {
                    xml: None,
                    security: None,
                    state: 0,
                    last_exit: 0,
                });
            }
            _ => {}
        }
        let xml = bounded(task.Xml().map_err(map)?)?;
        let security = bounded(task.GetSecurityDescriptor(5).map_err(map)?)?;
        let state = task.State().map_err(map)?.0;
        let instances = task.GetInstances(0).map_err(map)?.Count().map_err(map)?;
        Ok(TaskObservation {
            xml: Some(xml),
            security: Some(security),
            state: if instances > 0 && state != 2 {
                4
            } else {
                state
            },
            last_exit: task.LastTaskResult().map_err(map)?,
        })
    }
}

fn bounded(text: BSTR) -> io::Result<String> {
    if text.len() > 64 * 1024 {
        return Err(io::Error::other("host.service_task_output_limit"));
    }
    let text = text.to_string();
    if text.len() > 64 * 1024 {
        return Err(io::Error::other("host.service_task_output_limit"));
    }
    Ok(text)
}

/// The updater-signed artifact authenticates the publisher and exact bytes.
/// This additionally requires the native Authenticode chain without UI/network.
pub fn verify_authenticode(file: &File, path: &Path) -> io::Result<()> {
    use windows_sys::Win32::Security::WinTrust::*;
    let name: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut info: WINTRUST_FILE_INFO = unsafe { std::mem::zeroed() };
    info.cbStruct = std::mem::size_of_val(&info) as u32;
    info.pcwszFilePath = name.as_ptr();
    info.hFile = file.as_raw_handle();
    let mut data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
    data.cbStruct = std::mem::size_of_val(&data) as u32;
    data.dwUIChoice = WTD_UI_NONE;
    data.fdwRevocationChecks = WTD_REVOKE_NONE;
    data.dwUnionChoice = WTD_CHOICE_FILE;
    data.Anonymous.pFile = &mut info;
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    data.dwProvFlags = WTD_CACHE_ONLY_URL_RETRIEVAL;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    if status != 0 {
        return Err(io::Error::other("host.service_authenticode_required"));
    }
    Ok(())
}
