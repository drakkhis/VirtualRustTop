#![allow(dead_code)]

use crate::raycast::CylinderScreen;
use anyhow::{anyhow, Result};
use ash::vk::{self, Handle};
use glam::{Mat4, Quat, Vec3};
use openxr as xr;
use std::ffi::CStr;
use std::ptr;
use vdesk_protocol::{InputEvent, MonitorDescriptor};

pub const REQUIRED_EXTENSIONS: &[&str] = &[
    "XR_KHR_vulkan_enable2",
    "XR_KHR_composition_layer_cylinder",
    "XR_FB_passthrough",
    "XR_FB_display_refresh_rate",
    "XR_FB_composition_layer_settings",
    "XR_KHR_android_surface_swapchain",
    "XR_EXT_hand_tracking",
];

pub const DEFAULT_CYLINDER_RADIUS_M: f32 = 1.8;
pub const DEFAULT_EYE_HEIGHT_M: f32 = 0.0;
pub const PRIMARY_MONITOR_ARC_RAD: f32 = 1.1;
pub const MONITOR_GAP_RAD: f32 = 0.05;

#[derive(Debug, Clone)]
pub struct CylinderLayerParams {
    pub stream_id: u32,
    pub radius: f32,
    pub central_angle: f32,
    pub aspect_ratio: f32,
    pub pose: xr::Posef,
}

pub fn build_default_cylinder_screen(stream_id: u32, width: u32, height: u32) -> CylinderScreen {
    let aspect = (width as f32) / (height.max(1) as f32);
    CylinderScreen::new(
        stream_id,
        Mat4::from_translation(Vec3::new(0.0, DEFAULT_EYE_HEIGHT_M, 0.0)),
        DEFAULT_CYLINDER_RADIUS_M,
        PRIMARY_MONITOR_ARC_RAD,
        aspect,
    )
}

/// Arrange one or more Wayland monitors around a shared cylindrical arc at radius 1.8m.
/// The largest monitor sits centered straight ahead (`-Z`), with left/right neighbors
/// placed side-by-side according to their `logical_x` order.
pub fn layout_cylinder_screens(monitors: &[MonitorDescriptor]) -> Vec<CylinderScreen> {
    if monitors.is_empty() {
        return Vec::new();
    }

    let mut ordered: Vec<&MonitorDescriptor> = monitors.iter().collect();
    ordered.sort_by_key(|m| (m.logical_x, m.stream_id));

    let primary_idx = ordered
        .iter()
        .enumerate()
        .max_by_key(|(_, m)| u64::from(m.width) * u64::from(m.height))
        .map(|(i, _)| i)
        .unwrap_or(0);

    let primary_width = ordered[primary_idx].width.max(1) as f32;
    let rad_per_px = PRIMARY_MONITOR_ARC_RAD / primary_width;

    let angles: Vec<f32> = ordered
        .iter()
        .map(|m| (m.width.max(1) as f32 * rad_per_px).clamp(0.35, 1.4))
        .collect();

    let mut yaws = vec![0.0f32; ordered.len()];

    // Monitors to the left of primary (smaller logical_x) rotate +yaw around +Y.
    let mut cursor_yaw = angles[primary_idx] * 0.5;
    for i in (0..primary_idx).rev() {
        cursor_yaw += MONITOR_GAP_RAD + angles[i] * 0.5;
        yaws[i] = cursor_yaw;
        cursor_yaw += angles[i] * 0.5;
    }

    // Monitors to the right of primary (larger logical_x) rotate -yaw around +Y.
    let mut cursor_yaw = -angles[primary_idx] * 0.5;
    for i in (primary_idx + 1)..ordered.len() {
        cursor_yaw -= MONITOR_GAP_RAD + angles[i] * 0.5;
        yaws[i] = cursor_yaw;
        cursor_yaw -= angles[i] * 0.5;
    }

    ordered
        .iter()
        .enumerate()
        .map(|(i, mon)| {
            let aspect = (mon.width as f32) / (mon.height.max(1) as f32);
            let transform = Mat4::from_translation(Vec3::new(0.0, DEFAULT_EYE_HEIGHT_M, 0.0))
                * Mat4::from_rotation_y(yaws[i]);
            CylinderScreen::new(
                mon.stream_id,
                transform,
                DEFAULT_CYLINDER_RADIUS_M,
                angles[i],
                aspect,
            )
        })
        .collect()
}

