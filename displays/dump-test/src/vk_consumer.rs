//! DMA-BUF readback for the `dump_display` test consumer.
//!
//! ## Strategy
//!
//! For `DRM_FORMAT_MOD_LINEAR` modifiers we mmap the DMA-BUF fd
//! directly — the producer guarantees CPU-readable layout, and the
//! kernel handles cache flushing via the DMA-BUF sync ioctl. This is
//! the cross-vendor universal path with no Vulkan import needed.
//!
//! For non-LINEAR (tiled / vendor-specific) modifiers we go through a
//! real Vulkan import: build a `VkImage` with
//! `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`, import the DMA-BUF fd
//! as `VkDeviceMemory`, then issue `vkCmdCopyImageToBuffer` into a
//! `HOST_VISIBLE` staging buffer that we map and dump as
//! tightly-packed RGBA8. This matches what a real consumer (the
//! `waywallen-display` C lib's `backend_vulkan.c`) does on receive.
//!
//! [`VkContext`] caches the instance/device + extension fn pointers
//! so each call to [`VkContext::import_and_dump`] only pays the cost
//! of one `VkImage` + one staging buffer + one command buffer.

use std::ffi::CStr;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::path::Path;
use std::ptr;

use anyhow::{anyhow, bail, Context, Result};
use ash::vk;

use crate::{ModCapJson, PeerCapsJson};

const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// All four 32-bit RGB(A) packings the producer might emit. Each maps
/// to a single Vulkan format because the byte order is the same once
/// the producer's RGBA8 source is uploaded — alpha presence is purely
/// a downstream sampling concern.
fn drm_fourcc_to_vk(fourcc: u32) -> Option<vk::Format> {
    match fourcc {
        // ABGR8888 / XBGR8888 — R-G-B-A byte order in memory.
        waywallen::negotiate::DRM_FORMAT_ABGR8888 | waywallen::negotiate::DRM_FORMAT_XBGR8888 => {
            Some(vk::Format::R8G8B8A8_UNORM)
        }
        // ARGB8888 / XRGB8888 — B-G-R-A byte order in memory.
        waywallen::negotiate::DRM_FORMAT_ARGB8888 | waywallen::negotiate::DRM_FORMAT_XRGB8888 => {
            Some(vk::Format::B8G8R8A8_UNORM)
        }
        _ => None,
    }
}

const FOURCCS: &[u32] = &[
    waywallen::negotiate::DRM_FORMAT_ABGR8888,
    waywallen::negotiate::DRM_FORMAT_XBGR8888,
    waywallen::negotiate::DRM_FORMAT_ARGB8888,
    waywallen::negotiate::DRM_FORMAT_XRGB8888,
];

// ---------------------------------------------------------------------------
// VkContext
// ---------------------------------------------------------------------------

/// Lazily-initialised Vulkan state for the consumer side. Holds the
/// instance, the picked physical device, a logical device with the
/// DMA-BUF / external-memory / external-semaphore extensions
/// enabled, and the extension fn pointers required for import.
///
/// `query_caps()` is cheap (no image creation); `import_and_dump()`
/// does one full upload+readback per call.
pub struct VkContext {
    // Order matters for Drop: we must drop the device-level handles
    // before the device, and the device before the instance.
    // `drm_modifier` is kept around to anchor its loaded fn pointer
    // table even though we don't currently call into it (the
    // alternative would be re-loading on every import).
    #[allow(dead_code)]
    drm_modifier: ash::ext::image_drm_format_modifier::Device,
    external_memory_fd: ash::khr::external_memory_fd::Device,
    external_semaphore_fd: ash::khr::external_semaphore_fd::Device,
    command_pool: vk::CommandPool,
    queue: vk::Queue,
    queue_family: u32,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    pub device_uuid: [u8; 16],
    pub driver_uuid: [u8; 16],
    pub drm_render_major: u32,
    pub drm_render_minor: u32,
    device: ash::Device,
    physical: vk::PhysicalDevice,
    instance: ash::Instance,
    _entry: ash::Entry,
}

