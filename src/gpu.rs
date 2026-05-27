//! /dev/dri enumeration: collect every DRM node + its PCI/driver info,
//! surface to UI via the control plane, and let `RendererManager` map a
//! user-picked (major, minor) to a render-node path injected into
//! `Init.settings`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Plugin-settings key that persists "preferred GPU" as `"<major>:<minor>"`.
/// Daemon translates this to a `/dev/dri/renderD*` path at spawn time and
/// injects the path as the existing `render_node` setting that subprocess
/// plugins (notably `video`) already consume.
pub const GPU_DRM_DEV_KEY: &str = "gpu_drm_dev";

/// Settings key that flows to the renderer subprocess's Init.settings.
pub const RENDER_NODE_KEY: &str = "render_node";

pub fn parse_drm_dev(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

pub fn format_drm_dev(major: u32, minor: u32) -> String {
    format!("{major}:{minor}")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GpuInfo {
    pub render_node: Option<PathBuf>,
    pub primary_node: Option<PathBuf>,
    pub render_major: u32,
    pub render_minor: u32,
    pub primary_major: u32,
    pub primary_minor: u32,
    pub pci_bdf: Option<String>,
    pub vendor_id: u16,
    pub device_id: u16,
    pub driver: String,
    pub description: String,
}

impl GpuInfo {
    pub fn matches_render(&self, major: u32, minor: u32) -> bool {
        self.render_node.is_some() && self.render_major == major && self.render_minor == minor
    }
}

pub fn enumerate() -> Vec<GpuInfo> {
    enumerate_with_roots(Path::new("/dev/dri"), Path::new("/sys/dev/char"))
}

pub(crate) fn enumerate_with_roots(dev_dri: &Path, sysfs_char: &Path) -> Vec<GpuInfo> {
    let entries = match fs::read_dir(dev_dri) {
        Ok(it) => it,
        Err(e) => {
            log::warn!(
                "gpu::enumerate: read_dir({}) failed: {e}",
                dev_dri.display()
            );
            return Vec::new();
        }
    };

    // Group by PCI device directory so a single GPU's cardN + renderD1xx
    // collapse to one GpuInfo. Key = canonical path of
    // sysfs_char/<m>:<n>/device. Devices without a PCI parent (vgem etc.)
    // get their own group keyed by node path.
    let mut groups: BTreeMap<String, GpuInfo> = BTreeMap::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let kind = if name.starts_with("renderD") {
            NodeKind::Render
        } else if name.starts_with("card") {
            NodeKind::Primary
        } else {
            continue;
        };

        let (major, minor) = match stat_rdev(&path) {
            Some(t) => t,
            None => continue,
        };

        let pci = read_pci_for_node(sysfs_char, major, minor);
        let group_key = pci
            .as_ref()
            .and_then(|p| p.dir.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| path.display().to_string());

        let g = groups.entry(group_key).or_default();
        match kind {
            NodeKind::Render => {
                g.render_node = Some(path.clone());
                g.render_major = major;
                g.render_minor = minor;
            }
            NodeKind::Primary => {
                g.primary_node = Some(path.clone());
                g.primary_major = major;
                g.primary_minor = minor;
            }
        }
        if let Some(p) = pci {
            // Card + render in the same group resolve to the same PCI dir,
            // so overwriting is a no-op the second time around.
            g.pci_bdf = Some(p.bdf);
            g.vendor_id = p.vendor;
            g.device_id = p.device;
            g.driver = p.driver;
        }
    }

    let mut out: Vec<GpuInfo> = groups
        .into_values()
        .map(|mut g| {
            g.description = format_description(&g);
            g
        })
        .collect();
    // Stable order for UI: entries with a render node first, then by
    // render minor / primary minor.
    out.sort_by_key(|g| (g.render_node.is_none(), g.render_minor, g.primary_minor));
    out
}

enum NodeKind {
    Render,
    Primary,
}

struct Pci {
    dir: PathBuf,
    bdf: String,
    vendor: u16,
    device: u16,
    driver: String,
}

fn stat_rdev(p: &Path) -> Option<(u32, u32)> {
    let st = nix::sys::stat::stat(p).ok()?;
    let rdev = st.st_rdev as u64;
    Some((dev_major(rdev), dev_minor(rdev)))
}

// Linux glibc dev_t encoding (extended)
fn dev_major(rdev: u64) -> u32 {
    (((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfffu64)) as u32
}
fn dev_minor(rdev: u64) -> u32 {
    ((rdev & 0xff) | ((rdev >> 12) & !0xffu64)) as u32
}

fn read_pci_for_node(sysfs_char: &Path, major: u32, minor: u32) -> Option<Pci> {
    let link = sysfs_char.join(format!("{major}:{minor}")).join("device");
    parse_pci_dir(&link)
}

fn parse_pci_dir(device_link: &Path) -> Option<Pci> {
    let dir = fs::canonicalize(device_link).ok()?;
    let bdf = dir.file_name()?.to_str()?.to_string();

    let vendor = read_hex_u16(&dir.join("vendor"))?;
    let device = read_hex_u16(&dir.join("device"))?;
    let driver = read_driver(&dir).unwrap_or_default();

    Some(Pci {
        dir,
        bdf,
        vendor,
        device,
        driver,
    })
}

fn read_hex_u16(p: &Path) -> Option<u16> {
    let s = fs::read_to_string(p).ok()?;
    let s = s.trim();
    let s = s.strip_prefix("0x").unwrap_or(s);
    u16::from_str_radix(s, 16).ok()
}

fn read_driver(pci_dir: &Path) -> Option<String> {
    // /sys/.../device/driver -> ../../bus/pci/drivers/<name>
    let target = fs::read_link(pci_dir.join("driver")).ok()?;
    Some(target.file_name()?.to_str()?.to_string())
}

fn format_description(g: &GpuInfo) -> String {
    let driver = if g.driver.is_empty() {
        "unknown".to_string()
    } else {
        g.driver.clone()
    };
    if g.vendor_id == 0 && g.device_id == 0 {
        driver
    } else {
        format!("{driver} {:#06x}:{:#06x}", g.vendor_id, g.device_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Fake sysfs: an empty renderD128 in dev_dri, sysfs_char/<m>:<n>/device
    /// symlinked to a PCI dir with vendor/device/driver populated. mknod
    /// would need root, so this exercises parse_pci_dir only; the
    /// end-to-end enumerate path is covered by the `#[ignore]` live test.
    #[test]
    fn parse_pci_dir_reads_vendor_device_driver() {
        let tmp = tempfile::tempdir().unwrap();
        let pci = tmp.path().join("0000:03:00.0");
        fs::create_dir_all(&pci).unwrap();
        fs::write(pci.join("vendor"), "0x1002\n").unwrap();
        fs::write(pci.join("device"), "0x73bf\n").unwrap();
        let drivers = tmp.path().join("drivers/amdgpu");
        fs::create_dir_all(&drivers).unwrap();
        symlink(&drivers, pci.join("driver")).unwrap();

        let chardir = tmp.path().join("226-128");
        fs::create_dir_all(&chardir).unwrap();
        let device_link = chardir.join("device");
        symlink(&pci, &device_link).unwrap();

        let p = parse_pci_dir(&device_link).expect("parse");
        assert_eq!(p.bdf, "0000:03:00.0");
        assert_eq!(p.vendor, 0x1002);
        assert_eq!(p.device, 0x73bf);
        assert_eq!(p.driver, "amdgpu");
    }

    #[test]
    fn dev_major_minor_round_trip() {
        // makedev(226, 128) on Linux extended encoding = (226 << 8) | 128
        let rdev: u64 = (226u64 << 8) | 128u64;
        assert_eq!(dev_major(rdev), 226);
        assert_eq!(dev_minor(rdev), 128);
    }

    #[test]
    fn format_description_handles_unknown_pci() {
        let g = GpuInfo {
            driver: "vgem".to_string(),
            ..Default::default()
        };
        assert_eq!(format_description(&g), "vgem");
    }

    #[test]
    fn matches_render_requires_render_node() {
        let mut g = GpuInfo {
            render_major: 226,
            render_minor: 128,
            ..Default::default()
        };
        assert!(!g.matches_render(226, 128));
        g.render_node = Some(PathBuf::from("/dev/dri/renderD128"));
        assert!(g.matches_render(226, 128));
        assert!(!g.matches_render(226, 129));
    }

    #[test]
    #[ignore = "live: requires /dev/dri/renderD128"]
    fn live_enumerate_finds_a_gpu() {
        let v = enumerate();
        assert!(!v.is_empty(), "expected at least one GPU on this host");
        let any_render = v.iter().any(|g| g.render_node.is_some());
        assert!(any_render, "expected at least one render node");
    }
}