/// Raycast a controller aim ray against all spatial monitor cylinders and return the closest
/// `PointerMotionAbsolute` event if hit.
pub fn raycast_monitors(
    screens: &[CylinderScreen],
    ray_origin_world: Vec3,
    ray_dir_world: Vec3,
) -> Option<InputEvent> {
    screens
        .iter()
        .filter_map(|screen| {
            screen
                .intersect_ray(ray_origin_world, ray_dir_world)
                .map(|(t, uv)| (t, screen.stream_id, uv))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, stream_id, uv)| InputEvent::PointerMotionAbsolute {
            stream_id,
            u: uv.x.clamp(0.0, 1.0),
            v: uv.y.clamp(0.0, 1.0),
        })
}

pub fn build_cylinder_layer_params(screen: &CylinderScreen) -> CylinderLayerParams {
    let (_, rot, pos) = screen.world_from_cylinder.to_scale_rotation_translation();
    let rot = rot.normalize();
    CylinderLayerParams {
        stream_id: screen.stream_id,
        radius: screen.radius,
        central_angle: screen.central_angle,
        aspect_ratio: screen.aspect_ratio,
        pose: xr::Posef {
            orientation: xr::Quaternionf {
                x: rot.x,
                y: rot.y,
                z: rot.z,
                w: rot.w,
            },
            position: xr::Vector3f {
                x: pos.x,
                y: pos.y,
                z: pos.z,
            },
        },
    }
}

pub fn build_cylinder_layer<'a, G: xr::Graphics>(
    params: &CylinderLayerParams,
    space: &'a xr::Space,
    swapchain: &'a xr::Swapchain<G>,
    width: i32,
    height: i32,
) -> xr::CompositionLayerCylinderKHR<'a, G> {
    let sub_image = xr::SwapchainSubImage::new()
        .swapchain(swapchain)
        .image_array_index(0)
        .image_rect(xr::Rect2Di {
            offset: xr::Offset2Di { x: 0, y: 0 },
            extent: xr::Extent2Di { width, height },
        });

    xr::CompositionLayerCylinderKHR::new()
        .space(space)
        .eye_visibility(xr::EyeVisibility::BOTH)
        .sub_image(sub_image)
        .pose(params.pose)
        .radius(params.radius)
        .central_angle(params.central_angle)
        .aspect_ratio(params.aspect_ratio)
}

pub fn build_quad_layer<'a, G: xr::Graphics>(
    world_from_quad: Mat4,
    width_m: f32,
    height_m: f32,
    space: &'a xr::Space,
    swapchain: &'a xr::Swapchain<G>,
    tex_w: i32,
    tex_h: i32,
) -> xr::CompositionLayerQuad<'a, G> {
    let (_, rot, pos) = world_from_quad.to_scale_rotation_translation();
    let rot = rot.normalize();
    let sub_image = xr::SwapchainSubImage::new()
        .swapchain(swapchain)
        .image_array_index(0)
        .image_rect(xr::Rect2Di {
            offset: xr::Offset2Di { x: 0, y: 0 },
            extent: xr::Extent2Di {
                width: tex_w,
                height: tex_h,
            },
        });
    xr::CompositionLayerQuad::new()
        .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
        .space(space)
        .eye_visibility(xr::EyeVisibility::BOTH)
        .sub_image(sub_image)
        .pose(xr::Posef {
            orientation: xr::Quaternionf {
                x: rot.x,
                y: rot.y,
                z: rot.z,
                w: rot.w,
            },
            position: xr::Vector3f {
                x: pos.x,
                y: pos.y,
                z: pos.z,
            },
        })
        .size(xr::Extent2Df {
            width: width_m,
            height: height_m,
        })
}

pub fn apply_layer_settings_sharpening(
    layer_raw: &mut xr::sys::CompositionLayerCylinderKHR,
    settings: &xr::sys::CompositionLayerSettingsFB,
) {
    layer_raw.next = settings as *const _ as *const std::ffi::c_void;
}

pub fn select_preferred_extensions(available: &xr::ExtensionSet) -> xr::ExtensionSet {
    let mut set = xr::ExtensionSet::default();
    set.khr_vulkan_enable2 = available.khr_vulkan_enable2;
    set.khr_composition_layer_cylinder = available.khr_composition_layer_cylinder;
    set.fb_passthrough = available.fb_passthrough;
    set.fb_display_refresh_rate = available.fb_display_refresh_rate;
    set.fb_composition_layer_settings = available.fb_composition_layer_settings;
    #[cfg(target_os = "android")]
    {
        set.khr_android_create_instance = available.khr_android_create_instance;
        set.khr_android_surface_swapchain = available.khr_android_surface_swapchain;
    }
    set.ext_hand_tracking = available.ext_hand_tracking;
    set
}