impl VkContext {
    pub fn new() -> Result<Self> {
        // Loaded at runtime so the test still builds on hosts without
        // the Vulkan loader installed (it'll just fail at construction
        // time, which the caller handles by skipping).
        let entry = unsafe { ash::Entry::load() }.context("load Vulkan loader (libvulkan.so.1)")?;

        // ---- Instance ----
        let app_name = CStr::from_bytes_with_nul(b"waywallen-dump-display\0").unwrap();
        let app_info = vk::ApplicationInfo::default()
            .application_name(app_name)
            .api_version(vk::API_VERSION_1_1);

        // We target Vulkan 1.1 to match the producer; the
        // get_physical_device_properties2 / external-memory-caps /
        // external-semaphore-caps extensions are core in 1.1, so an
        // empty extension list is enough. (Listing them explicitly
        // would be a no-op under 1.1.)
        let inst_ci = vk::InstanceCreateInfo::default().application_info(&app_info);
        let instance = unsafe { entry.create_instance(&inst_ci, None) }
            .map_err(|e| anyhow!("vkCreateInstance: {e:?}"))?;

        // ---- Physical device ----
        let pds = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| anyhow!("enumerate_physical_devices: {e:?}"))?;
        if pds.is_empty() {
            unsafe { instance.destroy_instance(None) };
            bail!("no Vulkan physical devices found");
        }

        let req_dev_exts: &[&CStr] = &[
            ash::khr::external_memory_fd::NAME,
            ash::ext::external_memory_dma_buf::NAME,
            ash::ext::image_drm_format_modifier::NAME,
            ash::khr::external_semaphore_fd::NAME,
        ];
        // Optional: VK_EXT_physical_device_drm to report a real DRM
        // render-node id. When absent we leave (0,0); the daemon's
        // negotiate then assumes cross-GPU and forces LINEAR, which
        // is fine — we still satisfy at least that pair.
        let drm_ext_name = ash::ext::physical_device_drm::NAME;

        let physical = pds
            .into_iter()
            .find(|pd| {
                let exts = match unsafe { instance.enumerate_device_extension_properties(*pd) } {
                    Ok(v) => v,
                    Err(_) => return false,
                };
                req_dev_exts.iter().all(|need| {
                    exts.iter().any(|p| {
                        let n = unsafe { CStr::from_ptr(p.extension_name.as_ptr()) };
                        n == *need
                    })
                })
            })
            .ok_or_else(|| {
                anyhow!(
                    "no physical device exposes the required extension set: {:?}",
                    req_dev_exts
                        .iter()
                        .map(|c| c.to_string_lossy())
                        .collect::<Vec<_>>()
                )
            })?;
        let have_drm_ext = match unsafe { instance.enumerate_device_extension_properties(physical) }
        {
            Ok(exts) => exts.iter().any(|p| {
                let n = unsafe { CStr::from_ptr(p.extension_name.as_ptr()) };
                n == drm_ext_name
            }),
            Err(_) => false,
        };

        // ---- Queue family ----
        let qf_props = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let queue_family = qf_props
            .iter()
            .position(|q| {
                q.queue_flags.intersects(
                    vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER,
                )
            })
            .ok_or_else(|| anyhow!("no transfer-capable queue family"))?
            as u32;

