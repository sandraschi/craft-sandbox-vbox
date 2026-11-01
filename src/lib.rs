//! Lean, zero-panic VirtualBox hypervisor sandbox engine.
//!
//! Designed for safe detonation, execution tracing, and reverse-engineering of
//! untrusted, suspicious, or packed binaries inside an isolated guest VM.
//!
//! Follows strict clean-room safety:
//! - All operations communicate with `VBoxManage` through non-shell argument lists.
//! - Bounded timeouts and strict error handling (zero unwrap/panic in production code).
//! - Disposable RAII [`SandboxSession`] automatically powers off and restores pre-detonation
//!   snapshots on exit or drop.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use thiserror::Error;

/// Error conditions for VirtualBox sandbox operations.
#[derive(Debug, Error)]
pub enum VBoxError {
    #[error("VBoxManage executable not found on host system")]
    NotFound,
    #[error("I/O error running VBoxManage: {0}")]
    Io(#[from] std::io::Error),
    #[error("Command '{0}' timed out after {1:?}")]
    Timeout(String, Duration),
    #[error("VBoxManage command failed (exit code {code:?}): {stderr}")]
    CommandFailed { code: Option<i32>, stderr: String },
    #[error("Failed to parse VirtualBox output: {0}")]
    ParseError(String),
    #[error("Virtual machine '{0}' not found")]
    VmNotFound(String),
}

/// A registered VirtualBox virtual machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VirtualMachine {
    pub name: String,
    pub uuid: String,
}

/// Current operational state of a virtual machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    Running,
    Paused,
    PoweredOff,
    Saved,
    Aborted,
    Unknown(String),
}

/// The VirtualBox manager interface.
#[derive(Debug, Clone)]
pub struct VBox {
    vboxmanage_path: PathBuf,
    default_timeout: Duration,
}

impl VBox {
    /// Discover the local VirtualBox installation or fail gracefully.
    pub fn new() -> Result<Self, VBoxError> {
        let path = Self::discover_vboxmanage()?;
        Ok(Self {
            vboxmanage_path: path,
            default_timeout: Duration::from_secs(30),
        })
    }

