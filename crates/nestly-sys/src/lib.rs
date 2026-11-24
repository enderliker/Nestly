//! The only crate in nestly allowed to contain `unsafe` code.
//!
//! Everything here is a small, auditable wrapper around Linux process and
//! socket primitives. All public APIs are safe; the invariants are documented
//! at each `unsafe` block.
#![deny(unsafe_op_in_unsafe_fn)]
#![doc = "Linux process and socket hardening primitives."]

use std::io;
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// File descriptor used to pass the supervisor IPC socket to a child.
pub const IPC_FD: RawFd = 3;

/// Linux peer credentials for a Unix socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

/// Hardening applied to a child after `fork` and before `exec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildHardening {
    pub parent_death_signal: bool,
    pub no_new_privs: bool,
    pub non_dumpable: bool,
    pub core_dump_disabled: bool,
    pub nofile_limit: Option<u64>,
    pub address_space_mb: Option<u64>,
    pub umask: Option<u32>,
}

impl Default for ChildHardening {
    fn default() -> Self {
        Self {
            parent_death_signal: true,
            no_new_privs: true,
            non_dumpable: true,
            core_dump_disabled: true,
            nofile_limit: Some(1024),
            address_space_mb: None,
            umask: Some(0o077),
        }
    }
}

/// Configure a [`Command`] to inherit `child_fd` as [`IPC_FD`] and apply
/// process hardening in the child.
///
/// # Errors
///
/// Returns an error when registering the `pre_exec` hook fails.
pub fn configure_child_command(
    command: &mut Command,
    child_fd: RawFd,
    hardening: ChildHardening,
) -> io::Result<()> {
    // SAFETY: the closure runs in the forked child before exec. It only calls
    // async-signal-safe libc functions and returns an owned io::Error.
    unsafe {
        command.pre_exec(move || {
            if child_fd != IPC_FD && libc::dup2(child_fd, IPC_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            clear_cloexec(IPC_FD)?;
            apply_hardening(hardening)?;
            Ok(())
        });
    }
    Ok(())
}

fn clear_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl with F_GETFD/F_SETFD is safe for any integer fd; errors are
    // reported through the return value and errno.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: same as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn apply_hardening(hardening: ChildHardening) -> io::Result<()> {
    if hardening.parent_death_signal {
        prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong)?;
        // SAFETY: getppid has no preconditions.
        if unsafe { libc::getppid() } == 1 {
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
    }
    if hardening.no_new_privs {
        prctl(libc::PR_SET_NO_NEW_PRIVS, 1)?;
    }
    if hardening.non_dumpable {
        prctl(libc::PR_SET_DUMPABLE, 0)?;
    }
    if hardening.core_dump_disabled {
        set_rlimit(libc::RLIMIT_CORE, 0, 0)?;
    }
    if let Some(nofile) = hardening.nofile_limit {
        set_rlimit(libc::RLIMIT_NOFILE, nofile, nofile)?;
    }
    if let Some(address_space_mb) = hardening.address_space_mb {
        let bytes = address_space_mb.saturating_mul(1024 * 1024);
        set_rlimit(libc::RLIMIT_AS, bytes, bytes)?;
    }
    if let Some(mask) = hardening.umask {
        // SAFETY: umask only changes the process file mode mask.
        unsafe {
            libc::umask(mask as libc::mode_t);
        }
    }
    Ok(())
}

fn prctl(option: libc::c_int, value: libc::c_ulong) -> io::Result<()> {
    // SAFETY: prctl with these options and integer arguments has no memory
    // safety preconditions. Extra arguments are zero.
    let result = unsafe { libc::prctl(option, value, 0, 0, 0) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
type RlimitResource = libc::c_int;

fn set_rlimit(resource: RlimitResource, soft: u64, hard: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft as libc::rlim_t,
        rlim_max: hard as libc::rlim_t,
    };
    // SAFETY: `limit` is a valid rlimit for the duration of the call.
    let result = unsafe { libc::setrlimit(resource, &limit) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Harden the supervisor process itself.
///
/// # Errors
///
/// Returns an error when a kernel control is unavailable.
pub fn harden_self() -> io::Result<()> {
    prctl(libc::PR_SET_DUMPABLE, 0)?;
    prctl(libc::PR_SET_NO_NEW_PRIVS, 1)?;
    set_rlimit(libc::RLIMIT_CORE, 0, 0)?;
    // SAFETY: umask only changes the process file mode mask.
    unsafe {
        libc::umask(0o077);
    }
    Ok(())
}

/// Ask the kernel to terminate this process when its parent exits.
///
/// # Errors
///
/// Returns an error when `prctl` fails.
pub fn set_parent_death_signal() -> io::Result<()> {
    prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong)
}

/// Take ownership of the inherited IPC file descriptor.
///
/// # Errors
///
/// Returns an error when fd 3 is not a Unix socket or cannot be made
/// nonblocking.
pub fn take_inherited_ipc() -> io::Result<StdUnixStream> {
    // SAFETY: fstat only writes into the provided struct.
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` points to valid writable memory.
    let result = unsafe { libc::fstat(IPC_FD, stat.as_mut_ptr()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat initialized the struct on success.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inherited fd is not a Unix socket",
        ));
    }
    // SAFETY: fd 3 is a socket owned by this process. Ownership is transferred
    // exactly once to the returned stream.
    let stream = unsafe { StdUnixStream::from_raw_fd(IPC_FD) };
    stream.set_nonblocking(true)?;
    Ok(stream)
}

/// Read `SO_PEERCRED` from a Unix socket.
///
/// # Errors
///
/// Returns an error when `getsockopt` fails.
pub fn peer_credentials<F: AsRawFd>(socket: &F) -> io::Result<PeerCredentials> {
    // SAFETY: zeroed ucred is a valid initial value.
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the pointer and length describe a valid ucred buffer.
    let result = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCredentials {
        pid: credentials.pid as u32,
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

/// Current effective user id.
#[must_use]
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::{ChildHardening, peer_credentials};

    #[test]
    fn defaults_are_restrictive() {
        let hardening = ChildHardening::default();
        assert!(hardening.no_new_privs);
        assert!(hardening.non_dumpable);
        assert!(hardening.core_dump_disabled);
        assert_eq!(hardening.umask, Some(0o077));
    }

    #[test]
    fn reads_peer_credentials() {
        let (left, _right) = UnixStream::pair().unwrap();
        let credentials = peer_credentials(&left).unwrap();
        assert_eq!(credentials.uid, super::effective_uid());
        assert!(credentials.pid > 0);
    }
}