        // ---- Device ----
        let prio = [1.0f32];
        let qci = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&prio);

        let mut dev_ext_names: Vec<*const i8> = req_dev_exts.iter().map(|c| c.as_ptr()).collect();
        if have_drm_ext {
            dev_ext_names.push(drm_ext_name.as_ptr());
        }
        let queue_cis = [qci];
        let dci = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_cis)
            .enabled_extension_names(&dev_ext_names);
        let device = unsafe { instance.create_device(physical, &dci, None) }
            .map_err(|e| anyhow!("vkCreateDevice: {e:?}"))?;

        let queue = unsafe { device.get_device_queue(queue_family, 0) };

        // ---- Extension fn loaders ----
        let drm_modifier = ash::ext::image_drm_format_modifier::Device::new(&instance, &device);
        let external_memory_fd = ash::khr::external_memory_fd::Device::new(&instance, &device);
        let external_semaphore_fd =
            ash::khr::external_semaphore_fd::Device::new(&instance, &device);

        // ---- Identity (device_uuid, driver_uuid, render-node) ----
        let mut id_props = vk::PhysicalDeviceVulkan11Properties::default();
        let mut drm_props = vk::PhysicalDeviceDrmPropertiesEXT::default();
        let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id_props);
        if have_drm_ext {
            props2 = props2.push_next(&mut drm_props);
        }
        unsafe { instance.get_physical_device_properties2(physical, &mut props2) };
        let device_uuid = id_props.device_uuid;
        let driver_uuid = id_props.driver_uuid;
        let (drm_render_major, drm_render_minor) = if have_drm_ext && drm_props.has_render != 0 {
            (drm_props.render_major as u32, drm_props.render_minor as u32)
        } else {
            (0, 0)
        };

        let mem_props = unsafe { instance.get_physical_device_memory_properties(physical) };

        // ---- Command pool ----
        let pool_ci = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = unsafe { device.create_command_pool(&pool_ci, None) }
            .map_err(|e| anyhow!("create_command_pool: {e:?}"))?;

        log::info!(
            "vk_consumer: device_uuid={} render={}:{}",
            uuid_str(&device_uuid),
            drm_render_major,
            drm_render_minor
        );

        Ok(Self {
            _entry: entry,
            instance,
            physical,
            device,
            queue,
            queue_family,
            command_pool,
            mem_props,
            device_uuid,
            driver_uuid,
            drm_render_major,
            drm_render_minor,
            drm_modifier,
            external_memory_fd,
            external_semaphore_fd,
        })
    }

    /// Enumerate every `(fourcc, modifier)` pair the picked physical
    /// device can import as `TRANSFER_SRC` (which is all we need to
    /// blit it back to a staging buffer). Returns the JSON shape the
    /// test orchestrator parses.
    pub fn query_caps(&self) -> Result<PeerCapsJson> {
        use std::collections::BTreeMap;
        let mut by_fourcc: BTreeMap<String, Vec<ModCapJson>> = BTreeMap::new();

        for &fourcc in FOURCCS {
            let Some(vk_format) = drm_fourcc_to_vk(fourcc) else {
                continue;
            };
            let mods = self.modifiers_for(vk_format);
            if mods.is_empty() {
                continue;
            }
            // Always advertise LINEAR as a fallback even if the
            // driver-reported list happened to omit it (it usually
            // doesn't, but the LINEAR fast-path doesn't actually need
            // driver participation — we mmap the dmabuf directly).
            let mut mods = mods;
            if !mods.iter().any(|m| m.modifier == DRM_FORMAT_MOD_LINEAR) {
                mods.push(ModCapJson {
                    modifier: DRM_FORMAT_MOD_LINEAR,
                    usage: waywallen::negotiate::USAGE_SAMPLED
                        | waywallen::negotiate::USAGE_TRANSFER_DST,
                    plane_count: 1,
                });
            }
            by_fourcc.insert(format!("0x{fourcc:08x}"), mods);
        }

        if by_fourcc.is_empty() {
            bail!("vk_consumer: no fourcc/modifier pairs supported by this device");
        }

        let mut device_uuid = [0u8; 16];
        device_uuid.copy_from_slice(&self.device_uuid);
        let mut driver_uuid = [0u8; 16];
        driver_uuid.copy_from_slice(&self.driver_uuid);

        Ok(PeerCapsJson {
            by_fourcc,
            device_uuid,
            driver_uuid,
            drm_render_major: self.drm_render_major,
            drm_render_minor: self.drm_render_minor,
            sync: waywallen::negotiate::SYNC_SYNCOBJ_BINARY
                | waywallen::negotiate::SYNC_SYNCOBJ_TIMELINE,
            color: waywallen::negotiate::DEFAULT_COLOR,
            mem_hint: waywallen::negotiate::MEM_HINT_HOST_VISIBLE,
            extent_max_w: 8192,
            extent_max_h: 8192,
        })
    }

    /// Two-pass `vkGetPhysicalDeviceFormatProperties2 +
    /// VkDrmFormatModifierPropertiesListEXT` query for one VkFormat.
    /// Returns only modifiers whose declared `drmFormatModifierTilingFeatures`
    /// include `TRANSFER_SRC` — those are the ones we can `vkCmdCopyImageToBuffer`
    /// out of after import.
    fn modifiers_for(&self, vk_format: vk::Format) -> Vec<ModCapJson> {
        // 1st pass: count.
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut fp2 = vk::FormatProperties2::default().push_next(&mut list);
        unsafe {
            self.instance
                .get_physical_device_format_properties2(self.physical, vk_format, &mut fp2)
        };
        let n = list.drm_format_modifier_count as usize;
        if n == 0 {
            return Vec::new();
        }

        // 2nd pass: fill.
        let mut buf = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
            .drm_format_modifier_properties(&mut buf);
        let mut fp2 = vk::FormatProperties2::default().push_next(&mut list);
        unsafe {
            self.instance
                .get_physical_device_format_properties2(self.physical, vk_format, &mut fp2)
        };

        // The producer's import path needs at least TRANSFER_SRC so we
        // can `vkCmdCopyImageToBuffer` out. Filter accordingly.
        buf.into_iter()
            .filter(|p| {
                p.drm_format_modifier_tiling_features
                    .contains(vk::FormatFeatureFlags::TRANSFER_SRC)
            })
            .map(|p| ModCapJson {
                modifier: p.drm_format_modifier,
                usage: waywallen::negotiate::USAGE_SAMPLED
                    | waywallen::negotiate::USAGE_TRANSFER_DST,
                plane_count: p.drm_format_modifier_plane_count,
            })
            .collect()
    }

    /// Block on `acquire_fd`, copy the imported DMA-BUF into a
    /// host-visible staging buffer, dump tightly-packed RGBA8 to
    /// `<dump-dir>/consumer-{seq:06}-…bin` plus a `.json` sidecar.
    ///
    /// LINEAR takes a fast path (mmap + DMA_BUF_IOCTL_SYNC) because
    /// it doesn't need driver participation — both sides agree the
    /// CPU can read the bytes directly. Tiled modifiers go through
    /// the full Vulkan import + `vkCmdCopyImageToBuffer` path.
    #[allow(clippy::too_many_arguments)]
    pub fn import_and_dump(
        &self,
        dmabuf_fd: &OwnedFd,
        acquire_fd: i32,
        fourcc: u32,
        modifier: u64,
        width: u32,
        height: u32,
        stride: u32,
        plane_offset: u32,
        size: u64,
        seq: u64,
        dump_dir: &Path,
    ) -> Result<()> {
        if modifier == DRM_FORMAT_MOD_LINEAR {
            return import_and_dump_linear(
                dmabuf_fd,
                acquire_fd,
                fourcc,
                modifier,
                width,
                height,
                stride,
                plane_offset,
                size,
                seq,
                dump_dir,
            );
        }
        self.import_and_dump_tiled(
            dmabuf_fd,
            acquire_fd,
            fourcc,
            modifier,
            width,
            height,
            stride,
            plane_offset,
            size,
            seq,
            dump_dir,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn import_and_dump_tiled(
        &self,
        dmabuf_fd: &OwnedFd,
        acquire_fd: i32,
        fourcc: u32,
        modifier: u64,
        width: u32,
        height: u32,
        stride: u32,
        plane_offset: u32,
        _size: u64,
        seq: u64,
        dump_dir: &Path,
    ) -> Result<()> {
        let vk_format = drm_fourcc_to_vk(fourcc)
            .ok_or_else(|| anyhow!("no VkFormat mapping for fourcc 0x{fourcc:08x}"))?;

        // ---- VkImage with the explicit modifier + plane layout ----
        let plane_layouts = [vk::SubresourceLayout {
            offset: plane_offset as u64,
            row_pitch: stride as u64,
            size: 0, // driver computes
            ..Default::default()
        }];
        let mut mod_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(modifier)
            .plane_layouts(&plane_layouts);
        let mut ext_mem_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let queue_indices = [self.queue_family];
        let image_ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .queue_family_indices(&queue_indices)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut ext_mem_info)
            .push_next(&mut mod_info);
        let image = unsafe { self.device.create_image(&image_ci, None) }.map_err(|e| {
            anyhow!("vkCreateImage(modifier=0x{modifier:016x}, fourcc=0x{fourcc:08x}): {e:?}")
        })?;

        let cleanup_image = ImageGuard { ctx: self, image };

        // ---- Memory: import the dmabuf fd ----
        let mem_req = unsafe { self.device.get_image_memory_requirements(image) };

        let mut fd_props = vk::MemoryFdPropertiesKHR::default();
        // Dup the fd for the property query (the call doesn't take
        // ownership but on some drivers the fd's offset is consulted).
        let probe_fd = dup_fd(dmabuf_fd.as_raw_fd())?;
        let probe_raw = probe_fd.into_raw_fd();
        let res = unsafe {
            self.external_memory_fd.get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                probe_raw,
                &mut fd_props,
            )
        };
        unsafe { libc::close(probe_raw) };
        res.map_err(|e| anyhow!("vkGetMemoryFdPropertiesKHR: {e:?}"))?;

        let mask = mem_req.memory_type_bits & fd_props.memory_type_bits;
        if mask == 0 {
            bail!(
                "no memory type accepts both image and fd \
                 (image.bits=0x{:08x} fd.bits=0x{:08x}) — likely the producer \
                 allocated DEVICE_LOCAL VRAM but the consumer can't import it",
                mem_req.memory_type_bits,
                fd_props.memory_type_bits
            );
        }

        // Iterate candidate memtypes; take the first that allocates.
        let mut chosen_memory: Option<vk::DeviceMemory> = None;
        let mut last_err = String::new();
        for i in 0..32u32 {
            if (mask >> i) & 1 == 0 {
                continue;
            }
            let try_fd = dup_fd(dmabuf_fd.as_raw_fd())?;
            let raw_try = try_fd.into_raw_fd();

            // Both `ImportMemoryFdInfoKHR` and `MemoryDedicatedAllocateInfo`
            // implement `ExtendsMemoryAllocateInfo` in ash, so they
            // chain onto the alloc_info, not onto each other.
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let mut import_fd = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .fd(raw_try);
            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_req.size)
                .memory_type_index(i)
                .push_next(&mut import_fd)
                .push_next(&mut dedicated);

            match unsafe { self.device.allocate_memory(&alloc_info, None) } {
                Ok(mem) => {
                    chosen_memory = Some(mem);
                    log::debug!(
                        "vk_consumer: imported dmabuf into memTypeIndex={i} (modifier=0x{modifier:016x})"
                    );
                    break;
                }
                Err(e) => {
                    // The driver did NOT take ownership of the fd on
                    // failure — we must close it ourselves.
                    unsafe { libc::close(raw_try) };
                    last_err = format!("memTypeIndex={i}: {e:?}");
                }
            }
        }
        let memory = chosen_memory
            .ok_or_else(|| anyhow!("vkAllocateMemory exhausted candidates: {last_err}"))?;
        let cleanup_memory = MemoryGuard { ctx: self, memory };

        unsafe {
            self.device
                .bind_image_memory(image, memory, 0)
                .map_err(|e| anyhow!("bind_image_memory: {e:?}"))?
        };

        // ---- Staging buffer (HOST_VISIBLE) ----
        let staging_size = (width as u64) * (height as u64) * 4;
        let buf_ci = vk::BufferCreateInfo::default()
            .size(staging_size)
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let staging_buf = unsafe { self.device.create_buffer(&buf_ci, None) }
            .map_err(|e| anyhow!("create_buffer staging: {e:?}"))?;
        let cleanup_buf = BufferGuard {
            ctx: self,
            buffer: staging_buf,
        };
        let buf_req = unsafe { self.device.get_buffer_memory_requirements(staging_buf) };
        let mem_idx = self
            .find_memory_type(
                buf_req.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .ok_or_else(|| anyhow!("no HOST_VISIBLE+HOST_COHERENT memory type"))?;
        let staging_alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(buf_req.size)
            .memory_type_index(mem_idx);
        let staging_mem = unsafe { self.device.allocate_memory(&staging_alloc, None) }
            .map_err(|e| anyhow!("allocate_memory staging: {e:?}"))?;
        let cleanup_staging_mem = MemoryGuard {
            ctx: self,
            memory: staging_mem,
        };
        unsafe {
            self.device
                .bind_buffer_memory(staging_buf, staging_mem, 0)
                .map_err(|e| anyhow!("bind_buffer_memory staging: {e:?}"))?
        };

        // ---- Command buffer ----
        let cb_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd_bufs = unsafe { self.device.allocate_command_buffers(&cb_alloc) }
            .map_err(|e| anyhow!("allocate_command_buffers: {e:?}"))?;
        let cb = cmd_bufs[0];
        let cleanup_cb = CmdBufGuard {
            ctx: self,
            cmd_bufs: cmd_bufs.clone(),
        };

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe { self.device.begin_command_buffer(cb, &begin) }
            .map_err(|e| anyhow!("begin_command_buffer: {e:?}"))?;

        // Layout transition: UNDEFINED → TRANSFER_SRC_OPTIMAL.
        // For VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT the spec only
        // permits initialLayout = UNDEFINED or PREINITIALIZED at
        // create time; we pick UNDEFINED. On every common consumer
        // driver (radv/anv/amdgpu), this transition preserves the
        // producer-written contents — our byte-compare pass is the
        // canary if a vendor changes that.
        let barrier = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
            .dst_queue_family_index(self.queue_family)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        unsafe {
            self.device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
        }

        // CopyImageToBuffer: target buffer is tightly packed
        // (bufferRowLength=0 ⇒ width, bufferImageHeight=0 ⇒ height).
        let region = vk::BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
            image_extent: vk::Extent3D {
                width,
                height,
                depth: 1,
            },
        };
        unsafe {
            self.device.cmd_copy_image_to_buffer(
                cb,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                staging_buf,
                &[region],
            );
        }

        // Pipeline barrier so the host read after the submit sees the
        // staging buffer's write.
        let buf_barrier = vk::BufferMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ)
            .src_queue_family_index(self.queue_family)
            .dst_queue_family_index(self.queue_family)
            .buffer(staging_buf)
            .offset(0)
            .size(staging_size);
        unsafe {
            self.device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[],
                &[buf_barrier],
                &[],
            );
        }

        unsafe { self.device.end_command_buffer(cb) }
            .map_err(|e| anyhow!("end_command_buffer: {e:?}"))?;

        // ---- Import the acquire sync_fd as a binary VkSemaphore ----
        // SYNC_FD imports MUST use TEMPORARY semantics (the spec
        // forbids permanent import) — the payload is consumed on the
        // first wait.
        let sem_ci = vk::SemaphoreCreateInfo::default();
        let acquire_sem = unsafe { self.device.create_semaphore(&sem_ci, None) }
            .map_err(|e| anyhow!("create_semaphore: {e:?}"))?;
        let cleanup_sem = SemaphoreGuard {
            ctx: self,
            sem: acquire_sem,
        };

        // Dup so the caller's OwnedFd can drop unconditionally —
        // VkImportSemaphoreFdInfoKHR with SYNC_FD + TEMPORARY consumes
        // the fd on success, so passing the caller's fd directly
        // would race with their drop on the failure path. Dup keeps
        // ownership clear.
        let dup_acquire = dup_fd(acquire_fd)?.into_raw_fd();
        let import = vk::ImportSemaphoreFdInfoKHR::default()
            .semaphore(acquire_sem)
            .flags(vk::SemaphoreImportFlags::TEMPORARY)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
            .fd(dup_acquire);
        if let Err(e) = unsafe { self.external_semaphore_fd.import_semaphore_fd(&import) } {
            unsafe { libc::close(dup_acquire) };
            return Err(anyhow!(
                "vkImportSemaphoreFdKHR(sync_fd={acquire_fd}): {e:?}"
            ));
        }
        // Vulkan now owns the dup; caller's original is still owned
        // by its OwnedFd and will close at scope exit.

        // ---- Submit ----
        let fence_ci = vk::FenceCreateInfo::default();
        let fence = unsafe { self.device.create_fence(&fence_ci, None) }
            .map_err(|e| anyhow!("create_fence: {e:?}"))?;
        let cleanup_fence = FenceGuard { ctx: self, fence };

        let wait_sems = [acquire_sem];
        let wait_stages = [vk::PipelineStageFlags::TRANSFER];
        let cbs = [cb];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&wait_sems)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&cbs);
        unsafe { self.device.queue_submit(self.queue, &[submit], fence) }
            .map_err(|e| anyhow!("queue_submit: {e:?}"))?;

        unsafe { self.device.wait_for_fences(&[fence], true, 5_000_000_000) }
            .map_err(|e| anyhow!("wait_for_fences: {e:?}"))?;

        // ---- Map staging, dump bytes ----
        let map = unsafe {
            self.device
                .map_memory(staging_mem, 0, staging_size, vk::MemoryMapFlags::empty())
                .map_err(|e| anyhow!("map_memory staging: {e:?}"))?
        } as *const u8;

        let dump_path = dump_dir.join(format!(
            "consumer-{seq:06}-0x{fourcc:08x}-0x{modifier:016x}.bin"
        ));
        let mut file =
            File::create(&dump_path).with_context(|| format!("create {}", dump_path.display()))?;
        let bytes = unsafe { std::slice::from_raw_parts(map, staging_size as usize) };
        file.write_all(bytes).context("dump write")?;
        unsafe { self.device.unmap_memory(staging_mem) };

        let sidecar = dump_path.with_extension("json");
        let row_bytes = (width as u64) * 4;
        let meta = serde_json::json!({
            "kind": "consumer",
            "seq": seq,
            "fourcc": format!("0x{fourcc:08x}"),
            "modifier": format!("0x{modifier:016x}"),
            "width": width,
            "height": height,
            "stride": stride,
            "plane_offset": plane_offset,
            "size": staging_size,
            "row_bytes": row_bytes,
            "row_count": height,
            "dump_layout": "tightly_packed_rgba8",
            "import_path": "vulkan_copy_image_to_buffer",
        });
        std::fs::write(&sidecar, serde_json::to_vec_pretty(&meta)?)?;

        // RAII guards drop in reverse declaration order, which is the
        // order we want (fence → sem → cb → staging mem → buf → img mem → image).
        drop(cleanup_fence);
        drop(cleanup_sem);
        drop(cleanup_cb);
        drop(cleanup_staging_mem);
        drop(cleanup_buf);
        drop(cleanup_memory);
        drop(cleanup_image);
        Ok(())
    }

    fn find_memory_type(&self, type_bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
        for i in 0..self.mem_props.memory_type_count {
            if (type_bits >> i) & 1 == 0 {
                continue;
            }
            if self.mem_props.memory_types[i as usize]
                .property_flags
                .contains(flags)
            {
                return Some(i);
            }
        }
        None
    }
}

