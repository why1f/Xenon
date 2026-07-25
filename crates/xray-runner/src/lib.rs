//! Embedded Xray lifecycle and Linux memfd execution boundary.

use anyhow::Context;
use tokio::process::Child;

pub const EMBEDDED_VERSION: &str = env!("XRAY_EMBEDDED_VERSION");
pub const EMBEDDED_SHA256: &str = env!("XRAY_EMBEDDED_SHA256");
const EMBEDDED_BYTES: &[u8] = include_bytes!(env!("XRAY_EMBEDDED_PATH"));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum XrayStatus {
    #[default]
    NotConfigured,
    Running,
    Stopped,
    Failed,
}

#[derive(Debug, Default)]
pub struct XraySupervisor {
    status: XrayStatus,
    restart_count: u64,
    child: Option<Child>,
}

impl XraySupervisor {
    pub fn embedded_available() -> bool {
        env!("XRAY_EMBEDDED_AVAILABLE") == "1" && !EMBEDDED_BYTES.is_empty()
    }

    pub fn status(&self) -> XrayStatus {
        self.status
    }

    pub fn restart_count(&self) -> u64 {
        self.restart_count
    }

    pub async fn start(&mut self, config_json: &[u8]) -> anyhow::Result<()> {
        if !Self::embedded_available() {
            anyhow::bail!(
                "Xray-core v{EMBEDDED_VERSION} is not embedded in this development build"
            );
        }
        self.start_binary(EMBEDDED_BYTES, config_json).await
    }

    async fn start_binary(&mut self, binary: &[u8], config_json: &[u8]) -> anyhow::Result<()> {
        if self.child.is_some() {
            anyhow::bail!("Xray is already running");
        }
        let child = match platform::spawn(binary, config_json).await {
            Ok(child) => child,
            Err(error) => {
                self.status = XrayStatus::Failed;
                return Err(error).context("spawn embedded Xray");
            }
        };
        if self.status != XrayStatus::NotConfigured {
            self.restart_count = self.restart_count.saturating_add(1);
        }
        self.child = Some(child);
        self.status = XrayStatus::Running;
        Ok(())
    }

    pub fn poll(&mut self) -> anyhow::Result<Option<std::process::ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        let Some(exit) = child.try_wait().context("poll Xray child")? else {
            return Ok(None);
        };
        self.child = None;
        self.status = if exit.success() {
            XrayStatus::Stopped
        } else {
            XrayStatus::Failed
        };
        Ok(Some(exit))
    }

    pub async fn stop(&mut self) -> anyhow::Result<()> {
        let Some(mut child) = self.child.take() else {
            self.status = XrayStatus::Stopped;
            return Ok(());
        };
        child.start_kill().context("signal Xray child")?;
        child.wait().await.context("wait for Xray child")?;
        self.status = XrayStatus::Stopped;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::{
        ffi::CString,
        fs::File,
        io::Write,
        os::fd::{AsRawFd, FromRawFd},
        process::Stdio,
    };
    use tokio::process::{Child, Command};

    pub async fn spawn(binary: &[u8], config_json: &[u8]) -> anyhow::Result<Child> {
        let binary_fd = create_sealed_memfd("xray-core", binary)?;
        let config_fd = create_sealed_memfd("xray-config", config_json)?;
        clear_close_on_exec(binary_fd.as_raw_fd())?;
        clear_close_on_exec(config_fd.as_raw_fd())?;

        let binary_path = format!("/proc/self/fd/{}", binary_fd.as_raw_fd());
        let config_path = format!("/proc/self/fd/{}", config_fd.as_raw_fd());
        let mut command = Command::new(binary_path);
        command
            .arg("run")
            .arg("-format")
            .arg("json")
            .arg("-config")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        // SAFETY: pre_exec only calls the async-signal-safe prctl syscall and
        // does not allocate or touch shared Rust state in the child.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(binary_fd);
        drop(config_fd);
        Ok(child)
    }

    fn create_sealed_memfd(name: &str, bytes: &[u8]) -> anyhow::Result<File> {
        let name = CString::new(name)?;
        // Call the kernel directly so release binaries do not require glibc's
        // memfd_create wrapper, which was only added in glibc 2.27.
        // SAFETY: name is NUL-terminated and the remaining arguments match the
        // Linux memfd_create syscall ABI.
        let result = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let fd = result as std::os::fd::RawFd;
        // SAFETY: fd was just created successfully and has no other owner.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes)?;
        file.flush()?;
        let mode_result = unsafe { libc::fchmod(file.as_raw_fd(), 0o500) };
        if mode_result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let seals =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        let seal_result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) };
        if seal_result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(file)
    }

    fn clear_close_on_exec(fd: std::os::fd::RawFd) -> anyhow::Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use tokio::process::Child;

    pub async fn spawn(_binary: &[u8], _config_json: &[u8]) -> anyhow::Result<Child> {
        anyhow::bail!("embedded Xray execution is supported only on Linux")
    }
}

#[cfg(test)]
mod tests {
    use super::{XrayStatus, XraySupervisor, EMBEDDED_VERSION};

    #[test]
    fn development_build_reports_missing_asset() {
        assert_eq!(EMBEDDED_VERSION, "26.6.27");
        let supervisor = XraySupervisor::default();
        assert_eq!(supervisor.status(), XrayStatus::NotConfigured);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn memfd_supervisor_restarts_after_embedded_child_exit() {
        let mut supervisor = XraySupervisor::default();
        let fake_xray = b"#!/bin/sh
[ \"$1\" = run ] || exit 10
[ \"$2\" = -format ] || exit 11
[ \"$3\" = json ] || exit 12
[ \"$4\" = -config ] || exit 13
[ -r \"$5\" ] || exit 14
exit 23
";
        supervisor
            .start_binary(fake_xray, b"{}")
            .await
            .expect("spawn fake embedded process");
        for _ in 0..20 {
            if supervisor.poll().expect("poll child").is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(supervisor.status(), XrayStatus::Failed);
        assert_eq!(supervisor.restart_count(), 0);
        supervisor
            .start_binary(fake_xray, b"{}")
            .await
            .expect("restart fake embedded process");
        assert_eq!(supervisor.restart_count(), 1);
        supervisor.stop().await.expect("stop restarted process");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn embedded_xray_accepts_json_config_from_memfd() {
        if !XraySupervisor::embedded_available() {
            return;
        }
        let mut supervisor = XraySupervisor::default();
        supervisor
            .start(br#"{"log":{"loglevel":"none"},"inbounds":[],"outbounds":[]}"#)
            .await
            .expect("start embedded Xray with memfd JSON config");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let exit = supervisor.poll().expect("poll embedded Xray");
        assert!(
            exit.is_none(),
            "embedded Xray exited during startup: {exit:?}"
        );
        supervisor.stop().await.expect("stop embedded Xray");
    }
}
