//! Identity and race-free signaling for the native lifecycle.
//!
//! A PID is only a locator. On Linux, retain a pidfd before reading identity,
//! check the boot, start time and a random per-launch marker, and signal only
//! through that same handle. No numeric PID is ever used to send a signal.

pub(crate) const MARKER_ENV: &str = "CODEX_MEMORYD_MANAGED_PROCESS_ID";
pub(crate) const STATE_PATH_ENV: &str = "CODEX_MEMORYD_MANAGED_PID_PATH";
pub(crate) const ACK_ENV: &str = "CODEX_MEMORYD_MANAGED_STARTUP_ACK";

#[cfg(target_os = "linux")]
mod platform {
    use super::{ACK_ENV, MARKER_ENV, STATE_PATH_ENV};
    use serde::{Deserialize, Serialize};
    use std::ffi::OsString;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Identity {
        version: u32,
        pid: u32,
        boot_id: String,
        start_ticks: u64,
        marker: String,
    }

    pub(crate) struct ManagedProcess {
        pid: u32,
        fd: OwnedFd,
    }

    pub(crate) enum Inspection {
        Stopped,
        Unverified(String),
        Running(ManagedProcess),
    }

    pub(crate) struct Lock(File);
    pub(crate) struct Paths {
        pid: PathBuf,
        identity: PathBuf,
        lock: PathBuf,
    }