impl Drop for VkContext {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

// ---------------------------------------------------------------------------
// LINEAR fast path (mmap)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn import_and_dump_linear(
    dmabuf_fd: &OwnedFd,
    acquire_fd: i32,
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
    stride: u32,
    plane_offset: u32,
    size: u64,
    seq: u64,
    dump_dir: &Path,
) -> Result<()> {
    if (stride as u64) < (width as u64) * 4 {
        bail!("stride {stride} < width*4 ({})", width * 4);
    }
    let needed = stride as u64 * height as u64 + plane_offset as u64;
    if size < needed {
        bail!("buffer size {size} < stride*height + offset ({needed})");
    }

    poll_in(acquire_fd, 5_000).context("wait acquire sync_fd")?;

    let map_len = size as libc::size_t;
    let map = unsafe {
        libc::mmap(
            ptr::null_mut(),
            map_len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            dmabuf_fd.as_raw_fd(),
            0,
        )
    };
    if map == libc::MAP_FAILED {
        let err = std::io::Error::last_os_error();
        bail!("mmap dmabuf: {err}");
    }
    let raw = map as *const u8;

    sync_dma_buf(
        dmabuf_fd.as_raw_fd(),
        DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ,
    )
    .context("DMA_BUF_IOCTL_SYNC start")?;

    let dump_path = dump_dir.join(format!(
        "consumer-{seq:06}-0x{fourcc:08x}-0x{modifier:016x}.bin"
    ));
    let mut file =
        File::create(&dump_path).with_context(|| format!("create {}", dump_path.display()))?;
    let plane_off = plane_offset as usize;
    let row_bytes = (width as usize) * 4;
    let stride_us = stride as usize;
    for y in 0..height as usize {
        let row_start = plane_off + y * stride_us;
        let slice = unsafe { std::slice::from_raw_parts(raw.add(row_start), row_bytes) };
        file.write_all(slice).context("dump write")?;
    }

    let _ = sync_dma_buf(dmabuf_fd.as_raw_fd(), DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
    unsafe {
        libc::munmap(map, map_len);
    }

    let sidecar = dump_path.with_extension("json");
    let meta = serde_json::json!({
        "kind": "consumer",
        "seq": seq,
        "fourcc": format!("0x{fourcc:08x}"),
        "modifier": format!("0x{modifier:016x}"),
        "width": width,
        "height": height,
        "stride": stride,
        "plane_offset": plane_offset,
        "size": size,
        "row_bytes": row_bytes,
        "row_count": height,
        "dump_layout": "tightly_packed_rgba8",
        "import_path": "mmap_linear",
    });
    std::fs::write(&sidecar, serde_json::to_vec_pretty(&meta)?)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn dup_fd(fd: i32) -> Result<OwnedFd> {
    let r = unsafe { libc::dup(fd) };
    if r < 0 {
        return Err(anyhow!("dup({fd}): {}", std::io::Error::last_os_error()));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(r) })
}

use std::os::fd::FromRawFd;

fn uuid_str(u: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u[0],
        u[1],
        u[2],
        u[3],
        u[4],
        u[5],
        u[6],
        u[7],
        u[8],
        u[9],
        u[10],
        u[11],
        u[12],
        u[13],
        u[14],
        u[15],
    )
}

