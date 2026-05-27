use std::os::fd::{IntoRawFd, OwnedFd};

use anyhow::{anyhow, Result};
use ash::vk;

use super::device::VkDevice;

pub struct TimelineSemaphore {
    pub sem: vk::Semaphore,
}

pub fn create_timeline_exportable(vkd: &VkDevice) -> Result<TimelineSemaphore> {
    let mut export = vk::ExportSemaphoreCreateInfo::default()
        .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
    let mut tl = vk::SemaphoreTypeCreateInfo::default()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0);
    let sem = unsafe {
        vkd.device.create_semaphore(
            &vk::SemaphoreCreateInfo::default()
                .push_next(&mut tl)
                .push_next(&mut export),
            None,
        )
    }
    .map_err(|e| anyhow!("vkCreateSemaphore(timeline export): {e}"))?;
    Ok(TimelineSemaphore { sem })
}

pub fn export_opaque_fd(vkd: &VkDevice, sem: &TimelineSemaphore) -> Result<OwnedFd> {
    let raw = unsafe {
        vkd.ext_sem_fd.get_semaphore_fd(
            &vk::SemaphoreGetFdInfoKHR::default()
                .semaphore(sem.sem)
                .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD),
        )
    }
    .map_err(|e| anyhow!("vkGetSemaphoreFdKHR(OPAQUE_FD timeline): {e}"))?;
    Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(raw) })
}

pub fn import_timeline_opaque_fd(vkd: &VkDevice, fd: OwnedFd) -> Result<TimelineSemaphore> {
    let mut tl = vk::SemaphoreTypeCreateInfo::default()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0);
    let sem = unsafe {
        vkd.device
            .create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut tl), None)
    }
    .map_err(|e| anyhow!("vkCreateSemaphore(timeline import-target): {e}"))?;

    // vk consumes the fd on success; on failure caller closes it.
    let raw = fd.into_raw_fd();
    unsafe {
        vkd.ext_sem_fd
            .import_semaphore_fd(
                &vk::ImportSemaphoreFdInfoKHR::default()
                    .semaphore(sem)
                    .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
                    .fd(raw),
            )
            .map_err(|e| {
                libc::close(raw);
                vkd.device.destroy_semaphore(sem, None);
                anyhow!("vkImportSemaphoreFdKHR(OPAQUE_FD timeline): {e}")
            })?;
    }
    Ok(TimelineSemaphore { sem })
}

/// Binary VkSemaphore exportable as SYNC_FD. Signal it via a queue submit
/// then call [`export_signaled_sync_fd`] to take the dma_fence sync_file
/// fd out — that fd is what the production display endpoint hands to
/// each consumer as the per-frame acquire fence.
pub fn create_binary_sync_fd_exportable(vkd: &VkDevice) -> Result<vk::Semaphore> {
    let mut export = vk::ExportSemaphoreCreateInfo::default()
        .handle_types(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
    let sem = unsafe {
        vkd.device.create_semaphore(
            &vk::SemaphoreCreateInfo::default().push_next(&mut export),
            None,
        )
    }
    .map_err(|e| anyhow!("vkCreateSemaphore(binary SYNC_FD export): {e}"))?;
    Ok(sem)
}

/// Export the SYNC_FD payload of a binary semaphore that has been (or
/// will soon be) signaled via a queue submit. After export, the
/// semaphore enters the unsignaled state per the SYNC_FD external
/// semaphore handle semantics, ready for re-use on the next submit.
pub fn export_signaled_sync_fd(vkd: &VkDevice, sem: vk::Semaphore) -> Result<OwnedFd> {
    let raw = unsafe {
        vkd.ext_sem_fd.get_semaphore_fd(
            &vk::SemaphoreGetFdInfoKHR::default()
                .semaphore(sem)
                .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD),
        )
    }
    .map_err(|e| anyhow!("vkGetSemaphoreFdKHR(SYNC_FD): {e}"))?;
    Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(raw) })
}

/// Import a SYNC_FD into a binary semaphore as a TEMPORARY association
/// (per spec: SYNC_FD imports are always temporary, so the next submit
/// that waits on `sem` consumes the fence and resets the binding).
pub fn import_sync_fd_temporary(vkd: &VkDevice, sem: vk::Semaphore, fd: OwnedFd) -> Result<()> {
    let raw = fd.into_raw_fd();
    unsafe {
        vkd.ext_sem_fd
            .import_semaphore_fd(
                &vk::ImportSemaphoreFdInfoKHR::default()
                    .semaphore(sem)
                    .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
                    .flags(vk::SemaphoreImportFlags::TEMPORARY)
                    .fd(raw),
            )
            .map_err(|e| {
                libc::close(raw);
                anyhow!("vkImportSemaphoreFdKHR(SYNC_FD): {e}")
            })?;
    }
    Ok(())
}

pub fn create_binary_importable(vkd: &VkDevice) -> Result<vk::Semaphore> {
    let mut export = vk::ExportSemaphoreCreateInfo::default()
        .handle_types(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
    let sem = unsafe {
        vkd.device.create_semaphore(
            &vk::SemaphoreCreateInfo::default().push_next(&mut export),
            None,
        )
    }
    .map_err(|e| anyhow!("vkCreateSemaphore(SYNC_FD import-able): {e}"))?;
    Ok(sem)
}

pub fn wait_timeline(
    vkd: &VkDevice,
    sem: &TimelineSemaphore,
    value: u64,
    timeout_ns: u64,
) -> Result<()> {
    let sems = [sem.sem];
    let values = [value];
    let info = vk::SemaphoreWaitInfo::default()
        .semaphores(&sems)
        .values(&values);
    unsafe {
        vkd.timeline
            .wait_semaphores(&info, timeout_ns)
            .map_err(|e| anyhow!("vkWaitSemaphores: {e}"))?;
    }
    Ok(())
}
