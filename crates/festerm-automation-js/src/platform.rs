//! Native enforcement lives only in this module. Unsupported platforms do not
//! instantiate the engine. A process, removed globals, or advisory RSS limit is
//! never reported as confinement.
use crate::Error;

pub const LINUX_PROFILE: &str = "linux-seccomp-stdio-rlimit-v1";

pub fn availability() -> Result<&'static str, Error> {
    if cfg!(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )) {
        // Actual kernel/rlimit qualification occurs inside the fresh child,
        // before Ready; this is only an architecture support check.
        Ok(LINUX_PROFILE)
    } else if cfg!(target_os = "macos") {
        Err(Error::Unavailable(
            "macOS has no qualified hard 128MiB process ceiling",
        ))
    } else if cfg!(target_os = "ios") {
        Err(Error::Unavailable(
            "iOS has no qualified separate helper/stdio transport",
        ))
    } else if cfg!(target_os = "windows") {
        Err(Error::Unavailable(
            "Windows AppContainer policy has not been qualified",
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_platforms_are_explicitly_unavailable() {
        if !cfg!(all(
            target_os = "linux",
            any(target_arch = "aarch64", target_arch = "x86_64")
        )) {
            assert!(matches!(availability(), Err(Error::Unavailable(_))));
            assert!(matches!(confine(), Err(Error::Unavailable(_))));
        } else {
            assert_eq!(availability().unwrap(), LINUX_PROFILE);
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
