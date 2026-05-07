use anyhow::{anyhow, Result};
use ash::{vk, Instance};

use super::instance::DeviceMeta;

pub struct VkDevice {
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub mem_props: vk::PhysicalDeviceMemoryProperties,
    pub ext_mem_fd: ash::khr::external_memory_fd::Device,
    pub ext_sem_fd: ash::khr::external_semaphore_fd::Device,
    pub drm_mod: ash::ext::image_drm_format_modifier::Device,
    pub timeline: ash::khr::timeline_semaphore::Device,
}

impl Drop for VkDevice {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
        }
    }
}

const REQUIRED_EXTS: &[&[u8]] = &[
    b"VK_KHR_external_memory",
    b"VK_KHR_external_memory_fd",
    b"VK_EXT_external_memory_dma_buf",
    b"VK_EXT_image_drm_format_modifier",
    b"VK_KHR_external_semaphore",
    b"VK_KHR_external_semaphore_fd",
    b"VK_KHR_timeline_semaphore",
];

pub fn create(instance: &Instance, dev: &DeviceMeta) -> Result<VkDevice> {
    let families = unsafe { instance.get_physical_device_queue_family_properties(dev.phys) };
    let queue_family = families
        .iter()
        .enumerate()
        .find(|(_, f)| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .map(|(i, _)| i as u32)
        .ok_or_else(|| anyhow!("no graphics queue family on {}", dev.name))?;

    let avail = unsafe {
        instance
            .enumerate_device_extension_properties(dev.phys)
            .unwrap_or_default()
    };
    let mut missing: Vec<String> = Vec::new();
    for &needle in REQUIRED_EXTS {
        let present = avail.iter().any(|p| {
            let c = unsafe { std::ffi::CStr::from_ptr(p.extension_name.as_ptr()) };
            c.to_bytes() == needle
        });
        if !present {
            missing.push(String::from_utf8_lossy(needle).into_owned());
        }
    }
    if !missing.is_empty() {
        return Err(anyhow!(
            "device {} is missing required extensions: {:?}",
            dev.name,
            missing,
        ));
    }
    let ext_cstrs: Vec<&'static std::ffi::CStr> = vec![
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"VK_KHR_external_memory\0") },
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"VK_KHR_external_memory_fd\0") },
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"VK_EXT_external_memory_dma_buf\0") },
        unsafe {
            std::ffi::CStr::from_bytes_with_nul_unchecked(b"VK_EXT_image_drm_format_modifier\0")
        },
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"VK_KHR_external_semaphore\0") },
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"VK_KHR_external_semaphore_fd\0") },
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(b"VK_KHR_timeline_semaphore\0") },
    ];
    let ext_ptrs: Vec<*const i8> = ext_cstrs.iter().map(|c| c.as_ptr()).collect();

    let mut tl_feat = vk::PhysicalDeviceTimelineSemaphoreFeatures::default()
        .timeline_semaphore(true);
    let queue_priorities = [1.0f32];
    let queue_ci = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family)
        .queue_priorities(&queue_priorities)];
    let device = unsafe {
        instance
            .create_device(
                dev.phys,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queue_ci)
                    .enabled_extension_names(&ext_ptrs)
                    .push_next(&mut tl_feat),
                None,
            )
            .map_err(|e| anyhow!("vkCreateDevice on {}: {e}", dev.name))?
    };

    let queue = unsafe { device.get_device_queue(queue_family, 0) };
    let mem_props = unsafe { instance.get_physical_device_memory_properties(dev.phys) };
    let ext_mem_fd = ash::khr::external_memory_fd::Device::new(instance, &device);
    let ext_sem_fd = ash::khr::external_semaphore_fd::Device::new(instance, &device);
    let drm_mod = ash::ext::image_drm_format_modifier::Device::new(instance, &device);
    let timeline = ash::khr::timeline_semaphore::Device::new(instance, &device);

    Ok(VkDevice {
        device,
        queue,
        queue_family,
        mem_props,
        ext_mem_fd,
        ext_sem_fd,
        drm_mod,
        timeline,
    })
}

pub fn pick_memory_type(
    mem_props: &vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
    want_host_visible: bool,
) -> Result<u32> {
    for i in 0..mem_props.memory_type_count {
        if (type_bits & (1 << i)) == 0 {
            continue;
        }
        let f = mem_props.memory_types[i as usize].property_flags;
        let ok = if want_host_visible {
            f.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
        } else {
            f.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        };
        if ok {
            return Ok(i);
        }
    }
    Err(anyhow!(
        "no memory type satisfies type_bits=0x{type_bits:x}, host_visible={want_host_visible}"
    ))
}

// dma-buf import: the driver's fd_props already constrains type_bits to
// the legal set. NVIDIA exposes a dedicated "PRIME-importable" type that
// shows up as the lowest-indexed bit; picking the strictly DEVICE_LOCAL
// alternative (when both are present) makes vkAllocateMemory fail at
// import time. Prefer the lowest matching bit unconditionally.
pub fn pick_memory_type_for_import(
    mem_props: &vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
) -> Result<u32> {
    for i in 0..mem_props.memory_type_count {
        if (type_bits & (1 << i)) != 0 {
            return Ok(i);
        }
    }
    Err(anyhow!(
        "no memory type matches type_bits=0x{type_bits:x}"
    ))
}
