use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell, ffi::OsString, io, marker::PhantomData, path::PathBuf, process::Stdio, rc::Rc,
    sync::Arc, time::Duration,
};

use crate::protected_state::Lease;

thread_local! {
    static OWNERSHIP: RefCell<Option<Arc<Lease>>> = const { RefCell::new(None) };
}

pub(super) struct OperationGuard {
    previous: Option<Arc<Lease>>,
    _thread: PhantomData<Rc<()>>,
}

impl OperationGuard {
    pub(super) fn enter(lease: Lease) -> Self {
        Self {
            previous: OWNERSHIP.with(|slot| slot.replace(Some(Arc::new(lease)))),
            _thread: PhantomData,
        }
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        drop(OWNERSHIP.with(|slot| slot.replace(self.previous.take())));
    }
}

pub(super) fn retain_operation() -> Option<Arc<Lease>> {
    OWNERSHIP.with(|slot| slot.borrow().clone())
}

pub const OUTPUT_LIMIT: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskAction {
    Query,
    Create(String),
    Start,
    Stop,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeRequest {
    Process { tool: PathBuf, args: Vec<OsString> },
    Task { name: String, action: TaskAction },
}

pub struct NativeOutput {
    pub success: bool,
    pub code: i32,
    pub stdout: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskSnapshot {
    pub xml: Option<String>,
    pub security: Option<String>,
    pub state: i32,
    pub last_exit: i32,
}

/// Injection is for deterministic native-adapter tests, not host health evidence.
pub trait CommandRunner {
    fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput>;
}

pub struct NativeRunner;

impl CommandRunner for NativeRunner {
    fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
        match request {
            NativeRequest::Process { tool, args } => {
                process(tool.clone(), args.clone(), retain_operation())
            }
            NativeRequest::Task { name, action } => task(name, action, retain_operation()),
        }
    }
}

fn process(
    tool: PathBuf,
    args: Vec<OsString>,
    ownership: Option<Arc<Lease>>,
) -> io::Result<NativeOutput> {
    std::thread::spawn(move || {
        let _ownership = ownership;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async move {
            use tokio::io::AsyncReadExt;
            let mut command = tokio::process::Command::new(tool);
            command
                .args(args)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(target_os = "linux")]
            {
                let uid = festerm_unix_security::current_user_id()?;
                command
                    .env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"))
                    .env(
                        "DBUS_SESSION_BUS_ADDRESS",
                        format!("unix:path=/run/user/{uid}/bus"),
                    )
                    .env("SYSTEMD_PAGER", "")
                    .env("SYSTEMD_COLORS", "0");
            }
            let mut child = command
                .spawn()
                .map_err(|_| super::refused("host.service_native_spawn_failed"))?;
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| super::refused("host.service_native_output"))?;
            let result = tokio::time::timeout(DEADLINE, async {
                let mut bytes = Vec::new();
                (&mut stdout)
                    .take(OUTPUT_LIMIT as u64 + 1)
                    .read_to_end(&mut bytes)
                    .await?;
                if bytes.len() > OUTPUT_LIMIT {
                    return Err(super::refused("host.service_native_output_limit"));
                }
                let status = child.wait().await?;
                Ok(NativeOutput {
                    success: status.success(),
                    code: status.code().unwrap_or(-1),
                    stdout: bytes,
                })
            })
            .await;
            if !matches!(result, Ok(Ok(_))) {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            }
            result.map_err(|_| super::refused("host.service_native_timeout"))?
        })
    })
    .join()
    .map_err(|_| super::refused("host.service_native_worker_failed"))?
}

#[cfg(windows)]
fn task(
    name: &str,
    action: &TaskAction,
    ownership: Option<Arc<Lease>>,
) -> io::Result<NativeOutput> {
    use festerm_windows_security::host_service as native;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let ownership =
        ownership.ok_or_else(|| super::refused("host.service_unscoped_task_operation"))?;
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::AcqRel) {
        return Err(super::refused("host.service_native_worker_busy"));
    }
    let action = match action {
        TaskAction::Query => native::TaskAction::Query,
        TaskAction::Create(xml) => native::TaskAction::Create(xml.clone()),
        TaskAction::Start => native::TaskAction::Start,
        TaskAction::Stop => native::TaskAction::Stop,
        TaskAction::Remove => native::TaskAction::Remove,
    };
    let name = name.to_owned();
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        // A timed-out caller must not let another process confirm absence
        // while this native effect can still complete.
        let _ownership = ownership;
        struct Permit;
        impl Drop for Permit {
            fn drop(&mut self) {
                BUSY.store(false, Ordering::Release);
            }
        }
        let permit = Permit;
        let result = native::task(&name, action).and_then(|task| {
            serde_json::to_vec(&TaskSnapshot {
                xml: task.xml,
                security: task.security,
                state: task.state,
                last_exit: task.last_exit,
            })
            .map(|stdout| NativeOutput {
                success: true,
                code: 0,
                stdout,
            })
            .map_err(|_| super::refused("host.service_native_output"))
        });
        drop(permit);
        let _ = send.send(result);
    });
    // At most one local COM operation can remain outstanding after timeout.
    receive
        .recv_timeout(DEADLINE)
        .map_err(|_| super::refused("host.service_native_timeout"))?
}

#[cfg(not(windows))]
fn task(_: &str, _: &TaskAction, _: Option<Arc<Lease>>) -> io::Result<NativeOutput> {
    Err(super::refused("host.service_platform_mismatch"))
}