// ---------------------------------------------------------------------------
// RAII guards (paired with each Vulkan handle so a mid-pipeline error
// doesn't leak GPU memory)
// ---------------------------------------------------------------------------

struct ImageGuard<'a> {
    ctx: &'a VkContext,
    image: vk::Image,
}
impl<'a> Drop for ImageGuard<'a> {
    fn drop(&mut self) {
        unsafe { self.ctx.device.destroy_image(self.image, None) };
    }
}

struct MemoryGuard<'a> {
    ctx: &'a VkContext,
    memory: vk::DeviceMemory,
}
impl<'a> Drop for MemoryGuard<'a> {
    fn drop(&mut self) {
        unsafe { self.ctx.device.free_memory(self.memory, None) };
    }
}

struct BufferGuard<'a> {
    ctx: &'a VkContext,
    buffer: vk::Buffer,
}
impl<'a> Drop for BufferGuard<'a> {
    fn drop(&mut self) {
        unsafe { self.ctx.device.destroy_buffer(self.buffer, None) };
    }
}

struct SemaphoreGuard<'a> {
    ctx: &'a VkContext,
    sem: vk::Semaphore,
}
impl<'a> Drop for SemaphoreGuard<'a> {
    fn drop(&mut self) {
        unsafe { self.ctx.device.destroy_semaphore(self.sem, None) };
    }
}