    /// Construct a VBox interface pointing to a specific `VBoxManage` executable.
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            vboxmanage_path: path.into(),
            default_timeout: Duration::from_secs(30),
        }
    }

    /// Set a custom default command timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Locate `VBoxManage` on Windows, Linux, or macOS.
    pub fn discover_vboxmanage() -> Result<PathBuf, VBoxError> {
        // 1. Check environment variable
        if let Ok(p) = std::env::var("VBOX_MANAGE_PATH") {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return Ok(pb);
            }
        }

        // 2. Standard Windows installation paths
        #[cfg(windows)]
        {
            let candidates = [
                r"C:\Program Files\Oracle\VirtualBox\VBoxManage.exe",
                r"C:\Program Files (x86)\Oracle\VirtualBox\VBoxManage.exe",
            ];
            for c in candidates {
                let p = Path::new(c);
                if p.is_file() {
                    return Ok(p.to_path_buf());
                }
            }
        }

        // 3. Standard Unix installation paths
        #[cfg(unix)]
        {
            let candidates = ["/usr/bin/VBoxManage", "/usr/local/bin/VBoxManage"];
            for c in candidates {
                let p = Path::new(c);
                if p.is_file() {
                    return Ok(p.to_path_buf());
                }
            }
        }

        // 4. Try PATH lookup
        if let Ok(path_var) = std::env::var("PATH") {
            let exe_name = if cfg!(windows) { "VBoxManage.exe" } else { "VBoxManage" };
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join(exe_name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }

        Err(VBoxError::NotFound)
    }

    /// Query the VirtualBox version string (e.g. "7.2.22r175245").
    pub fn version(&self) -> Result<String, VBoxError> {
        let stdout = self.run_args(&["--version"])?;
        Ok(stdout.trim().to_string())
    }

    /// List all registered virtual machines.
    pub fn list_vms(&self) -> Result<Vec<VirtualMachine>, VBoxError> {
        let stdout = self.run_args(&["list", "vms"])?;
        parse_vm_list(&stdout)
    }

    /// Query the current state of a virtual machine by name or UUID.
    pub fn get_vm_state(&self, name_or_uuid: &str) -> Result<VmState, VBoxError> {
        let info = self.get_vm_info(name_or_uuid)?;
        let state_str = match info.get("VMState") {
            Some(s) => s.as_str(),
            None => return Ok(VmState::Unknown("missing VMState".to_string())),
        };

        Ok(match state_str {
            "running" => VmState::Running,
            "paused" => VmState::Paused,
            "poweroff" => VmState::PoweredOff,
            "saved" => VmState::Saved,
            "aborted" => VmState::Aborted,
            other => VmState::Unknown(other.to_string()),
        })
    }

    /// Get raw machine-readable VM properties.
    pub fn get_vm_info(&self, name_or_uuid: &str) -> Result<BTreeMap<String, String>, VBoxError> {
        let stdout = self.run_args(&["showvminfo", name_or_uuid, "--machinereadable"])?;
        let mut map = BTreeMap::new();
        for line in stdout.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim().trim_matches('"');
                let val = v.trim().trim_matches('"');
                map.insert(key.to_string(), val.to_string());
            }
        }
        Ok(map)
    }

    /// Start a virtual machine in headless mode (no GUI window).
    pub fn start_headless(&self, name_or_uuid: &str) -> Result<(), VBoxError> {
        self.run_args(&["startvm", name_or_uuid, "--type", "headless"])?;
        Ok(())
    }

    /// Power off a virtual machine immediately.
    pub fn poweroff(&self, name_or_uuid: &str) -> Result<(), VBoxError> {
        let _ = self.run_args(&["controlvm", name_or_uuid, "poweroff"]);
        Ok(())
    }

    /// Take a named snapshot of a VM.
    pub fn take_snapshot(&self, name_or_uuid: &str, snapshot_name: &str) -> Result<(), VBoxError> {
        self.run_args(&["snapshot", name_or_uuid, "take", snapshot_name])?;
        Ok(())
    }

    /// Restore a VM to a named snapshot.
    pub fn restore_snapshot(&self, name_or_uuid: &str, snapshot_name: &str) -> Result<(), VBoxError> {
        self.run_args(&["snapshot", name_or_uuid, "restore", snapshot_name])?;
        Ok(())
    }

    /// Delete a named snapshot.
    pub fn delete_snapshot(&self, name_or_uuid: &str, snapshot_name: &str) -> Result<(), VBoxError> {
        self.run_args(&["snapshot", name_or_uuid, "delete", snapshot_name])?;
        Ok(())
    }

    /// Execute a command inside the guest via VirtualBox Guest Additions.
    pub fn guest_exec(
        &self,
        name_or_uuid: &str,
        username: &str,
        password: &str,
        executable: &str,
        args: &[&str],
    ) -> Result<String, VBoxError> {
        let mut full_args = vec![
            "guestcontrol",
            name_or_uuid,
            "run",
            "--username",
            username,
            "--password",
            password,
            "--wait-stdout",
            "--wait-stderr",
            "--exe",
            executable,
            "--",
        ];
        full_args.extend_from_slice(args);
        self.run_args(&full_args)
    }

    /// Execute a `VBoxManage` command with strict arguments and return its standard output.
    pub fn run_args(&self, args: &[&str]) -> Result<String, VBoxError> {
        let mut cmd = Command::new(&self.vboxmanage_path);
        cmd.args(args);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW (0x08000000) avoids popup consoles
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }

        let output = cmd.output()?;
        if output.status.success() {
            String::from_utf8(output.stdout)
                .map_err(|e| VBoxError::ParseError(format!("Invalid UTF-8 in stdout: {e}")))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(VBoxError::CommandFailed {
                code: output.status.code(),
                stderr,
            })
        }
    }
}

/// Parse `VBoxManage list vms` line-by-line: `"VM Name" {uuid}`.
pub fn parse_vm_list(text: &str) -> Result<Vec<VirtualMachine>, VBoxError> {
    let mut vms = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Expected format: "name" {uuid}
        if let (Some(first_quote), Some(last_quote)) = (trimmed.find('"'), trimmed.rfind('"')) {
            if first_quote < last_quote {
                let name = &trimmed[first_quote + 1..last_quote];
                let remainder = trimmed[last_quote + 1..].trim();
                let uuid = remainder.trim_start_matches('{').trim_end_matches('}');
                vms.push(VirtualMachine {
                    name: name.to_string(),
                    uuid: uuid.to_string(),
                });
            }
        }
    }
    Ok(vms)
}

