//! Native enforcement lives only in this module. Unsupported platforms do not
//! instantiate the engine. A process, removed globals, or advisory RSS limit is
//! never reported as confinement.
use crate::Error;

pub const LINUX_PROFILE: &str = "linux-seccomp-stdio-rlimit-v1";
/// Seatbelt deny-default sandbox, CPU/core/file rlimits, and a supervisor
/// physical-footprint kill at [`crate::PROCESS_BYTES`]. macOS has no
/// enforceable per-process address-space rlimit, so the memory ceiling is a
/// supervisor kill (polled), backed by QuickJS's own heap limit.
pub const MACOS_PROFILE: &str = "macos-seatbelt-footprint-watchdog-v1";
/// Zero-capability AppContainer in a kill-on-close Job Object with hard
/// commit, process-count, and CPU-time limits; no inherited handles.
pub const WINDOWS_PROFILE: &str = "windows-appcontainer-job-v1";

pub fn availability() -> Result<&'static str, Error> {
    if cfg!(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )) {
        // Actual kernel/rlimit qualification occurs inside the fresh child,
        // before Ready; this is only an architecture support check.
        Ok(LINUX_PROFILE)
    } else if cfg!(target_os = "macos") {
        Ok(MACOS_PROFILE)
    } else if cfg!(target_os = "ios") {
        Err(Error::Unavailable(
            "iOS has no qualified separate helper/stdio transport",
        ))
    } else if cfg!(all(
        target_os = "windows",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )) {
        Ok(WINDOWS_PROFILE)
    } else {
        Err(Error::Unavailable(
            "no qualified native confinement implementation",
        ))
    }
}

pub(crate) fn confine() -> Result<(), Error> {
    availability()?;
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ))]
    return linux::confine();
    #[cfg(target_os = "macos")]
    return macos::confine();
    #[cfg(windows)]
    return crate::windows::confine();
    #[allow(unreachable_code)]
    Err(Error::Unavailable("native confinement unavailable"))
}

