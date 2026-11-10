//! Same-user Task Scheduler control. Publisher admission is separate and closed.
use std::io;
use windows::{
    core::BSTR,
    Win32::System::{
        Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_MULTITHREADED,
        },
        TaskScheduler::{
            ITaskService, TaskScheduler, TASK_CREATE, TASK_DONT_ADD_PRINCIPAL_ACE,
            TASK_LOGON_INTERACTIVE_TOKEN,
        },
        Variant::VARIANT,
    },
};

#[derive(Clone, Debug)]
pub enum TaskAction {
    Prepare(String),
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
    if matches!(&action, TaskAction::Prepare(xml) | TaskAction::Create(xml) if xml.len() > 64 * 1024)
    {
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
        if let TaskAction::Prepare(xml) = &action {
            let definition = service.NewTask(0).map_err(map)?;
            definition
                .SetXmlText(&BSTR::from(xml.as_str()))
                .map_err(map)?;
            return Ok(TaskObservation {
                xml: Some(bounded(definition.XmlText().map_err(map)?)?),
                security: Some(task_security_policy(&sid)),
                state: 0,
                last_exit: 0,
            });
        }
        let folder = service.GetFolder(&BSTR::from("\\")).map_err(map)?;
        let name = BSTR::from(name);
        let task = match &action {
            TaskAction::Create(xml) => folder
                .RegisterTask(
                    &name,
                    &BSTR::from(xml.as_str()),
                    TASK_CREATE.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0,
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
            TaskAction::Stop => {
                return Err(io::Error::other(
                    "host.service_windows_authenticated_graceful_stop_unavailable",
                ));
            }
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
        let xml = bounded(task.Definition().map_err(map)?.XmlText().map_err(map)?)?;
        let raw_security = bounded(task.GetSecurityDescriptor(5).map_err(map)?)?;
        let security = checked_task_security(&raw_security, &sid)?;
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

fn task_security_policy(sid: &str) -> String {
    format!("task-owner-system-full-control-v1:{sid}")
}

fn checked_task_security(sddl: &str, user: &str) -> io::Result<String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW,
                ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl, ACCESS_ALLOWED_ACE,
            ACL, SE_DACL_PROTECTED,
        },
    };
    let refused = || io::Error::other("host.service_task_security_unqualified");
    let text: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(refused());
    }
    let result = (|| {
        let mut control = 0;
        let mut revision = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            || control & SE_DACL_PROTECTED == 0
            || unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
            } == 0
            || present == 0
            || dacl.is_null()
            || unsafe { (*dacl).AceCount } != 2
        {
            return Err(refused());
        }
        for index in 0..2 {
            let mut raw = std::ptr::null_mut();
            if unsafe { GetAce(dacl, index, &mut raw) } == 0 || raw.is_null() {
                return Err(refused());
            }
            let ace = unsafe { &mut *raw.cast::<ACCESS_ALLOWED_ACE>() };
            if ace.Header.AceType != 0
                || ace.Header.AceFlags != 0
                || !matches!(ace.Mask, 0x1000_0000 | 0x001f_01ff)
            {
                return Err(refused());
            }
            // Compare equivalent generic/file-all rights using native SDDL
            // normalization, never a permissive text parser.
            ace.Mask = 0x1000_0000;
        }
        let mut canonical = std::ptr::null_mut();
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                1,
                5,
                &mut canonical,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(refused());
        }
        let mut length = 0;
        while length < 64 * 1024 && unsafe { *canonical.add(length) } != 0 {
            length += 1;
        }
        let canonical_text = if length < 64 * 1024 {
            String::from_utf16(unsafe { std::slice::from_raw_parts(canonical, length) })
                .map_err(|_| refused())
        } else {
            Err(refused())
        };
        unsafe { LocalFree(canonical.cast()) };
        let canonical_text = canonical_text?;
        let expected = [
            format!("O:{user}D:P(A;;GA;;;SY)(A;;GA;;;{user})"),
            format!("O:{user}D:P(A;;GA;;;{user})(A;;GA;;;SY)"),
        ];
        if !expected.contains(&canonical_text) {
            return Err(refused());
        }
        Ok(task_security_policy(user))
    })();
    unsafe { LocalFree(descriptor) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_recovery_security_requires_exact_owner_system_protected_dacl() {
        let user = "S-1-5-21-1-2-3-1001";
        assert_eq!(
            checked_task_security(&format!("O:{user}D:P(A;;FA;;;SY)(A;;GA;;;{user})"), user)
                .unwrap(),
            task_security_policy(user)
        );
        for sddl in [
            format!("O:{user}D:(A;;GA;;;SY)(A;;GA;;;{user})"),
            format!("O:{user}D:P(A;;GA;;;WD)(A;;GA;;;{user})"),
            format!("O:SYD:P(A;;GA;;;SY)(A;;GA;;;{user})"),
            format!("O:{user}D:P(A;;FR;;;SY)(A;;GA;;;{user})"),
            format!("O:{user}D:P(A;;GA;;;SY)(A;;GA;;;{user})(A;;FR;;;WD)"),
        ] {
            assert!(checked_task_security(&sddl, user).is_err());
        }
    }
}
