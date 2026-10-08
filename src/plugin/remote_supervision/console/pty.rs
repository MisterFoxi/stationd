//! Linux PTY. No shell, nonblocking master, separate session/process group.
//! The RAII owner kills the process group and reaps the TUI on every exit path.
#[cfg(target_os = "linux")]
mod linux {
    use std::{
        io,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::process::CommandExt,
        },
        process::{Child, Command, Stdio},
    };
    use tokio::io::unix::AsyncFd;
    pub(crate) struct Pty {
        master: AsyncFd<OwnedFd>,
        child: Child,
        pid: i32,
    }
    fn size(cols: u16, rows: u16) -> libc::winsize {
        libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
    impl Pty {
        pub(crate) fn spawn(
            program: &str,
            endpoint: &str,
            cols: u16,
            rows: u16,
        ) -> io::Result<Self> {
            let (mut master, mut slave) = (-1, -1);
            let dimensions = size(cols, rows);
            // SAFETY: openpty writes the two descriptors and reads a valid winsize.
            if unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &dimensions,
                )
            } == -1
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: ownership of both new descriptors is transferred exactly once.
            let master = unsafe { OwnedFd::from_raw_fd(master) };
            let slave = unsafe { OwnedFd::from_raw_fd(slave) };
            // Prevent descriptor inheritance and use readiness-based, bounded I/O.
            for descriptor in [&master, &slave] {
                if unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) }
                    == -1
                {
                    return Err(io::Error::last_os_error());
                }
            }
            if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
                return Err(io::Error::last_os_error());
            }
            let master = AsyncFd::new(master)?;
            let mut command = Command::new(program);
            command
                .args(["--addr", endpoint])
                .current_dir("/")
                .env_clear()
                .env("TERM", "xterm-256color")
                .env("LANG", "C.UTF-8")
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            // SAFETY: only async-signal-safe libc operations run between fork/exec.
            // Stdio has already been mapped onto descriptor zero by std::process.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn()?;
            let pid = child.id() as i32;
            Ok(Self { master, child, pid })
        }
        pub(crate) async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
            loop {
                let mut ready = self.master.readable().await?;
                match ready.try_io(|fd| {
                    let count = unsafe {
                        libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len())
                    };
                    if count < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(count as usize)
                    }
                }) {
                    Ok(result) => return result,
                    Err(_) => continue,
                }
            }
        }
        pub(crate) async fn write(&self, mut bytes: &[u8]) -> io::Result<()> {
            while !bytes.is_empty() {
                let mut ready = self.master.writable().await?;
                match ready.try_io(|fd| {
                    let count =
                        unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
                    if count < 0 {
                        Err(io::Error::last_os_error())
                    } else if count == 0 {
                        Err(io::ErrorKind::WriteZero.into())
                    } else {
                        Ok(count as usize)
                    }
                }) {
                    Ok(result) => bytes = &bytes[result?..],
                    Err(_) => continue,
                }
            }
            Ok(())
        }
        pub(crate) fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
            let dimensions = size(cols, rows);
            if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &dimensions) } == -1
            {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        pub(crate) fn exited(&mut self) -> bool {
            // Observe without reaping: retain the PID until Drop has killed descendants.
            let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as u32,
                    &mut status,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                ) == 0
                    && status.si_pid() != 0
            }
        }
    }
    impl Drop for Pty {
        fn drop(&mut self) {
            // Child/group IDs are fixed by setsid in our own child, never supplied by a browser.
            unsafe {
                libc::kill(-self.pid, libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
#[cfg(target_os = "linux")]
pub(super) use linux::Pty;

#[cfg(not(target_os = "linux"))]
pub(super) struct Pty;
#[cfg(not(target_os = "linux"))]
impl Pty {
    pub(crate) fn spawn(_: &str, _: &str, _: u16, _: u16) -> std::io::Result<Self> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(crate) async fn read(&self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(crate) async fn write(&self, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(crate) fn resize(&self, _: u16, _: u16) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    pub(crate) fn exited(&mut self) -> bool {
        true
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::Pty;
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    #[tokio::test]
    async fn webmin_console_pty_drop_kills_child_and_descendant_group() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("stationd-tui");
        let script = r#"#!/usr/bin/python3
import os,time
child=os.fork()
if child==0:
    while True: time.sleep(1)
os.write(1, ('%s,%s\n' % (os.getpid(),child)).encode())
while True: time.sleep(1)
"#;
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let terminal =
            Pty::spawn(program.to_str().unwrap(), "http://127.0.0.1:50051", 80, 24).unwrap();
        let mut buffer = [0; 128];
        let count = tokio::time::timeout(Duration::from_secs(2), terminal.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        let text = std::str::from_utf8(&buffer[..count]).unwrap();
        let ids: Vec<i32> = text
            .trim()
            .split(',')
            .map(|id| id.parse().unwrap())
            .collect();
        drop(terminal);
        assert_eq!(unsafe { libc::kill(ids[0], 0) }, -1, "TUI was not reaped");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                // A dead descendant can remain a zombie briefly until Linux PID 1 reaps it.
                let status = std::fs::read_to_string(format!("/proc/{}/stat", ids[1]));
                if status.is_err() || status.unwrap().contains(") Z ") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}