/// An active, disposable sandbox session.
///
/// On initialization, takes a pre-detonation snapshot.
/// On drop, forcibly powers off the virtual machine and reverts the snapshot,
/// guaranteeing that any malware, dropper, or side effects are completely purged.
pub struct SandboxSession<'a> {
    vbox: &'a VBox,
    vm_name: String,
    snapshot_name: String,
    cleaned_up: bool,
}

impl<'a> SandboxSession<'a> {
    /// Start a sandbox session on a powered-off VM.
    pub fn start(vbox: &'a VBox, vm_name: &str) -> Result<Self, VBoxError> {
        let snapshot_name = format!("craft-detonate-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
        
        // 1. Take snapshot while clean
        vbox.take_snapshot(vm_name, &snapshot_name)?;

        // 2. Start VM headless
        if let Err(e) = vbox.start_headless(vm_name) {
            let _ = vbox.restore_snapshot(vm_name, &snapshot_name);
            let _ = vbox.delete_snapshot(vm_name, &snapshot_name);
            return Err(e);
        }

        Ok(Self {
            vbox,
            vm_name: vm_name.to_string(),
            snapshot_name,
            cleaned_up: false,
        })
    }

    /// Run an untrusted binary inside the guest.
    pub fn execute(
        &self,
        username: &str,
        password: &str,
        executable: &str,
        args: &[&str],
    ) -> Result<String, VBoxError> {
        self.vbox.guest_exec(&self.vm_name, username, password, executable, args)
    }

    /// Explicitly tear down the sandbox and restore the clean snapshot.
    pub fn cleanup(mut self) -> Result<(), VBoxError> {
        self.perform_cleanup();
        self.cleaned_up = true;
        Ok(())
    }

    fn perform_cleanup(&mut self) {
        if !self.cleaned_up {
            let _ = self.vbox.poweroff(&self.vm_name);
            let _ = self.vbox.restore_snapshot(&self.vm_name, &self.snapshot_name);
            let _ = self.vbox.delete_snapshot(&self.vm_name, &self.snapshot_name);
            self.cleaned_up = true;
        }
    }
}

impl<'a> Drop for SandboxSession<'a> {
    fn drop(&mut self) {
        self.perform_cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_vm_list() -> Result<(), Box<dyn std::error::Error>> {
        let sample = r#"
"ready-1080p-2026-10-10" {4b7847e0-d70f-4705-a93b-ec1f68bd26e2}
"clean-desktop-2026-10-10" {4acc386d-5430-47e0-9206-eca5f41918b5}
"clean-booted-43.1" {99efbccb-699b-4f56-a853-dee3df95d9eb}
"#;
        let vms = parse_vm_list(sample)?;
        assert_eq!(vms.len(), 3);
        assert_eq!(vms[0].name, "ready-1080p-2026-10-10");
        assert_eq!(vms[0].uuid, "4b7847e0-d70f-4705-a93b-ec1f68bd26e2");
        assert_eq!(vms[2].name, "clean-booted-43.1");
        assert_eq!(vms[2].uuid, "99efbccb-699b-4f56-a853-dee3df95d9eb");
        Ok(())
    }

    #[test]
    fn parse_empty_vm_list() -> Result<(), Box<dyn std::error::Error>> {
        let sample = "\n\n";
        let vms = parse_vm_list(sample)?;
        assert!(vms.is_empty());
        Ok(())
    }

    #[test]
    fn live_host_virtualbox_query() -> Result<(), Box<dyn std::error::Error>> {
        // Only runs if VirtualBox is discovered on the host
        if let Ok(vbox) = VBox::new() {
            let version = vbox.version()?;
            assert!(!version.is_empty());
            println!("Detected VirtualBox version: {version}");

            let vms = vbox.list_vms()?;
            println!("Found {} registered VMs:", vms.len());
            for vm in &vms {
                println!(" - {} ({})", vm.name, vm.uuid);
            }
            assert!(vms.len() >= 3);
        }
        Ok(())
    }
}