pub fn init_openxr_instance() -> Result<(xr::Entry, xr::Instance)> {
    let entry = unsafe { xr::Entry::load()? };
    #[cfg(target_os = "android")]
    entry.initialize_android_loader()?;
    let available = entry.enumerate_extensions()?;
    let enabled = select_preferred_extensions(&available);
    let app_info = xr::ApplicationInfo {
        application_name: "vdesk-rs",
        application_version: 1,
        engine_name: "vdesk-xr",
        engine_version: 1,
        api_version: xr::CURRENT_API_VERSION,
    };
    let instance = entry.create_instance(&app_info, &enabled, &[])?;
    Ok((entry, instance))
}

pub struct MonitorSwapchain {
    pub screen: CylinderScreen,
    pub width: u32,
    pub height: u32,
    pub swapchain: xr::Swapchain<xr::Vulkan>,
    pub decoder: crate::decoder::HardwareDecoder,
    pub pinned: bool,
}

pub struct VulkanCleanup {
    pub vk_instance: ash::Instance,
    pub vk_device: ash::Device,
    pub vk_cmd_pool: vk::CommandPool,
    pub vk_fence: vk::Fence,
    pub kb_staging_buf: vk::Buffer,
    pub kb_staging_mem: vk::DeviceMemory,
}

impl Drop for VulkanCleanup {
    fn drop(&mut self) {
        unsafe {
            let _ = self.vk_device.device_wait_idle();
            self.vk_device.unmap_memory(self.kb_staging_mem);
            self.vk_device.destroy_buffer(self.kb_staging_buf, None);
            self.vk_device.free_memory(self.kb_staging_mem, None);
            self.vk_device.destroy_fence(self.vk_fence, None);
            self.vk_device.destroy_command_pool(self.vk_cmd_pool, None);
            self.vk_device.destroy_device(None);
            self.vk_instance.destroy_instance(None);
        }
    }
}

pub struct QuestXrSession {
    pub _entry: xr::Entry,
    pub instance: xr::Instance,
    pub system: xr::SystemId,
    pub _vk_entry: ash::Entry,
    pub vk_device: ash::Device,
    pub vk_queue: vk::Queue,
    pub vk_cmd_buf: vk::CommandBuffer,
    pub vk_fence: vk::Fence,
    pub session: xr::Session<xr::Vulkan>,
    pub frame_waiter: xr::FrameWaiter,
    pub frame_stream: xr::FrameStream<xr::Vulkan>,
    pub local_space: xr::Space,
    pub view_space: xr::Space,
    pub keepalive_swapchain: xr::Swapchain<xr::Vulkan>,
    pub keepalive_images: Vec<vk::Image>,
    pub keyboard_swapchain: xr::Swapchain<xr::Vulkan>,
    pub keyboard_images: Vec<vk::Image>,
    pub kb_staging_buf: vk::Buffer,
    pub kb_staging_ptr: *mut u8,
    pub passthrough: Option<(xr::Passthrough, xr::PassthroughLayer)>,
    pub action_set: xr::ActionSet,
    pub aim_action: xr::Action<xr::Posef>,
    pub aim_space: xr::Space,
    pub left_aim_action: xr::Action<xr::Posef>,
    pub left_aim_space: xr::Space,
    pub haptic_action: xr::Action<xr::Haptic>,
    pub left_hand_path: xr::Path,
    pub right_hand_path: xr::Path,
    pub trigger_action: xr::Action<f32>,
    pub right_grip_action: xr::Action<f32>,
    pub right_thumbstick_action: xr::Action<xr::Vector2f>,
    pub right_thumbstick_click_action: xr::Action<bool>,
    pub right_a_action: xr::Action<bool>,
    pub right_b_action: xr::Action<bool>,
    pub left_x_action: xr::Action<bool>,
    pub left_y_action: xr::Action<bool>,
    pub left_trigger_action: xr::Action<f32>,
    // Must be last so OpenXR session and swapchains drop before VkDevice/VkInstance.
    _vk_cleanup: VulkanCleanup,
}

