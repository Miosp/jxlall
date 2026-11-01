//! Which execution backends exist on a platform and in which order to try them.

use std::path::Path;

const NVIDIA_PCI_VENDOR: &str = "0x10de";
const PCI_DISPLAY_CLASS_PREFIX: &str = "0x03";
const LINUX_PCI_DEVICES: &str = "/sys/bus/pci/devices";

/// A concrete execution backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    TensorrtRtx,
    Directml,
    Webgpu,
    Cpu,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::TensorrtRtx => "tensorrt-rtx",
            Self::Directml => "directml",
            Self::Webgpu => "webgpu",
            Self::Cpu => "cpu",
        }
    }
}

/// `--ai-backend` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum BackendChoice {
    /// Best available: tries GPU backends, falls back to CPU.
    Auto,
    /// NVIDIA RTX GPUs.
    TensorrtRtx,
    /// Any DirectX 12 GPU, Windows only.
    Directml,
    /// Vulkan, DirectX 12 or Metal GPUs.
    Webgpu,
    Cpu,
}

impl BackendChoice {
    fn explicit(self) -> Option<Backend> {
        match self {
            Self::Auto => None,
            Self::TensorrtRtx => Some(Backend::TensorrtRtx),
            Self::Directml => Some(Backend::Directml),
            Self::Webgpu => Some(Backend::Webgpu),
            Self::Cpu => Some(Backend::Cpu),
        }
    }
}

/// Backends on a platform, most capable first. Empty: no AI support there.
pub fn order(os: &str, arch: &str) -> &'static [Backend] {
    use Backend::{Cpu, Directml, TensorrtRtx, Webgpu};
    match (os, arch) {
        ("linux", "x86_64" | "aarch64") => &[TensorrtRtx, Webgpu, Cpu],
        ("windows", "x86_64" | "aarch64") => &[TensorrtRtx, Directml, Webgpu, Cpu],
        ("macos", "aarch64") => &[Webgpu, Cpu],
        _ => &[],
    }
}

/// Backends to try for a choice. `auto` leaves out TensorRT RTX without an
/// NVIDIA GPU, so other machines never fetch its large package.
pub fn attempts(
    choice: BackendChoice,
    os: &str,
    arch: &str,
    nvidia: bool,
) -> Result<Vec<Backend>, String> {
    let available = order(os, arch);
    if available.is_empty() {
        return Err(format!("no ONNX Runtime build for {os} {arch}"));
    }
    match choice.explicit() {
        Some(backend) if available.contains(&backend) => Ok(vec![backend]),
        Some(backend) => Err(format!(
            "backend {} is not available on {os} {arch}",
            backend.name()
        )),
        None => Ok(available
            .iter()
            .copied()
            .filter(|&b| nvidia || b != Backend::TensorrtRtx)
            .collect()),
    }
}

/// Any display-class PCI device from NVIDIA under a sysfs-style directory.
fn has_nvidia_pci_device(devices: &Path) -> bool {
    let attribute = |dir: &Path, name: &str| std::fs::read_to_string(dir.join(name));
    std::fs::read_dir(devices).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let dir = entry.path();
            attribute(&dir, "vendor").is_ok_and(|v| v.trim() == NVIDIA_PCI_VENDOR)
                && attribute(&dir, "class")
                    .is_ok_and(|c| c.trim().starts_with(PCI_DISPLAY_CLASS_PREFIX))
        })
    })
}

/// Cheap check before a large download; the plugin makes the final decision.
pub fn nvidia_gpu_present() -> bool {
    if cfg!(target_os = "linux") {
        has_nvidia_pci_device(Path::new(LINUX_PCI_DEVICES))
    } else if cfg!(windows) {
        std::env::var_os("SystemRoot").is_some_and(|root| {
            Path::new(&root)
                .join("System32")
                .join("nvcuda.dll")
                .is_file()
        })
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Backend::{Cpu, Directml, TensorrtRtx, Webgpu};

    #[test]
    fn order_per_platform() {
        for arch in ["x86_64", "aarch64"] {
            assert_eq!(order("linux", arch), [TensorrtRtx, Webgpu, Cpu]);
            assert_eq!(order("windows", arch), [TensorrtRtx, Directml, Webgpu, Cpu]);
        }
        assert_eq!(order("macos", "aarch64"), [Webgpu, Cpu]);
        assert!(order("macos", "x86_64").is_empty());
        assert!(order("freebsd", "x86_64").is_empty());
    }

    #[test]
    fn auto_skips_tensorrt_without_nvidia() {
        let without = attempts(BackendChoice::Auto, "linux", "x86_64", false).unwrap();
        assert_eq!(without, [Webgpu, Cpu]);
        let with = attempts(BackendChoice::Auto, "linux", "x86_64", true).unwrap();
        assert_eq!(with, [TensorrtRtx, Webgpu, Cpu]);
    }

    #[test]
    fn explicit_backend_is_the_only_attempt_even_without_nvidia() {
        let got = attempts(BackendChoice::TensorrtRtx, "linux", "x86_64", false).unwrap();
        assert_eq!(got, [TensorrtRtx]);
    }

    #[test]
    fn explicit_backend_missing_on_the_platform_is_an_error() {
        let linux = attempts(BackendChoice::Directml, "linux", "x86_64", true);
        assert!(linux.unwrap_err().contains("directml"));
        let mac = attempts(BackendChoice::TensorrtRtx, "macos", "aarch64", true);
        assert!(mac.is_err());
    }

    #[test]
    fn unsupported_platform_is_an_error() {
        assert!(attempts(BackendChoice::Auto, "macos", "x86_64", false).is_err());
        assert!(attempts(BackendChoice::Cpu, "macos", "x86_64", false).is_err());
    }

    fn device(root: &std::path::Path, name: &str, vendor: &str, class: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vendor"), format!("{vendor}\n")).unwrap();
        std::fs::write(dir.join("class"), format!("{class}\n")).unwrap();
    }

    #[test]
    fn pci_scan_needs_an_nvidia_display_device() {
        let root = std::env::temp_dir().join(format!("jxlall-pci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        device(&root, "gpu-intel", "0x8086", "0x030000");
        device(&root, "nvidia-audio", "0x10de", "0x040300");
        assert!(!has_nvidia_pci_device(&root));
        device(&root, "gpu-nvidia", "0x10de", "0x030000");
        assert!(has_nvidia_pci_device(&root));
        std::fs::remove_dir_all(&root).unwrap();
        assert!(!has_nvidia_pci_device(&root));
    }
}
