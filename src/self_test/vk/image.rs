use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};

use anyhow::{anyhow, Result};
use ash::vk;

use super::device::{pick_memory_type, VkDevice};

pub struct OwnedImage {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub width: u32,
    pub height: u32,
    pub modifier: u64,
    pub plane0_stride: u64,
    pub plane0_offset: u64,
    pub plane0_size: u64,
}

pub fn create_with_modifiers(
    vkd: &VkDevice,
    width: u32,
    height: u32,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
    mod_list: &[u64],
    want_host_visible: bool,
) -> Result<OwnedImage> {
    let mut mods_ci =
        vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(mod_list);
    let mut ext_info = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

    let image = unsafe {
        vkd.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                .usage(usage)
                .push_next(&mut mods_ci)
                .push_next(&mut ext_info),
            None,
        )
    }
    .map_err(|e| anyhow!("vkCreateImage(modifier_list): {e}"))?;

    let req = unsafe { vkd.device.get_image_memory_requirements(image) };
    let mtype = pick_memory_type(&vkd.mem_props, req.memory_type_bits, want_host_visible)?;

    let mut exp = vk::ExportMemoryAllocateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut ded = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let memory = unsafe {
        vkd.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(mtype)
                .push_next(&mut ded)
                .push_next(&mut exp),
            None,
        )
    }
    .map_err(|e| anyhow!("vkAllocateMemory(dma-buf export): {e}"))?;
    unsafe {
        vkd.device.bind_image_memory(image, memory, 0).map_err(|e| {
            anyhow!("vkBindImageMemory: {e}")
        })?;
    }

    let mut props = vk::ImageDrmFormatModifierPropertiesEXT::default();
    unsafe {
        vkd.drm_mod
            .get_image_drm_format_modifier_properties(image, &mut props)
            .map_err(|e| anyhow!("get_image_drm_format_modifier_properties: {e}"))?;
    }
    let layout0 = unsafe {
        vkd.device.get_image_subresource_layout(
            image,
            vk::ImageSubresource {
                aspect_mask: vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
                mip_level: 0,
                array_layer: 0,
            },
        )
    };

    let _ = format;
    Ok(OwnedImage {
        image,
        memory,
        width,
        height,
        modifier: props.drm_format_modifier,
        plane0_stride: layout0.row_pitch,
        plane0_offset: layout0.offset,
        plane0_size: req.size,
    })
}

pub fn export_dmabuf(vkd: &VkDevice, img: &OwnedImage) -> Result<OwnedFd> {
    let raw = unsafe {
        vkd.ext_mem_fd.get_memory_fd(
            &vk::MemoryGetFdInfoKHR::default()
                .memory(img.memory)
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT),
        )
    }
    .map_err(|e| anyhow!("vkGetMemoryFdKHR: {e}"))?;
    Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(raw) })
}

