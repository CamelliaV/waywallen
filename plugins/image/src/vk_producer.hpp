#pragma once

#include <cstdint>
#include <memory>
#include <string>

#include <vulkan/vulkan.h>

namespace ww_image {

// Immutable view of the DMA-BUF backing a VkImage slot. Owned by VkProducer;
// the `dmabuf_fd` stays open for the producer's lifetime and is dup'd into
// SCM_RIGHTS messages by the IPC layer.
struct VkSlotLayout {
    int      dmabuf_fd { -1 };
    uint64_t drm_modifier { 0 };
    uint32_t drm_fourcc { 0 };
    uint32_t width { 0 };
    uint32_t height { 0 };
    uint32_t plane_offset { 0 };
    uint32_t stride { 0 }; // bytes per row (rowPitch)
    uint32_t size { 0 };   // total memory size for this plane/image
};

// Bit set on the daemon-controlled `ConfigureBuffers.flags` request and
// echoed back on `BindBuffers.flags`. Bit 0 = host_visible: back the
// dmabuf with HOST_VISIBLE && !DEVICE_LOCAL memory (GTT) so a foreign
// GPU's amdgpu/i915 driver can PRIME-import it. Cleared = use plain
// DEVICE_LOCAL (VRAM) for the zero-copy same-GPU path.
constexpr uint32_t WW_BUF_HOST_VISIBLE = 1u << 0;

// Encapsulates a minimal Vulkan 1.1 instance+device set up for DMA-BUF export
// and a single VkImage slot. M3 will extend this with staging upload and
// signal-semaphore sync_fd export.
class VkProducer {
public:
    ~VkProducer();
    VkProducer(const VkProducer&)            = delete;
    VkProducer& operator=(const VkProducer&) = delete;

    // Create a producer with one `width` x `height` slot. `flags` selects
    // the dmabuf placement (see WW_BUF_HOST_VISIBLE). On failure returns
    // nullptr and populates `*err` with a human-readable reason.
    static std::unique_ptr<VkProducer>
    create(uint32_t width, uint32_t height, uint32_t flags, std::string* err);

    const VkSlotLayout& layout() const { return layout_; }

    // Current placement flags the slot was allocated with (the value
    // last passed to `create`/`rebuild`).
    uint32_t flags() const { return flags_; }

    // DRM render-node major/minor of the picked physical device. `(0, 0)`
    // when `VK_EXT_physical_device_drm` isn't advertised. Reported to
    // the daemon in `Ready` so it can match the renderer's GPU against
    // each connected display's GPU.
    uint32_t drm_render_major() const { return drm_render_major_; }
    uint32_t drm_render_minor() const { return drm_render_minor_; }

    // Tear down the current image+memory+exported fd and rebuild with a
    // new placement. Caller is responsible for re-uploading data
    // afterwards (the staging buffer is preserved). On success the new
    // `layout()` is the post-rebuild layout. Returns false and writes
    // `*err` on failure (instance/device are left intact, but the
    // image slot is gone — caller should treat that as terminal).
    bool rebuild(uint32_t flags, std::string* err);

    // Copy `data` (tightly packed RGBA8, `width*height*4` bytes) into the
    // slot's DMA-BUF via a staging buffer, transition the image layout to
    // GENERAL and release queue-family ownership to FOREIGN so the external
    // consumer can read it, then export a one-shot sync_file fd for the
    // signal. Caller owns the returned fd (sent via SCM_RIGHTS and then
    // closed). Returns -1 on failure and populates `*err`.
    int upload_and_submit(const uint8_t* data, size_t size, std::string* err);

private:
    VkProducer() = default;

    VkInstance       instance_ { VK_NULL_HANDLE };
    VkPhysicalDevice phys_ { VK_NULL_HANDLE };
    VkDevice         device_ { VK_NULL_HANDLE };
    uint32_t         queue_family_ { 0 };
    VkQueue          queue_ { VK_NULL_HANDLE };
    VkImage          image_ { VK_NULL_HANDLE };
    VkDeviceMemory   memory_ { VK_NULL_HANDLE };

    VkCommandPool    cmd_pool_ { VK_NULL_HANDLE };
    VkCommandBuffer  cmd_ { VK_NULL_HANDLE };
    VkSemaphore      signal_sem_ { VK_NULL_HANDLE };

    VkBuffer         staging_buf_ { VK_NULL_HANDLE };
    VkDeviceMemory   staging_mem_ { VK_NULL_HANDLE };
    void*            staging_map_ { nullptr };
    VkDeviceSize     staging_size_ { 0 };

    VkSlotLayout layout_ {};

    uint32_t flags_ { 0 };
    uint32_t drm_render_major_ { 0 };
    uint32_t drm_render_minor_ { 0 };

    PFN_vkGetMemoryFdKHR                         vkGetMemoryFdKHR_ { nullptr };
    PFN_vkGetSemaphoreFdKHR                      vkGetSemaphoreFdKHR_ { nullptr };
    PFN_vkGetImageDrmFormatModifierPropertiesEXT vkGetImageDrmFormatModifierPropertiesEXT_ { nullptr };
    PFN_vkGetPhysicalDeviceProperties2           vkGetPhysicalDeviceProperties2_ { nullptr };

    // Internal: allocate image+memory+export fd+layout for the current
    // queue-family on the existing device, using `flags_` to pick the
    // memory type. Caller (create/rebuild) must ensure the previous
    // image/memory/fd has been torn down before invoking.
    bool build_image(std::string* err);
};

} // namespace ww_image
