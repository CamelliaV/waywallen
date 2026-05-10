/* waywallen-bridge — internal vtable shared by pool.c and the
 * backend implementations (pool_egl_gbm.c, pool_vulkan.c). */
#ifndef WAYWALLEN_BRIDGE_POOL_INTERNAL_H
#define WAYWALLEN_BRIDGE_POOL_INTERNAL_H

#include <waywallen-bridge/bridge.h>
#include <waywallen-bridge/ipc_v1.h>
#include <waywallen-bridge/pool.h>

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define WW_POOL_MAX_SLOTS  8
#define WW_POOL_MAX_PLANES 4

/* Per-slot layout, multi-plane aware. The IPC `bind_buffers` wire
 * carries `count * planes_per_buffer` flattened entries; pool.c
 * walks every slot and emits the plane sub-array. Modifiers like
 * AMD DCC w/o RETILE require plane_count == 2 (colour + DCC
 * metadata); LINEAR / single-plane modifiers stay at plane_count
 * == 1 with the higher slots holding -1 / 0. */
typedef struct ww_pool_slot_layout {
    uint32_t plane_count;
    /* Owned by backend; pool.c closes them on directive teardown
     * and destroy. Unused slots hold -1. For modifiers backed by
     * a single dma-buf allocation (the common case), the backend
     * dups the same underlying fd into every plane index. */
    int      fds[WW_POOL_MAX_PLANES];
    uint32_t strides[WW_POOL_MAX_PLANES];
    uint32_t plane_offsets[WW_POOL_MAX_PLANES];
    uint64_t sizes[WW_POOL_MAX_PLANES];
    uint64_t modifier;
} ww_pool_slot_layout_t;

/* Producer-side advertised tuple, identical wire-format to the
 * bridge's negotiation_state but owned by the pool. */
typedef struct ww_pool_caps {
    /* Each entry is one (fourcc, modifier, plane_count) tuple the
     * producer can switch to via re-allocation. */
    ww_format_entry_t *entries;
    size_t             count;
    /* Producer's mem-hint set, possibly OR'd with WW_MEM_HINT_LINEAR_ONLY. */
    uint32_t           mem_hints;
    uint32_t           sync_caps;
    uint32_t           color_caps;
    uint32_t           extent_max_w;
    uint32_t           extent_max_h;
    uint8_t            device_uuid[16];
    uint8_t            driver_uuid[16];
    uint32_t           drm_render_major;
    uint32_t           drm_render_minor;
    bool               have_uuid;
} ww_pool_caps_t;

struct ww_pool_backend_ops;

/* The backend may store its own state in `backend_data`. */
struct ww_pool {
    ww_pool_backend_t                 backend;
    const struct ww_pool_backend_ops *ops;
    void                             *backend_data;

    /* drm_fd used for the release-timeline drm_syncobj. Owned by the
     * pool — closed in destroy. Source: EGL/GBM backend dups the
     * plugin's render-node fd; Vulkan backend dups the plugin-supplied
     * fd or opens its own. */
    int                               drm_fd;
    uint32_t                          release_syncobj_handle;
    uint64_t                          release_point;
    uint64_t                          last_release_point[WW_POOL_MAX_SLOTS];

    /* Producer-side advertised caps (filled by backend->advertise_caps).
     * Stable across the pool's lifetime once advertise has run. */
    ww_pool_caps_t                    caps;
    bool                              caps_advertised;

    /* Current directive + slot layout (filled by backend->apply_directive). */
    ww_pool_directive_t               cur;
    bool                              has_directive;
    uint64_t                          bind_generation;
    ww_pool_slot_layout_t             slots[WW_POOL_MAX_SLOTS];
    uint32_t                          n_slots;

    /* Wire-state guards: ready and release_syncobj are sent exactly
     * once per connection. */
    bool                              ready_sent;
    bool                              release_syncobj_sent;

    /* `width` / `height` carried into apply_directive from
     * advertise_caps when directive doesn't carry them; defensive. */
    uint32_t                          probe_width;
    uint32_t                          probe_height;
};

/* Backend vtable. Each entry returns 0 on success or a negated errno
 * on failure unless documented otherwise. */