impl QuestXrSession {
    pub fn new() -> Result<Self> {
        let (entry, instance) = init_openxr_instance()?;
        let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;

        let vk_req = instance.graphics_requirements::<xr::Vulkan>(system)?;
        let vk_entry = unsafe { ash::Entry::load()? };
        let vk_app_info = vk::ApplicationInfo::default()
            .application_name(c"vdesk-rs")
            .application_version(1)
            .engine_name(c"vdesk-xr")
            .engine_version(1)
            .api_version(vk::make_api_version(
                0,
                vk_req.min_api_version_supported.major() as u32,
                vk_req.min_api_version_supported.minor() as u32,
                0,
            ));

        let vk_inst_ci = vk::InstanceCreateInfo::default().application_info(&vk_app_info);

        let get_inst_proc: openxr::sys::platform::VkGetInstanceProcAddr =
            unsafe { std::mem::transmute(vk_entry.static_fn().get_instance_proc_addr) };

        let raw_vk_instance = unsafe {
            instance
                .create_vulkan_instance(system, get_inst_proc, &vk_inst_ci as *const _ as *const _)?
                .map_err(|e| anyhow!("vkCreateInstance via OpenXR failed: {:?}", e))?
        };
        let vk_instance_handle = vk::Instance::from_raw(raw_vk_instance as u64);
        let vk_instance = unsafe { ash::Instance::load(vk_entry.static_fn(), vk_instance_handle) };

        let raw_phys_dev =
            unsafe { instance.vulkan_graphics_device(system, raw_vk_instance)? };
        let vk_phys_dev = vk::PhysicalDevice::from_raw(raw_phys_dev as u64);

        let queue_props =
            unsafe { vk_instance.get_physical_device_queue_family_properties(vk_phys_dev) };
        let queue_family_index = queue_props
            .iter()
            .enumerate()
            .find(|(_, p)| p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .map(|(idx, _)| idx as u32)
            .ok_or_else(|| anyhow!("No Vulkan graphics queue family found"))?;

        let priorities = [1.0f32];
        let queue_ci = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&priorities)];
        let dev_ci = vk::DeviceCreateInfo::default().queue_create_infos(&queue_ci);

