use anyhow::Result;
use ash::{vk, Instance};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModifierEntry {
    pub modifier: u64,
    pub plane_count: u32,
    pub features: vk::FormatFeatureFlags,
}

pub fn query_supported(
    instance: &Instance,
    phys: vk::PhysicalDevice,
    format: vk::Format,
) -> Result<Vec<ModifierEntry>> {
    // Two-pass: first read the count, then allocate and refill.
    let mut list_count = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut props2 = vk::FormatProperties2::default().push_next(&mut list_count);
    unsafe {
        instance.get_physical_device_format_properties2(phys, format, &mut props2);
    }
    let count = list_count.drm_format_modifier_count as usize;
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut buf = vec![vk::DrmFormatModifierPropertiesEXT::default(); count];
    let mut list =
        vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut buf);
    let mut props2 = vk::FormatProperties2::default().push_next(&mut list);
    unsafe {
        instance.get_physical_device_format_properties2(phys, format, &mut props2);
    }
    let count = list.drm_format_modifier_count as usize;
    let used = buf.into_iter().take(count).map(|m| ModifierEntry {
        modifier: m.drm_format_modifier,
        plane_count: m.drm_format_modifier_plane_count,
        features: m.drm_format_modifier_tiling_features,
    });
    Ok(used.collect())
}

pub fn supports_clear_and_export(entry: &ModifierEntry) -> bool {
    let need = vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::TRANSFER_SRC;
    entry.features.contains(need)
}

pub fn format_modifier(m: u64) -> String {
    if m == 0 {
        return "LINEAR".into();
    }
    if m == fourcc_mod_invalid() {
        return "INVALID".into();
    }
    let vendor = (m >> 56) as u8;
    let val = m & ((1u64 << 56) - 1);
    let vendor_name = match vendor {
        0x00 => "NONE",
        0x01 => "INTEL",
        0x02 => "AMD",
        0x03 => "NVIDIA",
        0x04 => "SAMSUNG",
        0x05 => "QCOM",
        0x06 => "VIVANTE",
        0x07 => "BROADCOM",
        0x08 => "ARM",
        0x09 => "ALLWINNER",
        0x0A => "AMLOGIC",
        _ => "UNKNOWN",
    };
    format!("{vendor_name}({val:#x})")
}

pub const fn fourcc_mod_invalid() -> u64 {
    0x00ff_ffff_ffff_ffff
}