#[cfg(unix)]
pub(crate) fn nonblocking(fd: i32) -> Result<(), Error> {
    // SAFETY: fcntl operates on the supplied owned/inherited pipe; no pointer
    // arguments. All return values are checked.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(target_os = "ios"))]
pub(crate) fn cpu_time() -> Option<std::time::Duration> {
    #[cfg(unix)]
    {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: timespec is a valid out pointer. A clock failure fails closed
        // by exhausting the CPU quota rather than granting unlimited execution.
        if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) } == 0 {
            return Some(std::time::Duration::new(
                time.tv_sec as u64,
                time.tv_nsec as u32,
            ));
        }
    }
    #[cfg(windows)]
    {
        return crate::windows::cpu_time();
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
mod linux {
    use super::*;
    use crate::{CPU_SECONDS, PROCESS_BYTES};

    fn limit(resource: libc::__rlimit_resource_t, value: u64) -> Result<(), Error> {
        let r = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: r points to an initialized rlimit; affects this child only.
        if unsafe { libc::setrlimit(resource, &r) } != 0 {
            return Err(Error::Unavailable(
                "required native hard resource limit failed",
            ));
        }
        let mut actual = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: actual is a valid output pointer.
        if unsafe { libc::getrlimit(resource, &mut actual) } != 0
            || actual.rlim_cur != value
            || actual.rlim_max != value
        {
            return Err(Error::Unavailable(
                "required native hard resource limit not active",
            ));
        }
        Ok(())
    }

    fn stmt(code: u16, k: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        }
    }
    fn jump(k: u32, yes: u8, no: u8) -> libc::sock_filter {
        libc::sock_filter {
            code: 0x15,
            jt: yes,
            jf: no,
            k,
        } // BPF_JMP|JEQ|K
    }

    pub(super) fn confine() -> Result<(), Error> {
        if std::env::vars_os().any(|(key, value)| key != "MALLOC_ARENA_MAX" || value != "1") {
            return Err(Error::Unavailable(
                "worker environment was not cleared by its supervisor",
            ));
        }
        for fd in [0, 1] {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: fstat initializes stat on success; fd is inherited stdio.
            if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
                return Err(Error::Unavailable("worker stdio is not an inherited pipe"));
            }
            // SAFETY: successful fstat above initialized every field.
            if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFIFO {
                return Err(Error::Unavailable("worker stdio is not an inherited pipe"));
            }
        }
        // Lowering RLIMIT_AS below existing mappings would not evict them.
        // Refuse an oversized app executable/runtime before claiming a ceiling.
        let mut statm = String::new();
        {
            use std::io::Read;
            std::fs::File::open("/proc/self/statm")
                .and_then(|file| file.take(128).read_to_string(&mut statm))
                .map_err(|_| Error::Unavailable("kernel process-memory evidence unavailable"))?;
        }
        let pages = statm
            .split_ascii_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or(Error::Unavailable("kernel process-memory evidence invalid"))?;
        // SAFETY: scalar query of the platform page size, checked below.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0
            || pages
                .checked_mul(page_size as u64)
                .is_none_or(|bytes| bytes > PROCESS_BYTES)
        {
            return Err(Error::Unavailable(
                "worker baseline already exceeds process memory ceiling",
            ));
        }
        // The helper must be a fresh, single-threaded main, before any engine
        // initialization. TSYNC also closes a race with any accidental thread.
        // SAFETY: scalar prctl/syscall arguments; no borrowed memory escapes.
        unsafe {
            let parent = libc::getppid();
            if parent <= 1
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                || libc::getppid() != parent
                || libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0
                || libc::syscall(libc::SYS_close_range, 2u32, u32::MAX, 0u32) != 0
            {
                return Err(Error::Unavailable(
                    "native descriptor/parent isolation failed",
                ));
            }
        }
        limit(libc::RLIMIT_AS, PROCESS_BYTES)?;
        limit(libc::RLIMIT_CPU, CPU_SECONDS)?;
        limit(libc::RLIMIT_CORE, 0)?;
        limit(libc::RLIMIT_FSIZE, 0)?;
        limit(libc::RLIMIT_NOFILE, 2)?;
        nonblocking(0)?;
        nonblocking(1)?;
        #[cfg(target_arch = "x86_64")]
        const ARCH: u32 = 0xc000003e;
        #[cfg(target_arch = "aarch64")]
        const ARCH: u32 = 0xc00000b7;
        const ALLOW: u32 = 0x7fff0000;
        const DENY: u32 = 0x00050000 | libc::EPERM as u32;
        const KILL: u32 = 0x80000000;
        let mut filter = vec![
            stmt(0x20, 4), // seccomp_data.arch
            jump(ARCH, 1, 0),
            stmt(0x06, KILL),
            stmt(0x20, 0), // seccomp_data.nr
        ];
        // Only read(stdin) and write(stdout), not arbitrary descriptors.
        for (syscall, fd) in [(libc::SYS_read, 0), (libc::SYS_write, 1)] {
            filter.extend([
                jump(syscall as u32, 0, 4),
                stmt(0x20, 16), // args[0].low
                jump(fd, 0, 1),
                stmt(0x06, ALLOW),
                stmt(0x06, DENY),
            ]);
        }
        // No executable memory: this is an interpreter, never a JIT.
        for syscall in [libc::SYS_mmap, libc::SYS_mprotect] {
            filter.extend([
                jump(syscall as u32, 0, 4),
                stmt(0x20, 32), // args[2].low (prot)
                libc::sock_filter {
                    code: 0x45,
                    jt: 1,
                    jf: 0,
                    k: libc::PROT_EXEC as u32,
                },
                stmt(0x06, ALLOW),
                stmt(0x06, DENY),
            ]);
        }
        // Only private futex WAIT/WAKE/bitset variants (including realtime
        // WAIT_BITSET), and non-encoded local clock IDs. Shared futexes and
        // another process/thread's encoded CPU clock are not an IPC channel.
        for (syscall, argument, values) in [
            (libc::SYS_futex, 24, vec![128, 129, 137, 138, 393]),
            (libc::SYS_clock_gettime, 16, vec![0, 1, 2, 3]),
            (libc::SYS_clock_nanosleep, 16, vec![0, 1]),
        ] {
            filter.push(jump(syscall as u32, 0, (values.len() * 2 + 2) as u8));
            filter.push(stmt(0x20, argument));
            for value in values {
                filter.extend([jump(value, 0, 1), stmt(0x06, ALLOW)]);
            }
            filter.push(stmt(0x06, DENY));
        }
        for syscall in [
            libc::SYS_brk,
            libc::SYS_munmap,
            libc::SYS_mremap,
            libc::SYS_madvise,
            libc::SYS_nanosleep,
            libc::SYS_rt_sigreturn,
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigprocmask,
            libc::SYS_sigaltstack,
            libc::SYS_exit,
            libc::SYS_exit_group,
            libc::SYS_sched_yield,
            // Rust's keyed HashMap seeds and the engine's Math.random seed.
            libc::SYS_getrandom,
            // No poll/ioctl/fcntl/open/socket/clone/exec/ptrace/process_vm,
            // IPC, signals to other processes, filesystem metadata,
            // or resource-limit changes are admitted.
        ] {
            filter.extend([jump(syscall as u32, 0, 1), stmt(0x06, ALLOW)]);
        }
        filter.push(stmt(0x06, DENY));
        let program = libc::sock_fprog {
            len: filter.len().try_into().map_err(|_| Error::Protocol)?,
            filter: filter.as_mut_ptr(),
        };
        // SAFETY: this reads the current process identity before the filter
        // blocks process-query and signaling syscalls.
        let own_pid = unsafe { libc::getpid() };
        if own_pid <= 0 {
            return Err(Error::Unavailable("worker process identity unavailable"));
        }
        // SAFETY: kernel copies the initialized bounded BPF program during this
        // syscall. no_new_privs is irreversible; TSYNC applies to every thread.
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::syscall(libc::SYS_seccomp, 1, 1, &program) != 0
            {
                return Err(Error::Unavailable(
                    "kernel seccomp TSYNC policy unavailable",
                ));
            }
        }
        native_probes(own_pid)?;
        Ok(())
    }

    /// Run before Ready; each forbidden syscall must return EPERM. An outer
    /// policy or native privileges can also cause EPERM, so these checks
    /// complement review and isolated testing of the generated filter.
    fn native_probes(own_pid: libc::pid_t) -> Result<(), Error> {
        let word = 0i32;
        let remote_word = 42i32;
        let mut local_word = 0i32;
        let local = libc::iovec {
            iov_base: (&raw mut local_word).cast(),
            iov_len: std::mem::size_of::<i32>(),
        };
        let remote = libc::iovec {
            iov_base: (&raw const remote_word).cast_mut().cast(),
            iov_len: std::mem::size_of::<i32>(),
        };
        let forbidden = [
            (
                libc::SYS_openat,
                [
                    libc::AT_FDCWD as usize,
                    c"/etc/passwd".as_ptr() as usize,
                    libc::O_RDONLY as usize,
                    0,
                    0,
                    0,
                ],
            ),
            // Opening an existing file for write, without O_TRUNC/O_CREAT or a
            // write, cannot damage the host even if a broken policy admits it.
            (
                libc::SYS_openat,
                [
                    libc::AT_FDCWD as usize,
                    c"/etc/passwd".as_ptr() as usize,
                    libc::O_WRONLY as usize,
                    0,
                    0,
                    0,
                ],
            ),
            (
                libc::SYS_socket,
                [
                    libc::AF_INET as usize,
                    libc::SOCK_STREAM as usize,
                    0,
                    0,
                    0,
                    0,
                ],
            ),
            (
                libc::SYS_socket,
                [
                    libc::AF_INET as usize,
                    libc::SOCK_RAW as usize,
                    libc::IPPROTO_RAW as usize,
                    0,
                    0,
                    0,
                ],
            ),
            (
                libc::SYS_socket,
                [
                    libc::AF_UNIX as usize,
                    libc::SOCK_STREAM as usize,
                    0,
                    0,
                    0,
                    0,
                ],
            ),
            (
                libc::SYS_clone,
                [libc::CLONE_THREAD as usize, 0, 0, 0, 0, 0],
            ),
            (libc::SYS_execve, [0, 0, 0, 0, 0, 0]),
            (
                libc::SYS_ptrace,
                [libc::PTRACE_TRACEME as usize, 0, 0, 0, 0, 0],
            ),
            (libc::SYS_kill, [own_pid as usize, 0, 0, 0, 0, 0]),
            (
                libc::SYS_process_vm_readv,
                [
                    own_pid as usize,
                    &raw const local as usize,
                    1,
                    &raw const remote as usize,
                    1,
                    0,
                ],
            ),
            (
                libc::SYS_mmap,
                [
                    0,
                    4096,
                    (libc::PROT_READ | libc::PROT_EXEC) as usize,
                    (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as usize,
                    usize::MAX,
                    0,
                ],
            ),
            (
                libc::SYS_mprotect,
                [
                    0,
                    4096,
                    (libc::PROT_READ | libc::PROT_EXEC) as usize,
                    0,
                    0,
                    0,
                ],
            ),
            (libc::SYS_shmget, [0, 0, libc::IPC_CREAT as usize, 0, 0, 0]),
            (libc::SYS_eventfd2, [0; 6]),
            (libc::SYS_dup, [0; 6]),
            (libc::SYS_read, [3, 0, 0, 0, 0, 0]),
            (libc::SYS_write, [2, 0, 0, 0, 0, 0]),
            (
                libc::SYS_futex,
                [
                    &word as *const i32 as usize,
                    libc::FUTEX_WAKE as usize,
                    0,
                    0,
                    0,
                    0,
                ],
            ),
            (libc::SYS_clock_gettime, [u32::MAX as usize, 0, 0, 0, 0, 0]),
        ];
        for (syscall, a) in forbidden {
            // SAFETY: syscall must be rejected before argument dereference. The
            // pointer arguments used by openat are static C strings; the
            // process, signal, and IPC probes are harmless if not denied.
            let result = unsafe { libc::syscall(syscall, a[0], a[1], a[2], a[3], a[4], a[5]) };
            if result != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
                return Err(Error::Unavailable("active native denial probe failed"));
            }
        }
        // SAFETY: anonymous allocation probe, no dereference. A successful map
        // would contradict RLIMIT_AS and causes immediate qualification failure.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PROCESS_BYTES as usize + 1,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p != libc::MAP_FAILED {
            // SAFETY: p is exactly the mapping returned immediately above.
            unsafe {
                libc::munmap(p, PROCESS_BYTES as usize + 1);
            }
            return Err(Error::Unavailable(
                "hard process memory ceiling not enforced",
            ));
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
            return Err(Error::Unavailable(
                "hard process memory ceiling probe inconclusive",
            ));
        }
        Ok(())
    }
}