        let raw_vk_device = unsafe {
            instance
                .create_vulkan_device(
                    system,
                    get_inst_proc,
                    raw_phys_dev,
                    &dev_ci as *const _ as *const _,
                )?
                .map_err(|e| anyhow!("vkCreateDevice via OpenXR failed: {:?}", e))?
        };
        let vk_device_handle = vk::Device::from_raw(raw_vk_device as u64);
        let vk_device = unsafe { ash::Device::load(vk_instance.fp_v1_0(), vk_device_handle) };
        let vk_queue = unsafe { vk_device.get_device_queue(queue_family_index, 0) };
        let vk_cmd_pool = unsafe {
            vk_device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(queue_family_index)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?
        };
        let vk_cmd_buf = unsafe {
            vk_device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(vk_cmd_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?
        }[0];
        let vk_fence = unsafe { vk_device.create_fence(&vk::FenceCreateInfo::default(), None)? };

        let (session, frame_waiter, frame_stream) = unsafe {
            instance.create_session::<xr::Vulkan>(
                system,
                &xr::vulkan::SessionCreateInfo {
                    instance: raw_vk_instance,
                    physical_device: raw_phys_dev,
                    device: raw_vk_device,
                    queue_family_index,
                    queue_index: 0,
                },
            )?
        };

        let formats = session.enumerate_swapchain_formats()?;
        let srgb = vk::Format::R8G8B8A8_SRGB.as_raw() as u32;
        let unorm = vk::Format::R8G8B8A8_UNORM.as_raw() as u32;
        let chosen_format = if formats.contains(&srgb) {
            srgb
        } else if formats.contains(&unorm) {
            unorm
        } else {
            *formats
                .first()
                .ok_or_else(|| anyhow!("No Vulkan swapchain formats available"))?
        };
        let keepalive_swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                | xr::SwapchainUsageFlags::SAMPLED,
            format: chosen_format,
            sample_count: 1,
            width: 64,
            height: 64,
            face_count: 1,
            array_size: 1,
            mip_count: 1,
        })?;
        let keepalive_images: Vec<vk::Image> = keepalive_swapchain
            .enumerate_images()?
            .into_iter()
            .map(vk::Image::from_raw)
            .collect();

        let keyboard_swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: xr::SwapchainUsageFlags::SAMPLED
                | xr::SwapchainUsageFlags::TRANSFER_DST,
            format: chosen_format,
            sample_count: 1,
            width: crate::keyboard::KB_TEX_WIDTH,
            height: crate::keyboard::KB_TEX_HEIGHT,
            face_count: 1,
            array_size: 1,
            mip_count: 1,
        })?;
        let keyboard_images: Vec<vk::Image> = keyboard_swapchain
            .enumerate_images()?
            .into_iter()
            .map(vk::Image::from_raw)
            .collect();

        let kb_buf_size =
            (crate::keyboard::KB_TEX_WIDTH * crate::keyboard::KB_TEX_HEIGHT * 4) as vk::DeviceSize;
        let kb_staging_buf = unsafe {
            vk_device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(kb_buf_size)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?
        };
        let kb_mem_reqs = unsafe { vk_device.get_buffer_memory_requirements(kb_staging_buf) };
        let mem_props = unsafe { vk_instance.get_physical_device_memory_properties(vk_phys_dev) };
        let wanted_flags =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let kb_mem_type = mem_props
            .memory_types
            .iter()
            .enumerate()
            .take(mem_props.memory_type_count as usize)
            .find(|(idx, mt)| {
                (kb_mem_reqs.memory_type_bits & (1u32 << idx)) != 0
                    && mt.property_flags.contains(wanted_flags)
            })
            .map(|(idx, _)| idx as u32)
            .ok_or_else(|| anyhow!("No HOST_VISIBLE|HOST_COHERENT memory type for keyboard"))?;
        let kb_staging_mem = unsafe {
            vk_device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(kb_mem_reqs.size)
                    .memory_type_index(kb_mem_type),
                None,
            )?
        };
        unsafe { vk_device.bind_buffer_memory(kb_staging_buf, kb_staging_mem, 0)? };
        let kb_staging_ptr = unsafe {
            vk_device.map_memory(kb_staging_mem, 0, kb_buf_size, vk::MemoryMapFlags::empty())?
                as *mut u8
        };

        // Request 90 Hz (or highest supported refresh rate) on Quest 3
        if instance.exts().fb_display_refresh_rate.is_some() {
            if let Ok(rates) = session.enumerate_display_refresh_rates() {
                let target = if rates.contains(&90.0) {
                    90.0
                } else {
                    rates.into_iter().fold(72.0f32, f32::max)
                };
                let _ = session.request_display_refresh_rate(target);
            }
        }

        let local_space =
            session.create_reference_space(xr::ReferenceSpaceType::LOCAL, xr::Posef::IDENTITY)?;
        let view_space =
            session.create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)?;

        let passthrough = if instance.exts().fb_passthrough.is_some() {
            let pt = session.create_passthrough(xr::PassthroughFlagsFB::IS_RUNNING_AT_CREATION)?;
            let pt_layer = session.create_passthrough_layer(
                &pt,
                xr::PassthroughFlagsFB::IS_RUNNING_AT_CREATION,
                xr::PassthroughLayerPurposeFB::RECONSTRUCTION,
            )?;
            Some((pt, pt_layer))
        } else {
            None
        };

        let action_set = instance.create_action_set("vdesk_input", "vdesk-rs Spatial Input", 0)?;
        let right_hand = instance.string_to_path("/user/hand/right")?;
        let left_hand = instance.string_to_path("/user/hand/left")?;
        let aim_action =
            action_set.create_action::<xr::Posef>("aim_pose", "Aim Pose", &[right_hand])?;
        let left_aim_action = action_set.create_action::<xr::Posef>(
            "left_aim_pose",
            "Left Aim Pose",
            &[left_hand],
        )?;
        let haptic_action = action_set.create_action::<xr::Haptic>(
            "haptic_pulse",
            "Controller Haptic",
            &[left_hand, right_hand],
        )?;
        let trigger_action =
            action_set.create_action::<f32>("trigger_click", "Trigger Click", &[right_hand])?;
        let right_grip_action =
            action_set.create_action::<f32>("right_grip", "Right Grip", &[right_hand])?;
        let right_thumbstick_action = action_set.create_action::<xr::Vector2f>(
            "right_thumbstick",
            "Right Thumbstick",
            &[right_hand],
        )?;
        let right_thumbstick_click_action = action_set.create_action::<bool>(
            "right_thumbstick_click",
            "Right Thumbstick Click",
            &[right_hand],
        )?;
        let right_a_action =
            action_set.create_action::<bool>("right_a", "Left Click (A)", &[right_hand])?;
        let right_b_action =
            action_set.create_action::<bool>("right_b", "Right Click (B)", &[right_hand])?;
        let left_x_action =
            action_set.create_action::<bool>("left_x", "Toggle Keyboard (X)", &[left_hand])?;
        let left_y_action =
            action_set.create_action::<bool>("left_y", "Recenter (Y)", &[left_hand])?;
        let left_trigger_action =
            action_set.create_action::<f32>("left_trigger", "Right Click (LT)", &[left_hand])?;

        let oculus_profile =
            instance.string_to_path("/interaction_profiles/oculus/touch_controller")?;
        let _ = instance.suggest_interaction_profile_bindings(
            oculus_profile,
            &[
                xr::Binding::new(
                    &aim_action,
                    instance.string_to_path("/user/hand/right/input/aim/pose")?,
                ),
                xr::Binding::new(
                    &left_aim_action,
                    instance.string_to_path("/user/hand/left/input/aim/pose")?,
                ),
                xr::Binding::new(
                    &haptic_action,
                    instance.string_to_path("/user/hand/left/output/haptic")?,
                ),
                xr::Binding::new(
                    &haptic_action,
                    instance.string_to_path("/user/hand/right/output/haptic")?,
                ),
                xr::Binding::new(
                    &trigger_action,
                    instance.string_to_path("/user/hand/right/input/trigger/value")?,
                ),
                xr::Binding::new(
                    &right_grip_action,
                    instance.string_to_path("/user/hand/right/input/squeeze/value")?,
                ),
                xr::Binding::new(
                    &right_thumbstick_action,
                    instance.string_to_path("/user/hand/right/input/thumbstick")?,
                ),
                xr::Binding::new(
                    &right_thumbstick_click_action,
                    instance.string_to_path("/user/hand/right/input/thumbstick/click")?,
                ),
                xr::Binding::new(
                    &right_a_action,
                    instance.string_to_path("/user/hand/right/input/a/click")?,
                ),
                xr::Binding::new(
                    &right_b_action,
                    instance.string_to_path("/user/hand/right/input/b/click")?,
                ),
                xr::Binding::new(
                    &left_x_action,
                    instance.string_to_path("/user/hand/left/input/x/click")?,
                ),
                xr::Binding::new(
                    &left_y_action,
                    instance.string_to_path("/user/hand/left/input/y/click")?,
                ),
                xr::Binding::new(
                    &left_trigger_action,
                    instance.string_to_path("/user/hand/left/input/trigger/value")?,
                ),
            ],
        );
        session.attach_action_sets(&[&action_set])?;

        let aim_space = aim_action.create_space(session.clone(), right_hand, xr::Posef::IDENTITY)?;
        let left_aim_space =
            left_aim_action.create_space(session.clone(), left_hand, xr::Posef::IDENTITY)?;

        Ok(Self {
            _entry: entry,
            instance,
            system,
            _vk_entry: vk_entry,
            vk_device: vk_device.clone(),
            vk_queue,
            vk_cmd_buf,
            vk_fence,
            session,
            frame_waiter,
            frame_stream,
            local_space,
            view_space,
            keepalive_swapchain,
            keepalive_images,
            keyboard_swapchain,
            keyboard_images,
            kb_staging_buf,
            kb_staging_ptr,
            passthrough,
            action_set,
            aim_action,
            aim_space,
            left_aim_action,
            left_aim_space,
            haptic_action,
            left_hand_path: left_hand,
            right_hand_path: right_hand,
            trigger_action,
            right_grip_action,
            right_thumbstick_action,
            right_thumbstick_click_action,
            right_a_action,
            right_b_action,
            left_x_action,
            left_y_action,
            left_trigger_action,
            _vk_cleanup: VulkanCleanup {
                vk_instance,
                vk_device,
                vk_cmd_pool,
                vk_fence,
                kb_staging_buf,
                kb_staging_mem,
            },
        })
    }

    #[cfg(target_os = "android")]
    pub unsafe fn create_android_monitor_swapchain(
        &self,
        env: *mut jni::sys::JNIEnv,
        mon: &MonitorDescriptor,
        screen: CylinderScreen,
    ) -> Result<MonitorSwapchain> {
        use anyhow::Context;
        let ext = self
            .instance
            .exts()
            .khr_android_surface_swapchain
            .as_ref()
            .context("XR_KHR_android_surface_swapchain not enabled")?;

        let info = xr::sys::SwapchainCreateInfo {
            ty: xr::sys::SwapchainCreateInfo::TYPE,
            next: ptr::null(),
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: xr::SwapchainUsageFlags::EMPTY,
            format: 0,
            sample_count: 0,
            width: mon.width,
            height: mon.height,
            face_count: 0,
            array_size: 0,
            mip_count: 0,
        };

        let mut raw_swapchain = xr::sys::Swapchain::NULL;
        let mut java_surface: xr::sys::platform::jobject = ptr::null_mut();
        let res = (ext.create_swapchain_android_surface)(
            self.session.as_raw(),
            &info,
            &mut raw_swapchain,
            &mut java_surface,
        );
        if res.into_raw() < 0 {
            anyhow::bail!("xrCreateSwapchainAndroidSurfaceKHR failed: {:?}", res);
        }

        let swapchain = xr::Swapchain::from_raw(self.session.clone(), raw_swapchain);
        let decoder = crate::decoder::HardwareDecoder::from_java_surface(
            env,
            java_surface as jni::sys::jobject,
            mon.codec,
            mon.width,
            mon.height,
        )?;

        Ok(MonitorSwapchain {
            screen,
            width: mon.width,
            height: mon.height,
            swapchain,
            decoder,
            pinned: false,
        })
    }

    pub fn build_passthrough_layer(&self) -> Option<xr::sys::CompositionLayerPassthroughFB> {
        self.passthrough.as_ref().map(|(_, pt_layer)| {
            xr::sys::CompositionLayerPassthroughFB {
                ty: xr::sys::CompositionLayerPassthroughFB::TYPE,
                next: ptr::null(),
                flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
                space: xr::sys::Space::NULL,
                layer_handle: *pt_layer.inner(),
            }
        })
    }

    fn locate_space_pose(
        &self,
        space: &xr::Space,
        predicted_display_time: xr::Time,
    ) -> Result<Option<(Vec3, Quat)>> {
        let loc = space.locate(&self.local_space, predicted_display_time)?;
        if !loc
            .location_flags
            .contains(xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID)
        {
            return Ok(None);
        }
        let origin = Vec3::new(
            loc.pose.position.x,
            loc.pose.position.y,
            loc.pose.position.z,
        );
        let q = Quat::from_xyzw(
            loc.pose.orientation.x,
            loc.pose.orientation.y,
            loc.pose.orientation.z,
            loc.pose.orientation.w,
        )
        .normalize();
        Ok(Some((origin, q)))
    }

    pub fn locate_aim_pose(&self, predicted_display_time: xr::Time) -> Result<Option<(Vec3, Quat)>> {
        self.locate_space_pose(&self.aim_space, predicted_display_time)
    }

    pub fn locate_left_aim_pose(
        &self,
        predicted_display_time: xr::Time,
    ) -> Result<Option<(Vec3, Quat)>> {
        self.locate_space_pose(&self.left_aim_space, predicted_display_time)
    }

    pub fn pulse_haptic(&self, is_left: bool, amplitude: f32, duration_ms: i64) {
        let path = if is_left {
            self.left_hand_path
        } else {
            self.right_hand_path
        };
        let vib = xr::HapticVibration::new()
            .amplitude(amplitude.clamp(0.0, 1.0))
            .duration(xr::Duration::from_nanos(duration_ms.max(1) * 1_000_000))
            .frequency(0.0);
        let _ = self.haptic_action.apply_feedback(&self.session, path, &vib);
    }

    pub fn locate_aim_ray(&self, predicted_display_time: xr::Time) -> Result<Option<(Vec3, Vec3)>> {
        Ok(self
            .locate_aim_pose(predicted_display_time)?
            .map(|(origin, q)| (origin, (q * Vec3::NEG_Z).normalize())))
    }

    pub fn locate_head_pose(&self, predicted_display_time: xr::Time) -> Result<Option<(Vec3, Quat)>> {
        self.locate_space_pose(&self.view_space, predicted_display_time)
    }

    pub fn upload_keyboard_rgba(&mut self, rgba: &[u8]) -> Result<()> {
        let expected =
            (crate::keyboard::KB_TEX_WIDTH * crate::keyboard::KB_TEX_HEIGHT * 4) as usize;
        if rgba.len() != expected {
            anyhow::bail!("Keyboard RGBA size mismatch: {} != {}", rgba.len(), expected);
        }

        let img_idx = self.keyboard_swapchain.acquire_image()?;
        self.keyboard_swapchain.wait_image(xr::Duration::INFINITE)?;
        let image = *self
            .keyboard_images
            .get(img_idx as usize)
            .ok_or_else(|| anyhow!("Keyboard swapchain image index {} out of bounds", img_idx))?;

        unsafe {
            std::ptr::copy_nonoverlapping(rgba.as_ptr(), self.kb_staging_ptr, expected);

            self.vk_device
                .reset_command_buffer(self.vk_cmd_buf, vk::CommandBufferResetFlags::empty())?;
            self.vk_device.begin_command_buffer(
                self.vk_cmd_buf,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;

            let subresource = vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1);

            let to_dst = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(subresource)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            self.vk_device.cmd_pipeline_barrier(
                self.vk_cmd_buf,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_dst],
            );

            let region = vk::BufferImageCopy::default()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(0)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width: crate::keyboard::KB_TEX_WIDTH,
                    height: crate::keyboard::KB_TEX_HEIGHT,
                    depth: 1,
                });
            self.vk_device.cmd_copy_buffer_to_image(
                self.vk_cmd_buf,
                self.kb_staging_buf,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );

            let to_read = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(subresource)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ);
            self.vk_device.cmd_pipeline_barrier(
                self.vk_cmd_buf,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_read],
            );

            self.vk_device.end_command_buffer(self.vk_cmd_buf)?;
            let cmd_bufs = [self.vk_cmd_buf];
            let submit_info = vk::SubmitInfo::default().command_buffers(&cmd_bufs);
            self.vk_device
                .queue_submit(self.vk_queue, &[submit_info], self.vk_fence)?;
            self.vk_device
                .wait_for_fences(&[self.vk_fence], true, 100_000_000)?;
            self.vk_device.reset_fences(&[self.vk_fence])?;
        }

        self.keyboard_swapchain.release_image()?;
        Ok(())
    }

    pub fn step_keepalive_swapchain(&mut self) -> Result<()> {
        let img_idx = self.keepalive_swapchain.acquire_image()?;
        self.keepalive_swapchain.wait_image(xr::Duration::INFINITE)?;
        let image = *self
            .keepalive_images
            .get(img_idx as usize)
            .ok_or_else(|| anyhow!("Keepalive swapchain image index {} out of bounds", img_idx))?;

        unsafe {
            self.vk_device
                .reset_command_buffer(self.vk_cmd_buf, vk::CommandBufferResetFlags::empty())?;
            self.vk_device.begin_command_buffer(
                self.vk_cmd_buf,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .base_mip_level(0)
                        .level_count(1)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
            self.vk_device.cmd_pipeline_barrier(
                self.vk_cmd_buf,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
            self.vk_device.end_command_buffer(self.vk_cmd_buf)?;

            let cmd_bufs = [self.vk_cmd_buf];
            let submit_info = vk::SubmitInfo::default().command_buffers(&cmd_bufs);
            self.vk_device
                .queue_submit(self.vk_queue, &[submit_info], self.vk_fence)?;
            self.vk_device
                .wait_for_fences(&[self.vk_fence], true, 100_000_000)?;
            self.vk_device.reset_fences(&[self.vk_fence])?;
        }

        self.keepalive_swapchain.release_image()?;
        Ok(())
    }
}

pub fn format_extension_names() -> Vec<&'static str> {
    REQUIRED_EXTENSIONS.to_vec()
}

