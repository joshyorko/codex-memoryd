//! Identity and race-free signaling for the native lifecycle.
//!
//! A PID is only a locator. On Linux, retain a pidfd before reading identity,
//! check the boot, start time and a random per-launch marker, and signal only
//! through that same handle. No numeric PID is ever used to send a signal.

pub(crate) const MARKER_ENV: &str = "CODEX_MEMORYD_MANAGED_PROCESS_ID";
pub(crate) const STATE_PATH_ENV: &str = "CODEX_MEMORYD_MANAGED_PID_PATH";

#[cfg(target_os = "linux")]
mod platform {
    use super::{MARKER_ENV, STATE_PATH_ENV};
    use serde::{Deserialize, Serialize};
    use std::ffi::OsString;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
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
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
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
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
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
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::io;
    use std::path::Path;
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

pub(crate) use platform::{capture, clear, inspect, lock, preflight, write, Inspection, Paths};