/// Supervisor-side memory ceiling for platforms whose kernel cannot enforce
/// one inside the worker. `Err` means the child could not be measured.
#[cfg(target_os = "macos")]
pub(crate) fn footprint_exceeded(pid: u32) -> Result<bool, Error> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: info is a correctly sized, writable rusage_info_v2 buffer for
    // the RUSAGE_INFO_V2 flavor; pid is this supervisor's own child.
    let status = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return Err(Error::Unavailable("worker memory footprint unavailable"));
    }
    // SAFETY: proc_pid_rusage initialized the buffer on success.
    let info = unsafe { info.assume_init() };
    Ok(info.ri_phys_footprint > crate::PROCESS_BYTES
        || info.ri_resident_size > crate::PROCESS_BYTES)
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::CPU_SECONDS;

    // Deny everything, including file, network, IPC, Mach service lookup,
    // signals, sysctl, fork, and exec. Reads and writes on the already-open
    // inherited pipes are not mediated by Seatbelt.
    const POLICY: &std::ffi::CStr = c"(version 1)\n(deny default)\n";

    extern "C" {
        fn sandbox_init(
            profile: *const libc::c_char,
            flags: u64,
            errorbuf: *mut *mut libc::c_char,
        ) -> libc::c_int;
        fn sandbox_free_error(errorbuf: *mut libc::c_char);
    }

    fn limit(resource: libc::c_int, value: u64) -> Result<(), Error> {
        let r = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: r points to an initialized rlimit; affects this child only.
        if unsafe { libc::setrlimit(resource, &r) } != 0 {
            return Err(Error::Unavailable(
                "required native hard resource limit failed",
            ));
        }
        let mut actual = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: actual is a valid output pointer.
        if unsafe { libc::getrlimit(resource, &mut actual) } != 0
            || actual.rlim_cur != value
            || actual.rlim_max != value
        {
            return Err(Error::Unavailable(
                "required native hard resource limit not active",
            ));
        }
        Ok(())
    }

    fn close_inherited_descriptors() -> Result<(), Error> {
        const ENTRY: usize = std::mem::size_of::<libc::proc_fdinfo>();
        let mut entries = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0,
            };
            4096
        ];
        // SAFETY: getpid has no arguments.
        let pid = unsafe { libc::getpid() };
        // SAFETY: entries is a writable buffer of the stated byte length.
        let bytes = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                entries.as_mut_ptr().cast(),
                (entries.len() * ENTRY) as libc::c_int,
            )
        };
        if bytes <= 0 || bytes as usize >= entries.len() * ENTRY {
            return Err(Error::Unavailable(
                "native descriptor/parent isolation failed",
            ));
        }
        for entry in &entries[..bytes as usize / ENTRY] {
            if entry.proc_fd > 1 {
                // SAFETY: closes an inherited descriptor this fresh worker
                // has not wrapped in any owned Rust handle.
                unsafe { libc::close(entry.proc_fd) };
            }
        }
        // SAFETY: as above; verifies only stdin/stdout remain.
        let bytes = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                entries.as_mut_ptr().cast(),
                (entries.len() * ENTRY) as libc::c_int,
            )
        };
        if bytes as usize != 2 * ENTRY || entries[..2].iter().any(|entry| entry.proc_fd > 1) {
            return Err(Error::Unavailable(
                "native descriptor/parent isolation failed",
            ));
        }
        Ok(())
    }

    /// CoreFoundation's initializer sets `__CF_USER_TEXT_ENCODING` in any
    /// binary linking it, before `main`, even after the supervisor's
    /// `env_clear`. Allow only that key with its `0xH:0xH:0xH` value shape.
    pub(super) fn cleared_environment(
        mut vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) -> bool {
        vars.all(|(key, value)| {
            key == "__CF_USER_TEXT_ENCODING"
                && value.to_str().is_some_and(|value| {
                    let parts: Vec<_> = value.split(':').collect();
                    parts.len() == 3
                        && parts.iter().all(|part| {
                            part.strip_prefix("0x").is_some_and(|hex| {
                                (1..=8).contains(&hex.len())
                                    && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                        })
                })
        })
    }

    pub(super) fn confine() -> Result<(), Error> {
        if !cleared_environment(std::env::vars_os()) {
            return Err(Error::Unavailable(
                "worker environment was not cleared by its supervisor",
            ));
        }
        for fd in [0, 1] {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: fstat initializes stat on success; fd is inherited stdio.
            if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
                return Err(Error::Unavailable("worker stdio is not an inherited pipe"));
            }
            // SAFETY: successful fstat above initialized every field.
            if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFIFO {
                return Err(Error::Unavailable("worker stdio is not an inherited pipe"));
            }
        }
        // SAFETY: scalar process queries; no borrowed memory escapes.
        if unsafe { libc::getppid() } <= 1 {
            return Err(Error::Unavailable(
                "native descriptor/parent isolation failed",
            ));
        }
        close_inherited_descriptors()?;
        // SAFETY: denies debugger attachment to this worker; scalar arguments.
        if unsafe { libc::ptrace(libc::PT_DENY_ATTACH, 0, std::ptr::null_mut(), 0) } != 0 {
            return Err(Error::Unavailable(
                "native descriptor/parent isolation failed",
            ));
        }
        limit(libc::RLIMIT_CPU, CPU_SECONDS)?;
        limit(libc::RLIMIT_CORE, 0)?;
        limit(libc::RLIMIT_FSIZE, 0)?;
        nonblocking(0)?;
        nonblocking(1)?;
        let mut error = std::ptr::null_mut();
        // SAFETY: POLICY is a NUL-terminated SBPL string; error is a valid
        // out pointer, freed with the matching sandbox_free_error.
        if unsafe { sandbox_init(POLICY.as_ptr(), 0, &mut error) } != 0 {
            if !error.is_null() {
                // SAFETY: allocated by sandbox_init above.
                unsafe { sandbox_free_error(error) };
            }
            return Err(Error::Unavailable("Seatbelt sandbox policy unavailable"));
        }
        native_probes()?;
        // After the probes, which need descriptor slots to reach the policy.
        limit(libc::RLIMIT_NOFILE, 2)?;
        Ok(())
    }

    fn denied(result: libc::c_int) -> bool {
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Run before Ready; each operation must be refused by the active policy.
    fn native_probes() -> Result<(), Error> {
        let mut failed = false;
        for (path, flags) in [
            (c"/etc/passwd", libc::O_RDONLY),
            (c"/etc/passwd", libc::O_WRONLY),
            (c"/tmp", libc::O_RDONLY),
        ] {
            // SAFETY: static C path; any unexpected descriptor is closed.
            let fd = unsafe { libc::open(path.as_ptr(), flags) };
            if fd >= 0 {
                // SAFETY: fd was returned by open immediately above.
                unsafe { libc::close(fd) };
            }
            failed |= !denied(fd);
        }
        // SAFETY: scalar socket arguments; a descriptor is closed after use.
        let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        if socket >= 0 {
            let address = libc::sockaddr_in {
                sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: 9u16.to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(std::net::Ipv4Addr::LOCALHOST).to_be(),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: address is a valid sockaddr_in of the stated length.
            let connected = unsafe {
                libc::connect(
                    socket,
                    (&raw const address).cast(),
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            };
            failed |= !denied(connected);
            // SAFETY: socket was returned by socket() above.
            unsafe { libc::close(socket) };
        } else {
            failed |= !denied(socket);
        }
        // SAFETY: signal 0 only checks permission; it delivers nothing.
        failed |= !denied(unsafe { libc::kill(libc::getppid(), 0) });
        // SAFETY: a fork that is wrongly admitted exits immediately without
        // running any parent state, and is then reaped.
        let child = unsafe { libc::fork() };
        if child == 0 {
            // SAFETY: async-signal-safe exit in the forked child.
            unsafe { libc::_exit(0) };
        }
        if child > 0 {
            let mut status = 0;
            // SAFETY: child is our own just-forked process.
            unsafe { libc::waitpid(child, &mut status, 0) };
        }
        failed |= !denied(child);
        let argv = [c"/usr/bin/true".as_ptr(), std::ptr::null()];
        let envp = [std::ptr::null()];
        // SAFETY: NUL-terminated argv/envp of static strings. A wrongly
        // admitted exec replaces the worker before Ready, which fails closed.
        failed |= !denied(unsafe { libc::execve(argv[0], argv.as_ptr(), envp.as_ptr()) });
        if failed {
            return Err(Error::Unavailable("active native denial probe failed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_worker_environment_allows_only_corefoundation_encoding() {
        let vars = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect::<Vec<_>>()
                .into_iter()
        };
        assert!(macos::cleared_environment(vars(&[])));
        assert!(macos::cleared_environment(vars(&[(
            "__CF_USER_TEXT_ENCODING",
            "0x1F5:0x0:0x0"
        )])));
        assert!(!macos::cleared_environment(vars(&[(
            "__CF_USER_TEXT_ENCODING",
            "0x1F5:0x0:0x0;rm"
        )])));
        assert!(!macos::cleared_environment(vars(&[("HOME", "/tmp")])));
        assert!(!macos::cleared_environment(vars(&[
            ("__CF_USER_TEXT_ENCODING", "0x1F5:0x0:0x0"),
            ("DYLD_INSERT_LIBRARIES", "/tmp/x.dylib"),
        ])));
    }

    #[test]
    fn unsupported_platforms_are_explicitly_unavailable() {
        if cfg!(all(
            target_os = "linux",
            any(target_arch = "aarch64", target_arch = "x86_64")
        )) {
            assert_eq!(availability().unwrap(), LINUX_PROFILE);
        } else if cfg!(target_os = "macos") {
            assert_eq!(availability().unwrap(), MACOS_PROFILE);
        } else if cfg!(target_os = "windows") {
            assert_eq!(availability().unwrap(), WINDOWS_PROFILE);
        } else {
            assert!(matches!(availability(), Err(Error::Unavailable(_))));
            assert!(matches!(confine(), Err(Error::Unavailable(_))));
        }
    }

    #[test]
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ))]
    fn confined_native_child() {
        let Ok(mode) = std::env::var("FESTERM_JS_NATIVE_PROBE") else {
            return;
        };
        std::env::remove_var("FESTERM_JS_NATIVE_PROBE");
        if mode == "fail-setup" {
            let r = libc::rlimit {
                rlim_cur: 64 * 1024 * 1024,
                rlim_max: 64 * 1024 * 1024,
            };
            // SAFETY: this is a freshly spawned owned test child, valid rlimit.
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_AS, &r) }, 0);
            assert!(matches!(confine(), Err(Error::Unavailable(_))));
            std::process::exit(77);
        }
        if let Err(error) = confine() {
            println!("{error}");
            std::process::exit(78);
        }
        if mode == "cpu" {
            // Simulate compromised native code that never calls QuickJS's
            // cooperative interrupt hook. Only the kernel CPU ceiling can stop it.
            loop {
                std::hint::spin_loop();
            }
        }
        std::process::exit(0);
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ))]
    #[test]
    fn native_adversarial_probes_and_cumulative_cpu_ceiling() {
        use std::{
            os::unix::process::ExitStatusExt,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        for (mode, code) in [
            ("deny-probes", Some(0)),
            ("fail-setup", Some(77)),
            ("cpu", None),
        ] {
            let start = Instant::now();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "platform::tests::confined_native_child",
                    "--nocapture",
                ])
                .env_clear()
                .env("FESTERM_JS_NATIVE_PROBE", mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if start.elapsed() >= Duration::from_secs(20) {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("owned native qualification child exceeded deadline: {mode}");
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(status.code(), code, "native probe: {mode}, {status}");
            if mode == "cpu" {
                assert_eq!(status.signal(), Some(libc::SIGKILL));
                assert!(start.elapsed() >= Duration::from_secs(4));
            }
        }
    }
}