struct FenceGuard<'a> {
    ctx: &'a VkContext,
    fence: vk::Fence,
}
impl<'a> Drop for FenceGuard<'a> {
    fn drop(&mut self) {
        unsafe { self.ctx.device.destroy_fence(self.fence, None) };
    }
}

struct CmdBufGuard<'a> {
    ctx: &'a VkContext,
    cmd_bufs: Vec<vk::CommandBuffer>,
}
impl<'a> Drop for CmdBufGuard<'a> {
    fn drop(&mut self) {
        unsafe {
            self.ctx
                .device
                .free_command_buffers(self.ctx.command_pool, &self.cmd_bufs)
        };
    }
}

// ---------------------------------------------------------------------------
// dma-buf sync ioctl (LINEAR path only)
// ---------------------------------------------------------------------------

const DMA_BUF_SYNC_READ: u64 = 1 << 0;
#[allow(dead_code)]
const DMA_BUF_SYNC_WRITE: u64 = 1 << 1;
#[allow(dead_code)]
const DMA_BUF_SYNC_RW: u64 = DMA_BUF_SYNC_READ | DMA_BUF_SYNC_WRITE;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 1 << 2;

#[repr(C)]
struct DmaBufSync {
    flags: u64,
}

nix::ioctl_write_ptr!(dma_buf_sync_ioctl, b'b', 0, DmaBufSync);

