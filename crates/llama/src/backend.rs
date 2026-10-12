//! GPU / backend detection.

use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Cuda12,
    Vulkan,
    Cpu,
}

impl Backend {
    pub fn dir_name(self) -> &'static str {
        match self {
            Backend::Cuda12 => "cuda12",
            Backend::Vulkan => "vulkan",
            Backend::Cpu => "cpu",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub backend: Backend,
    /// GPU name for the UI (None for CPU-only).
    pub gpu_name: Option<String>,
    pub driver_version: Option<String>,
}

/// Minimum NVIDIA driver (major.minor as major*100+minor) for the CUDA 12.4 builds: 551.61.
const MIN_CUDA12_DRIVER: f32 = 551.61;

/// Pure selection logic: feed it the raw outputs of the two probes.
/// `nvidia_smi`: stdout of `nvidia-smi --query-gpu=name,driver_version --format=csv,noheader`
/// (None if the command failed). `adapters`: one video-controller name per line.
pub fn select_backend(nvidia_smi: Option<&str>, adapters: Option<&str>) -> Detected {
    if let Some(out) = nvidia_smi {
        if let Some(line) = out.lines().map(str::trim).find(|l| !l.is_empty()) {
            let mut it = line.splitn(2, ',');
            let name = it.next().unwrap_or("").trim().to_string();
            let driver = it.next().map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
            let old_driver = driver
                .as_deref()
                .and_then(|d| d.parse::<f32>().ok())
                .is_some_and(|v| v < MIN_CUDA12_DRIVER);
            if !name.is_empty() && !old_driver {
                return Detected { backend: Backend::Cuda12, gpu_name: Some(name), driver_version: driver };
            }
            // Driver too old for CUDA 12.4: Vulkan still works on NVIDIA.
            if !name.is_empty() {
                return Detected { backend: Backend::Vulkan, gpu_name: Some(name), driver_version: driver };
            }
        }
    }
    if let Some(out) = adapters {
        let real = out
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !is_software_adapter(l));
        if let Some(name) = real {
            return Detected { backend: Backend::Vulkan, gpu_name: Some(name.to_string()), driver_version: None };
        }
    }
    Detected { backend: Backend::Cpu, gpu_name: None, driver_version: None }
}

fn is_software_adapter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("microsoft") || n.contains("remote") || n.contains("virtual") || n.contains("basic")
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Probe the machine (spawns `nvidia-smi` and PowerShell).
pub fn detect_backend() -> Detected {
    let nv = run("nvidia-smi", &["--query-gpu=name,driver_version", "--format=csv,noheader"]);
    let adapters = if nv.is_some() {
        None
    } else {
        run(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name",
            ],
        )
    };
    select_backend(nv.as_deref(), adapters.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_wins() {
        let d = select_backend(Some("NVIDIA GeForce RTX 4070, 560.94\n"), Some("Intel(R) UHD"));
        assert_eq!(d.backend, Backend::Cuda12);
        assert_eq!(d.gpu_name.as_deref(), Some("NVIDIA GeForce RTX 4070"));
        assert_eq!(d.driver_version.as_deref(), Some("560.94"));
    }

    #[test]
    fn old_nvidia_driver_goes_vulkan() {
        let d = select_backend(Some("NVIDIA GeForce GTX 1060, 472.12"), None);
        assert_eq!(d.backend, Backend::Vulkan);
        assert_eq!(d.gpu_name.as_deref(), Some("NVIDIA GeForce GTX 1060"));
    }

    #[test]
    fn vulkan_for_amd_intel_skipping_microsoft() {
        let d = select_backend(None, Some("Microsoft Basic Display Adapter\r\nAMD Radeon RX 6600\r\n"));
        assert_eq!(d.backend, Backend::Vulkan);
        assert_eq!(d.gpu_name.as_deref(), Some("AMD Radeon RX 6600"));
    }

    #[test]
    fn cpu_fallback() {
        assert_eq!(select_backend(None, None).backend, Backend::Cpu);
        assert_eq!(select_backend(Some("  \n"), Some("Microsoft Remote Display Adapter\n")).backend, Backend::Cpu);
        assert!(select_backend(None, Some("")).gpu_name.is_none());
    }
}