    impl Paths {
        pub(crate) fn pid_path(&self) -> &Path {
            &self.pid
        }
        pub(crate) fn new(pid: &Path) -> io::Result<Self> {
            let parent = pid
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let parent = fs::canonicalize(parent)?;
            // Prevent a different user from replacing the state or any parent
            // directory. Sticky directories such as /tmp protect owned entries.
            let uid = unsafe { libc::geteuid() };
            for directory in parent.ancestors() {
                let m = fs::metadata(directory)?;
                // The filesystem root is the trusted anchor. Its owner can be
                // unmapped in a user namespace; it still must not be writable
                // by other users.
                if directory.parent().is_none() {
                    if !m.is_dir() || m.mode() & 0o022 != 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "untrusted filesystem root",
                        ));
                    }
                    continue;
                }
                if !m.is_dir()
                    || (m.uid() != uid && m.uid() != 0)
                    || (m.mode() & 0o022 != 0 && m.mode() & libc::S_ISVTX == 0)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("untrusted runtime state directory {}", directory.display()),
                    ));
                }
            }
            let name = pid.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "PID file has no filename")
            })?;
            let pid = parent.join(name);
            Ok(Self {
                identity: suffix(&pid, ".identity"),
                lock: suffix(&pid, ".lock"),
                pid,
            })
        }
    }

    fn suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut name = OsString::from(path.as_os_str());
        name.push(suffix);
        PathBuf::from(name)
    }

    fn open_state(path: &Path, private: bool) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            // Open before inspecting the descriptor; nonblocking prevents a
            // FIFO from hanging the lifecycle during that validation.
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        let forbidden = if private { 0o077 } else { 0o022 };
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & forbidden != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "runtime state has unsafe owner, mode, or file type",
            ));
        }
        Ok(file)
    }

    fn read_state(path: &Path, private: bool) -> io::Result<String> {
        let mut file = open_state(path, private)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file).take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runtime state is too large",
            ));
        }
        String::from_utf8(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "runtime state is not UTF-8"))
    }

    pub(crate) fn lock(paths: &Paths) -> io::Result<Lock> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&paths.lock)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "runtime lock has unsafe owner, mode, or file type",
            ));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Lock(file))
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }

    fn pidfd(pid: u32) -> io::Result<ManagedProcess> {
        let pid = libc::pid_t::try_from(pid)
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid PID"))?;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ManagedProcess {
            pid: pid as u32,
            fd: unsafe { OwnedFd::from_raw_fd(fd as i32) },
        })
    }

    impl ManagedProcess {
        pub(crate) fn pid(&self) -> u32 {
            self.pid
        }
        fn signal(&self, signal: i32) -> io::Result<()> {
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.fd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        fn exited(&self, timeout: Duration) -> io::Result<bool> {
            let mut pollfd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let rc = unsafe {
                libc::poll(
                    &mut pollfd,
                    1,
                    timeout.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            if pollfd.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::other("invalid pidfd"));
            }
            Ok(rc > 0 && pollfd.revents & (libc::POLLIN | libc::POLLHUP) != 0)
        }
        pub(crate) fn terminate_and_wait(&self) -> io::Result<()> {
            match self.signal(libc::SIGTERM) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
                Err(e) => return Err(e),
            }
            let deadline = Instant::now() + Duration::from_secs(4);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "process did not stop cleanly",
                    ));
                }
                match self.exited(remaining) {
                    Ok(true) => return Ok(()),
                    Ok(false) => continue,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
        }
    }

    fn boot_id() -> io::Result<String> {
        Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned())
    }
    fn start_ticks(pid: u32) -> io::Result<u64> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let fields = stat
            .rsplit_once(')')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))?
            .1;
        fields
            .split_whitespace()
            .nth(19)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing process start time")
            })?
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid process start time"))
    }
    fn marker_fields(pid: u32, marker: &str, path: &Path) -> io::Result<(bool, bool, usize)> {
        let env = fs::read(format!("/proc/{pid}/environ"))?;
        let expected = format!("{MARKER_ENV}={marker}");
        let mut expected_path = format!("{STATE_PATH_ENV}=").into_bytes();
        expected_path.extend_from_slice(path.as_os_str().as_bytes());
        Ok((
            env.split(|b| *b == 0)
                .any(|entry| entry == expected.as_bytes()),
            env.split(|b| *b == 0).any(|entry| entry == expected_path),
            env.len(),
        ))
    }
    fn missing_process(err: &io::Error) -> bool {
        err.raw_os_error() == Some(libc::ESRCH)
    }

    pub(crate) fn preflight() -> io::Result<()> {
        let handle = pidfd(std::process::id())?;
        handle.signal(0)?;
        let _ = boot_id()?;
        Ok(())
    }

    /// Launch the same executable image as the manager, pinned by a file
    /// descriptor. A wrapper or a different executable could start an
    /// untracked descendant before acknowledging, so reject it before spawn.
    pub(crate) fn verified_binary(path: &Path) -> io::Result<(File, PathBuf)> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(path)?;
        let candidate = file.metadata()?;
        let manager = fs::metadata("/proc/self/exe")?;
        if !candidate.is_file()
            || candidate.dev() != manager.dev()
            || candidate.ino() != manager.ino()
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput,
                "CODEX_MEMORYD_BIN must resolve to the running manager executable; wrappers and different binaries are not supported"));
        }
        let pinned_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        Ok((file, pinned_path))
    }

    pub(crate) fn startup_channel() -> io::Result<(File, Stdio)> {
        let mut fds = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let reader = unsafe { File::from_raw_fd(fds[0]) };
        let writer = unsafe { File::from_raw_fd(fds[1]) };
        let enabled: libc::c_int = 1;
        if unsafe {
            libc::setsockopt(
                reader.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&enabled as *const libc::c_int).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((reader, Stdio::from(writer)))
    }

    pub(crate) fn wait_for_startup_ack(
        reader: File,
        marker: &str,
        process: &ManagedProcess,
    ) -> io::Result<()> {
        let mut pollfd = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "daemon did not acknowledge its launch identity",
                ));
            }
            let rc = unsafe {
                libc::poll(
                    &mut pollfd,
                    1,
                    remaining.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if rc == 0 {
                continue;
            }
            let mut reply = [0_u8; 64];
            // Ancillary data must be aligned for cmsghdr/ucred. The kernel
            // supplies SCM_CREDENTIALS for the actual sender, including when
            // a socket descriptor was inherited by a descendant.
            let mut control = [0_usize; 8];
            let mut iov = libc::iovec {
                iov_base: reply.as_mut_ptr().cast(),
                iov_len: reply.len(),
            };
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = std::mem::size_of_val(&control);
            let count = unsafe { libc::recvmsg(reader.as_raw_fd(), &mut message, 0) };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated daemon acknowledgment",
                ));
            }
            let mut credential = None;
            let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
            while !header.is_null() {
                let hdr = unsafe { &*header };
                if hdr.cmsg_level == libc::SOL_SOCKET && hdr.cmsg_type == libc::SCM_CREDENTIALS {
                    if credential.is_some()
                        || hdr.cmsg_len
                            != unsafe {
                                libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as u32) as usize
                            }
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid daemon acknowledgment credentials",
                        ));
                    }
                    credential = Some(unsafe {
                        std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::ucred>())
                    });
                }
                header = unsafe { libc::CMSG_NXTHDR(&message, header) };
            }
            let sender = credential.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "daemon acknowledgment lacks sender credentials",
                )
            })?;
            if sender.pid != process.pid as libc::pid_t {
                return Err(io::Error::new(io::ErrorKind::InvalidData,
                    format!("daemon acknowledgment came from different pid {}; an untracked descendant may still be running", sender.pid)));
            }
            if &reply[..count as usize] == marker.as_bytes() && !process.exited(Duration::ZERO)? {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "daemon did not acknowledge its launch identity",
            ));
        }
    }

    pub(crate) fn acknowledge_startup() -> io::Result<()> {
        let marker = std::env::var(MARKER_ENV).ok();
        let channel = std::env::var_os(ACK_ENV).is_some();
        std::env::remove_var(MARKER_ENV);
        std::env::remove_var(STATE_PATH_ENV);
        std::env::remove_var(ACK_ENV);
        if !channel {
            return Ok(());
        }
        let marker = marker.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing managed launch identity",
            )
        })?;
        // The manager passes an anonymous credential-bearing socket as stdin. Acknowledge
        // only after removing the identity from the environment inherited by
        // subprocesses, then restore the normal /dev/null stdin.
        let mut pipe = unsafe { File::from_raw_fd(0) };
        let result = pipe.write_all(marker.as_bytes());
        drop(pipe);
        let null = File::open("/dev/null")?;
        if null.as_raw_fd() == 0 {
            let _ = null.into_raw_fd();
        } else if unsafe { libc::dup2(null.as_raw_fd(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        result
    }

    pub(crate) fn inspect(paths: &Paths) -> Inspection {
        match inspect_inner(paths) {
            Ok(state) => state,
            Err(e) => Inspection::Unverified(e.to_string()),
        }
    }
    fn inspect_inner(paths: &Paths) -> io::Result<Inspection> {
        let raw = match read_state(&paths.pid, false) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return match fs::symlink_metadata(&paths.identity) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Inspection::Stopped),
                    _ => Ok(Inspection::Unverified(
                        "identity exists without its PID file".into(),
                    )),
                };
            }
            Err(e) => return Err(e),
        };
        let pid: u32 = raw
            .trim()
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "malformed PID file"))?;
        let identity: Option<Identity> = match read_state(&paths.identity, true) {
            Ok(raw) => Some(serde_json::from_str(&raw).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "malformed process identity")
            })?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if let Some(identity) = &identity {
            if identity.version != 1
                || identity.pid != pid
                || uuid::Uuid::parse_str(&identity.marker).is_err()
            {
                return Ok(Inspection::Unverified(
                    "inconsistent process identity".into(),
                ));
            }
        }
        // Acquire the kernel handle before looking at any process identity.
        let handle = match pidfd(pid) {
            Ok(handle) => handle,
            Err(e) if missing_process(&e) => return Ok(Inspection::Stopped),
            Err(e) => return Err(e),
        };
        if handle.exited(Duration::ZERO)? {
            return Ok(Inspection::Stopped);
        }
        let Some(identity) = identity else {
            return Ok(Inspection::Unverified(
                "legacy PID file has no process identity".into(),
            ));
        };
        let evidence = (|| -> io::Result<bool> {
            if identity.boot_id != boot_id()? || identity.start_ticks != start_ticks(pid)? {
                return Ok(false);
            }
            let (marker, path, bytes) = marker_fields(pid, &identity.marker, &paths.pid)?;
            if bytes == 0 {
                return Err(io::Error::other("process environment is unavailable"));
            }
            Ok(marker && path)
        })();
        let matches = match evidence {
            Ok(matches) => matches,
            Err(_) if handle.exited(Duration::ZERO)? => return Ok(Inspection::Stopped),
            Err(err) => return Err(err),
        };
        if !matches {
            return Ok(Inspection::Stopped);
        }
        if handle.exited(Duration::ZERO)? {
            return Ok(Inspection::Stopped);
        }
        Ok(Inspection::Running(handle))
    }

    pub(crate) fn capture(
        paths: &Paths,
        pid: u32,
        marker: &str,
    ) -> io::Result<(ManagedProcess, String)> {
        let handle = pidfd(pid)?;
        let identity = Identity {
            version: 1,
            pid,
            boot_id: boot_id()?,
            start_ticks: start_ticks(pid)?,
            marker: marker.to_owned(),
        };
        // procfs may briefly expose an empty environment during exec. Keep
        // the same pidfd and wait for the spawned image; never signal an
        // unverified process if the evidence remains unavailable.
        let mut verified = false;
        for _ in 0..100 {
            if handle.exited(Duration::ZERO)? {
                return Err(io::Error::other("spawned process already exited"));
            }
            let (has_marker, has_path, _) = marker_fields(pid, marker, &paths.pid)?;
            if has_marker && has_path {
                verified = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !verified {
            return Err(io::Error::other(
                "spawned process launch identity did not match",
            ));
        }
        if handle.exited(Duration::ZERO)? {
            return Err(io::Error::other("spawned process already exited"));
        }
        let json = serde_json::to_string(&identity).map_err(io::Error::other)?;
        Ok((handle, json))
    }

    fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
        let temp = suffix(path, &format!(".tmp-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temp)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    pub(crate) fn write(paths: &Paths, pid: u32, identity: &str) -> io::Result<()> {
        write_atomic(&paths.identity, identity)?;
        write_atomic(&paths.pid, &pid.to_string())
    }
    pub(crate) fn clear(paths: &Paths) -> io::Result<()> {
        for path in [&paths.pid, &paths.identity] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::{Child, Command};

        struct Disposable(Child);
        impl Drop for Disposable {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        fn spawn(paths: &Paths, marker: &str) -> Disposable {
            Disposable(
                Command::new("/bin/sleep")
                    .arg("60")
                    .env(MARKER_ENV, marker)
                    .env(STATE_PATH_ENV, paths.pid_path())
                    .spawn()
                    .unwrap(),
            )
        }

        #[test]
        fn pid_reuse_with_same_start_tick_still_requires_launch_marker() {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::new(&dir.path().join("service.pid")).unwrap();
            let marker = uuid::Uuid::new_v4().to_string();
            let original = spawn(&paths, &marker);
            let (_, raw) = capture(&paths, original.0.id(), &marker).unwrap();
            let replacement = spawn(&paths, &uuid::Uuid::new_v4().to_string());
            let mut old: Identity = serde_json::from_str(&raw).unwrap();
            // Model PID reuse even in the same clock tick by deliberately
            // matching all public fields to the replacement; only the unique
            // launch marker still distinguishes the two processes.
            old.pid = replacement.0.id();
            old.start_ticks = start_ticks(old.pid).unwrap();
            write(&paths, old.pid, &serde_json::to_string(&old).unwrap()).unwrap();
            assert!(matches!(inspect(&paths), Inspection::Stopped));
        }

        #[test]
        fn copied_identity_cannot_control_another_runtime() {
            let dir = tempfile::tempdir().unwrap();
            let one = Paths::new(&dir.path().join("one.pid")).unwrap();
            let two = Paths::new(&dir.path().join("two.pid")).unwrap();
            let marker = uuid::Uuid::new_v4().to_string();
            let child = spawn(&one, &marker);
            let (_, raw) = capture(&one, child.0.id(), &marker).unwrap();
            write(&one, child.0.id(), &raw).unwrap();
            write(&two, child.0.id(), &raw).unwrap();
            assert!(matches!(inspect(&one), Inspection::Running(_)));
            assert!(matches!(inspect(&two), Inspection::Stopped));
        }

        #[test]
        fn pinned_handle_does_not_signal_after_original_exits() {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::new(&dir.path().join("service.pid")).unwrap();
            let marker = uuid::Uuid::new_v4().to_string();
            let mut original = spawn(&paths, &marker);
            let (handle, _) = capture(&paths, original.0.id(), &marker).unwrap();
            original.0.kill().unwrap();
            original.0.wait().unwrap();
            let mut replacement = spawn(&paths, &uuid::Uuid::new_v4().to_string());
            handle.terminate_and_wait().unwrap();
            assert!(replacement.0.try_wait().unwrap().is_none());
        }

        #[test]
        fn descendant_cannot_acknowledge_for_the_captured_process() {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::new(&dir.path().join("service.pid")).unwrap();
            let marker = uuid::Uuid::new_v4().to_string();
            let (reader, writer) = startup_channel().unwrap();
            // The background subshell inherits the acknowledgment socket and
            // sends the same marker, while its parent becomes a sleep process.
            let mut parent = Disposable(Command::new("/bin/sh")
                .args(["-c", "exec 3>&0; (printf '%s' \"$CODEX_MEMORYD_MANAGED_PROCESS_ID\" >&3) & exec /bin/sleep 60"])
                .env(MARKER_ENV, &marker)
                .env(STATE_PATH_ENV, paths.pid_path())
                .stdin(writer)
                .spawn().unwrap());
            let (handle, _) = capture(&paths, parent.0.id(), &marker).unwrap();
            let error = wait_for_startup_ack(reader, &marker, &handle)
                .expect_err("descendant must not certify its parent");
            assert!(error.to_string().contains("different pid"), "{error}");
            assert!(parent.0.try_wait().unwrap().is_none());
        }

        #[test]
        fn executable_remains_pinned_after_configured_symlink_changes() {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("configured-bin");
            std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &link).unwrap();
            let (_handle, pinned) = verified_binary(&link).unwrap();
            fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink("/bin/sleep", &link).unwrap();
            let target = fs::metadata(pinned).unwrap();
            let manager = fs::metadata("/proc/self/exe").unwrap();
            assert_eq!((target.dev(), target.ino()), (manager.dev(), manager.ino()));
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::{ACK_ENV, MARKER_ENV, STATE_PATH_ENV};
    use std::fs::File;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    pub(crate) struct ManagedProcess;
    pub(crate) enum Inspection {
        Stopped,
        Unverified(String),
        Running(ManagedProcess),
    }
    pub(crate) struct Lock;
    pub(crate) struct Paths;
    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "safe native process ownership is unavailable on this platform",
        )
    }
    impl Paths {
        pub(crate) fn new(_pid: &Path) -> io::Result<Self> {
            Ok(Self)
        }
        pub(crate) fn pid_path(&self) -> &Path {
            Path::new("")
        }
    }
    impl ManagedProcess {
        pub(crate) fn pid(&self) -> u32 {
            0
        }
        pub(crate) fn terminate_and_wait(&self) -> io::Result<()> {
            Err(unsupported())
        }
    }
    pub(crate) fn lock(_: &Paths) -> io::Result<Lock> {
        Err(unsupported())
    }
    pub(crate) fn preflight() -> io::Result<()> {
        Err(unsupported())
    }
    pub(crate) fn verified_binary(_: &Path) -> io::Result<(File, PathBuf)> {
        Err(unsupported())
    }
    pub(crate) fn startup_channel() -> io::Result<(File, Stdio)> {
        Err(unsupported())
    }
    pub(crate) fn wait_for_startup_ack(_: File, _: &str, _: &ManagedProcess) -> io::Result<()> {
        Err(unsupported())
    }
    pub(crate) fn acknowledge_startup() -> io::Result<()> {
        std::env::remove_var(MARKER_ENV);
        std::env::remove_var(STATE_PATH_ENV);
        std::env::remove_var(ACK_ENV);
        Ok(())
    }
    pub(crate) fn inspect(_: &Paths) -> Inspection {
        Inspection::Unverified(unsupported().to_string())
    }
    pub(crate) fn capture(_: &Paths, _: u32, _: &str) -> io::Result<(ManagedProcess, String)> {
        Err(unsupported())
    }
    pub(crate) fn write(_: &Paths, _: u32, _: &str) -> io::Result<()> {
        Err(unsupported())
    }
    pub(crate) fn clear(_: &Paths) -> io::Result<()> {
        Err(unsupported())
    }
}

pub(crate) use platform::{
    acknowledge_startup, capture, clear, inspect, lock, preflight, startup_channel,
    verified_binary, wait_for_startup_ack, write, Inspection, Paths,
};