pub fn check_cstr_name(bytes: &[u8]) -> &str {
    CStr::from_bytes_until_nul(bytes)
        .ok()
        .and_then(|c| c.to_str().ok())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vdesk_protocol::VideoCodec;

    fn sample_monitors() -> Vec<MonitorDescriptor> {
        vec![
            MonitorDescriptor {
                stream_id: 101,
                name: "Primary".into(),
                width: 2560,
                height: 1440,
                refresh_rate_mhz: 90_000,
                logical_x: 0,
                logical_y: 0,
                codec: VideoCodec::Av1YUV420,
            },
            MonitorDescriptor {
                stream_id: 102,
                name: "PortraitRight".into(),
                width: 1080,
                height: 1920,
                refresh_rate_mhz: 90_000,
                logical_x: 2560,
                logical_y: 0,
                codec: VideoCodec::Av1YUV420,
            },
        ]
    }

    #[test]
    fn dual_monitor_layout_and_raycast_hit_correct_monitors() {
        let screens = layout_cylinder_screens(&sample_monitors());
        assert_eq!(screens.len(), 2);

        let eye = Vec3::new(0.0, DEFAULT_EYE_HEIGHT_M, 0.0);

        // Straight ahead hits primary monitor 101 at (0.5, 0.5)
        match raycast_monitors(&screens, eye, Vec3::NEG_Z) {
            Some(InputEvent::PointerMotionAbsolute { stream_id, u, v }) => {
                assert_eq!(stream_id, 101);
                assert!((u - 0.5).abs() < 1e-4);
                assert!((v - 0.5).abs() < 1e-4);
            }
            other => panic!("Expected primary hit, got {:?}", other),
        }

        // Rotating right by ~0.83 rad (-Y rotation around +Y) hits monitor 102
        let right_dir = Quat::from_rotation_y(-0.83) * Vec3::NEG_Z;
        match raycast_monitors(&screens, eye, right_dir) {
            Some(InputEvent::PointerMotionAbsolute { stream_id, u, v }) => {
                assert_eq!(stream_id, 102);
                assert!((0.0..=1.0).contains(&u));
                assert!((0.0..=1.0).contains(&v));
            }
            other => panic!("Expected secondary monitor 102 hit, got {:?}", other),
        }
    }

    #[test]
    fn cylinder_layer_params_preserve_geometry() {
        let screens = layout_cylinder_screens(&sample_monitors());
        let params = build_cylinder_layer_params(&screens[0]);
        assert_eq!(params.stream_id, 101);
        assert!((params.radius - DEFAULT_CYLINDER_RADIUS_M).abs() < 1e-5);
        assert!((params.central_angle - PRIMARY_MONITOR_ARC_RAD).abs() < 1e-5);
        assert!((params.aspect_ratio - (2560.0 / 1440.0)).abs() < 1e-5);
        assert!((params.pose.position.y - DEFAULT_EYE_HEIGHT_M).abs() < 1e-5);
    }
}