fn sync_dma_buf(fd: i32, flags: u64) -> Result<()> {
    let s = DmaBufSync { flags };
    let r = unsafe { dma_buf_sync_ioctl(fd, &s as *const DmaBufSync) };
    if let Err(err) = r {
        return Err(anyhow!("DMA_BUF_IOCTL_SYNC flags=0x{flags:x}: {err}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// poll() wait on sync_file fd (LINEAR path only — Vulkan path imports
// the sync_fd into a VkSemaphore instead)
// ---------------------------------------------------------------------------

fn poll_in(fd: i32, timeout_ms: i32) -> Result<()> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let r = unsafe { libc::poll(&mut pfd as *mut _, 1, timeout_ms) };
    if r < 0 {
        return Err(anyhow!("poll: {}", std::io::Error::last_os_error()));
    }
    if r == 0 {
        bail!("poll timeout after {timeout_ms}ms");
    }
    if pfd.revents & libc::POLLIN == 0 {
        bail!("poll revents=0x{:x} (expected POLLIN)", pfd.revents);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Backwards-compatible free fns (call sites that don't yet hold a
// VkContext go through these — they just construct a one-shot
// context). `query_caps` is invoked only once from main.rs in
// --print-caps mode, so the construction overhead is fine.
// ---------------------------------------------------------------------------

pub fn query_caps() -> Result<PeerCapsJson> {
    VkContext::new()?.query_caps()
}
