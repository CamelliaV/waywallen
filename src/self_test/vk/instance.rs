use anyhow::{anyhow, Context, Result};
use ash::{vk, Entry, Instance};
use serde::Serialize;

pub struct VkContext {
    _entry: Entry,
    pub instance: Instance,
}

impl Drop for VkContext {
    fn drop(&mut self) {
        unsafe {
            self.instance.destroy_instance(None);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceMeta {
    pub index: usize,
    pub name: String,
    #[serde(serialize_with = "ser_uuid_hex")]
    pub uuid: [u8; 16],
    #[serde(serialize_with = "ser_uuid_hex")]
    pub driver_uuid: [u8; 16],
    #[serde(skip_serializing)]
    pub kind: vk::PhysicalDeviceType,
    pub kind_str: String,
    pub drm_render: (u32, u32),
    #[serde(skip_serializing)]
    pub phys: vk::PhysicalDevice,
    pub has_drm_ext: bool,
}

fn ser_uuid_hex<S: serde::Serializer>(b: &[u8; 16], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&super::super::format_uuid_hex(b))
}

pub fn create_instance() -> Result<VkContext> {
    let entry = unsafe { Entry::load().context("load Vulkan loader")? };
    let app_name = std::ffi::CString::new("waywallen-self-test").unwrap();
    let app_info = vk::ApplicationInfo::default()
        .application_name(&app_name)
        .api_version(vk::make_api_version(0, 1, 2, 0));
    let inst_exts = [vk::KHR_GET_PHYSICAL_DEVICE_PROPERTIES2_NAME.as_ptr()];
    let create_info = vk::InstanceCreateInfo::default()
        .application_info(&app_info)
        .enabled_extension_names(&inst_exts);
    let instance = unsafe {
        entry
            .create_instance(&create_info, None)
            .or_else(|_| {
                // Fall back without the extension on 1.0-only loaders;
                // we'll still get device names via the core 1.0 path.
                entry.create_instance(
                    &vk::InstanceCreateInfo::default().application_info(&app_info),
                    None,
                )
            })
            .map_err(|e| anyhow!("vkCreateInstance: {e}"))?
    };
    Ok(VkContext {
        _entry: entry,
        instance,
    })
}

pub fn enumerate(ctx: &VkContext) -> Result<Vec<DeviceMeta>> {
    let phys_list = unsafe {
        ctx.instance
            .enumerate_physical_devices()
            .map_err(|e| anyhow!("enumerate_physical_devices: {e}"))?
    };

    let mut out = Vec::with_capacity(phys_list.len());
    for (i, phys) in phys_list.into_iter().enumerate() {
        let props = unsafe { ctx.instance.get_physical_device_properties(phys) };
        let name = unsafe {
            std::ffi::CStr::from_ptr(props.device_name.as_ptr())
                .to_string_lossy()
                .into_owned()
        };

        let avail_exts = unsafe {
            ctx.instance
                .enumerate_device_extension_properties(phys)
                .unwrap_or_default()
        };
        let has = |needle: &[u8]| {
            avail_exts.iter().any(|p| {
                let c = unsafe { std::ffi::CStr::from_ptr(p.extension_name.as_ptr()) };
                c.to_bytes() == needle
            })
        };
        let has_drm_ext = has(b"VK_EXT_physical_device_drm");

        let mut id_props = vk::PhysicalDeviceIDProperties::default();
        let mut drm_props = vk::PhysicalDeviceDrmPropertiesEXT::default();
        let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id_props);
        if has_drm_ext {
            props2 = props2.push_next(&mut drm_props);
        }
        unsafe {
            ctx.instance
                .get_physical_device_properties2(phys, &mut props2);
        }

        let drm_render = if has_drm_ext && drm_props.has_render == vk::TRUE {
            (
                u32::try_from(drm_props.render_major).unwrap_or(0),
                u32::try_from(drm_props.render_minor).unwrap_or(0),
            )
        } else {
            (0, 0)
        };

        out.push(DeviceMeta {
            index: i,
            name,
            uuid: id_props.device_uuid,
            driver_uuid: id_props.driver_uuid,
            kind: props.device_type,
            kind_str: format!("{:?}", props.device_type),
            drm_render,
            phys,
            has_drm_ext,
        });
    }
    Ok(out)
}

pub fn find_by_uuid<'a>(devs: &'a [DeviceMeta], uuid: &[u8; 16]) -> Option<&'a DeviceMeta> {
    devs.iter().find(|d| &d.uuid == uuid)
}