struct ww_pool_backend_ops {
    /* Initialise backend-specific state from the init descriptor.
     * `init_data` is the corresponding `ww_pool_*_init_t`.
     * Establishes any GPU device resources (GBM device for EGL/GBM,
     * device-level Vulkan helpers for Vulkan) and stores them in
     * `pool->backend_data`. MUST also fill the `drm_render_*` and
     * `device_uuid` / `driver_uuid` fields in `pool->caps` (not the
     * fourcc/modifier lists — those come in advertise_caps). MUST
     * NOT advertise yet. */
    int  (*init)(ww_pool_t *pool, const void *init_data);

    /* Probe the producer's modifier capabilities at (width, height).
     * Fills `pool->caps.entries` (heap-allocated; freed in destroy)
     * and `pool->caps.{sync_caps,color_caps,mem_hints,extent_max_*}`.
     *
     * If the modifier-aware probe yields zero entries, the backend
     * MUST still populate at least one synthesized
     * `(default_fourcc, LINEAR, 1)` entry, AND set
     * `pool->caps.mem_hints |= WW_MEM_HINT_LINEAR_ONLY` so the
     * daemon knows to pick COMPAT_LINEAR straight away. */
    int  (*probe_caps)(ww_pool_t *pool, uint32_t width, uint32_t height);

    /* Allocate one slot for the current directive. The slot's dmabuf
     * fd, stride, plane_offset, and size MUST be written into the
     * provided `out_layout`. The backend stores any handle it needs
     * to expose to the plugin (GL FBO/texture or VkImage/memory)
     * keyed by `slot_index` in its own state.
     *
     * Iter 1 contract: the first slot is the dry-run. If this call
     * fails on slot 0, pool.c will emit `bind_failed` and unwind. */
    int  (*alloc_slot)(ww_pool_t *pool, uint32_t slot_index,
                       ww_pool_slot_layout_t *out_layout);

    /* Free one slot (released dmabuf fd is not closed here — pool.c
     * closes the layout->dmabuf_fd it owns). The backend should free
     * the GL/Vk handles it stored for `slot_index`. Safe to call on
     * an out-of-range or never-allocated slot index (no-op). */
    void (*free_slot)(ww_pool_t *pool, uint32_t slot_index);

    /* Plugin asked for the per-slot handle. Backend writes the
     * appropriate fields (gl_export_fbo / gl_export_texture or
     * vk_image / vk_memory) into `out_slot`. Common fields (index,
     * stride, plane_offset, size) are filled by pool.c — the backend
     * only fills its handle fields. */
    int  (*populate_slot_view)(ww_pool_t *pool, uint32_t slot_index,
                               ww_pool_slot_t *out_slot);

    /* Drain GPU work referencing any allocated slot. Called from
     * ww_bridge_pool_destroy before per-slot teardown so that
     * vkDestroyImage / glDeleteTextures don't run while the device
     * still has the resources in flight (which would either be
     * undefined behaviour or, on producer-process death, leave the
     * exported acquire dma_fence in a force-cancelled state and
     * propagate device-loss to the consumer).
     *
     * Vulkan: vkDeviceWaitIdle. EGL/GBM: glFinish (after eglMakeCurrent
     * with the backend's saved context, since the calling thread may
     * have been the bridge reader thread which has no GL context).
     * MUST be safe to call when no slots have been allocated and on
     * any thread.
     *
     * Optional — older or simpler backends may leave this NULL; pool.c
     * skips the call. */
    void (*wait_idle)(ww_pool_t *pool);

    /* Backend-specific teardown. pool.c destroys the drm_syncobj,
     * closes drm_fd, frees caps.entries and slots[].dmabuf_fd; the
     * backend frees its own state including any GBM device or
     * VkImage resources. Called exactly once. */
    void (*destroy)(ww_pool_t *pool);
};

/* Backend factories. Each picks up the matching init descriptor,
 * fills `pool->ops` and `pool->backend_data`, and returns 0 on
 * success. pool.c calls them via switch on the requested backend. */
int ww_pool_egl_gbm_create(ww_pool_t *pool, const void *init_data);
int ww_pool_vulkan_create(ww_pool_t *pool, const void *init_data);

#endif /* WAYWALLEN_BRIDGE_POOL_INTERNAL_H */