pub fn import_dmabuf(
    vkd: &VkDevice,
    fd: OwnedFd,
    width: u32,
    height: u32,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
    modifier: u64,
    plane0_stride: u64,
    plane0_offset: u64,
) -> Result<OwnedImage> {
    let raw_fd: RawFd = fd.into_raw_fd();
    let fd_owner = unsafe { OwnedFd::from_raw_fd(raw_fd) };

    let plane_layout = vk::SubresourceLayout {
        offset: plane0_offset,
        size: 0,
        row_pitch: plane0_stride,
        array_pitch: 0,
        depth_pitch: 0,
    };
    let layouts = [plane_layout];
    let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(modifier)
        .plane_layouts(&layouts);
    let mut ext_info = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

    let image = unsafe {
        vkd.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                .usage(usage)
                .push_next(&mut explicit)
                .push_next(&mut ext_info),
            None,
        )
    }
    .map_err(|e| anyhow!("vkCreateImage(import explicit modifier): {e}"))?;

    let req = unsafe { vkd.device.get_image_memory_requirements(image) };

    // Must intersect image_bits with fd_bits — bind verifies aliasing
    // post-alloc and rejects mismatches even when alloc itself accepted.
    let mut fd_props = vk::MemoryFdPropertiesKHR::default();
    unsafe {
        vkd.ext_mem_fd
            .get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                fd_owner.as_raw_fd_raw(),
                &mut fd_props,
            )
            .map_err(|e| anyhow!("vkGetMemoryFdPropertiesKHR: {e}"))?;
    }
    let bits = req.memory_type_bits & fd_props.memory_type_bits;
    if bits == 0 {
        unsafe {
            vkd.device.destroy_image(image, None);
        }
        return Err(anyhow!(
            "no memory type intersection: image_bits=0x{:x} fd_bits=0x{:x}",
            req.memory_type_bits,
            fd_props.memory_type_bits,
        ));
    }
    let mtype = super::device::pick_memory_type_for_import(&vkd.mem_props, bits)?;
    let alloc_size = req.size;

    // vk consumes the fd on success; on failure caller still owns it.
    let raw_for_vk = fd_owner.into_raw_fd();
    let mut import = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(raw_for_vk);
    let mut ded = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let memory = unsafe {
        vkd.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(alloc_size)
                .memory_type_index(mtype)
                .push_next(&mut ded)
                .push_next(&mut import),
            None,
        )
    }
    .map_err(|e| {
        unsafe { libc::close(raw_for_vk) };
        anyhow!("vkAllocateMemory(import dma-buf): {e}")
    })?;
    unsafe {
        vkd.device.bind_image_memory(image, memory, 0).map_err(|e| {
            anyhow!("vkBindImageMemory(import): {e}")
        })?;
    }

    let _ = format;
    Ok(OwnedImage {
        image,
        memory,
        width,
        height,
        modifier,
        plane0_stride,
        plane0_offset,
        plane0_size: req.size,
    })
}

impl Drop for OwnedImage {
    fn drop(&mut self) {
        // Image+memory lifetimes are owned by the calling phase, which
        // explicitly free_memory + destroy_image before VkDevice teardown.
        // The raw vk handles intentionally leak if this Drop runs first.
    }
}

trait AsRawFdRaw {
    fn as_raw_fd_raw(&self) -> i32;
}
impl AsRawFdRaw for OwnedFd {
    fn as_raw_fd_raw(&self) -> i32 {
        std::os::fd::AsRawFd::as_raw_fd(self)
    }
}

pub struct HostBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub mapped: *mut u8,
}

unsafe impl Send for HostBuffer {}

pub fn create_host_buffer(vkd: &VkDevice, size: u64) -> Result<HostBuffer> {
    let buffer = unsafe {
        vkd.device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )
    }
    .map_err(|e| anyhow!("create_buffer: {e}"))?;
    let req = unsafe { vkd.device.get_buffer_memory_requirements(buffer) };
    let mtype = super::device::pick_memory_type(&vkd.mem_props, req.memory_type_bits, true)?;
    let memory = unsafe {
        vkd.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(mtype),
            None,
        )
    }
    .map_err(|e| anyhow!("allocate_memory(host): {e}"))?;
    unsafe {
        vkd.device.bind_buffer_memory(buffer, memory, 0)?;
    }
    let mapped = unsafe {
        vkd.device.map_memory(memory, 0, req.size, vk::MemoryMapFlags::empty())
    }
    .map_err(|e| anyhow!("map_memory: {e}"))? as *mut u8;
    Ok(HostBuffer {
        buffer,
        memory,
        mapped,
    })
}

pub fn destroy_host_buffer(vkd: &VkDevice, hb: HostBuffer) {
    unsafe {
        vkd.device.unmap_memory(hb.memory);
        vkd.device.destroy_buffer(hb.buffer, None);
        vkd.device.free_memory(hb.memory, None);
    }
}
