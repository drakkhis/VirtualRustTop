pub mod audio;
pub mod decoder;
pub mod keyboard;
pub mod net;
pub mod pointer;
pub mod raycast;
pub mod xr_session;

#[cfg(target_os = "android")]
enum GrabTarget {
    Monitor {
        stream_id: u32,
        rel_transform: glam::Mat4,
    },
    Keyboard {
        rel_transform: glam::Mat4,
    },
}

#[cfg(target_os = "android")]
fn drain_android_events(
    app: &android_activity::AndroidApp,
    client: Option<&net::QuicClient>,
) -> bool {
    let mut should_exit = false;
    for _ in 0..32 {
        let mut got_event = false;
        app.poll_events(Some(std::time::Duration::ZERO), |ev| match ev {
            android_activity::PollEvent::Timeout => {}
            android_activity::PollEvent::Wake => {
                got_event = true;
            }
            android_activity::PollEvent::Main(main_ev) => {
                got_event = true;
                match main_ev {
                    android_activity::MainEvent::Destroy => {
                        should_exit = true;
                    }
                    android_activity::MainEvent::InputAvailable => {
                        if let Ok(mut iter) = app.input_events_iter() {
                            while iter.next(|input_ev| {
                                if let android_activity::input::InputEvent::KeyEvent(ke) = input_ev
                                {
                                    let pressed = match ke.action() {
                                        android_activity::input::KeyAction::Down => Some(true),
                                        android_activity::input::KeyAction::Up => Some(false),
                                        _ => None,
                                    };
                                    let raw_code = u32::from(ke.key_code()) as i32;
                                    if let (Some(p), Some(evdev_keycode), Some(c)) = (
                                        pressed,
                                        keyboard::android_keycode_to_evdev(raw_code),
                                        client,
                                    ) {
                                        let _ = c.send_input(
                                            &vdesk_protocol::InputEvent::KeyboardKey {
                                                evdev_keycode,
                                                pressed: p,
                                            },
                                        );
                                        return android_activity::InputStatus::Handled;
                                    }
                                }
                                android_activity::InputStatus::Unhandled
                            }) {}
                        }
                    }
                    _ => {}
                }
            }
            _ => {
                got_event = true;
            }
        });
        if !got_event || should_exit {
            break;
        }
    }
    should_exit
}

#[cfg(target_os = "android")]
fn sync_keyboard_screens(
    monitor_swapchains: &[xr_session::MonitorSwapchain],
    keyboard: &mut keyboard::VirtualKeyboard,
) {
    let mut ordered: Vec<&xr_session::MonitorSwapchain> = monitor_swapchains.iter().collect();
    ordered.sort_by_key(|m| (m.logical_x, m.screen.stream_id));
    let entries = ordered
        .into_iter()
        .map(|m| keyboard::ScreenUiEntry {
            stream_id: m.screen.stream_id,
            name: m.name.clone(),
            width: m.width,
            height: m.height,
            hidden: m.hidden,
            head_locked: m.head_lock.is_some(),
            pinned: m.pinned,
        })
        .collect();
    keyboard.sync_screens(entries);
}

#[cfg(target_os = "android")]
fn hide_hud(keyboard: &mut keyboard::VirtualKeyboard) {
    keyboard.mode = keyboard::PanelMode::Hidden;
    keyboard.hovered = [None, None];
    keyboard.pressed = [None, None];
    keyboard.cursors = [None, None];
    keyboard.dirty = true;
}

#[cfg(target_os = "android")]
fn recenter_unpinned(
    xr_sess: &xr_session::QuestXrSession,
    time: openxr::Time,
    monitors: &[vdesk_protocol::MonitorDescriptor],
    monitor_swapchains: &mut [xr_session::MonitorSwapchain],
    keyboard: &mut keyboard::VirtualKeyboard,
) {
    let (head_pos, head_rot) = xr_sess
        .locate_head_pose(time)
        .ok()
        .flatten()
        .unwrap_or((glam::Vec3::ZERO, glam::Quat::IDENTITY));
    let head_world = xr_session::centered_screen_transform_from_head(head_pos, head_rot);

    let defaults = xr_session::layout_cylinder_screens(monitors);
    for mon_sw in monitor_swapchains.iter_mut() {
        if mon_sw.pinned || mon_sw.head_lock.is_some() {
            continue;
        }
        if let Some(def) = defaults
            .iter()
            .find(|s| s.stream_id == mon_sw.screen.stream_id)
        {
            mon_sw.screen.radius = def.radius;
            mon_sw.screen.central_angle = def.central_angle;
            mon_sw
                .screen
                .set_transform(head_world * def.world_from_cylinder);
        }
    }
    if keyboard.is_hud_panel() {
        keyboard.hud_view_offset = keyboard::VirtualKeyboard::default_view_pose();
        let hw = glam::Mat4::from_rotation_translation(head_rot, head_pos);
        keyboard.world_from_quad =
            xr_session::resolve_head_locked_world(hw, keyboard.hud_view_offset);
    } else if keyboard.head_lock.is_none() {
        keyboard.world_from_quad =
            keyboard::VirtualKeyboard::default_pose_from_head(head_pos, head_rot);
    }
}

#[cfg(target_os = "android")]
fn recenter_all_default(
    xr_sess: &xr_session::QuestXrSession,
    time: openxr::Time,
    active_client: &net::QuicClient,
    monitor_swapchains: &mut [xr_session::MonitorSwapchain],
    keyboard: &mut keyboard::VirtualKeyboard,
) {
    let (head_pos, head_rot) = xr_sess
        .locate_head_pose(time)
        .ok()
        .flatten()
        .unwrap_or((glam::Vec3::ZERO, glam::Quat::IDENTITY));
    let head_world = xr_session::centered_screen_transform_from_head(head_pos, head_rot);

    let defaults = xr_session::layout_cylinder_screens(&active_client.monitors);
    for mon_sw in monitor_swapchains.iter_mut() {
        if mon_sw.hidden {
            mon_sw.hidden = false;
            active_client.set_stream_active(mon_sw.screen.stream_id, true);
        }
        mon_sw.head_lock = None;
        if let Some(def) = defaults
            .iter()
            .find(|s| s.stream_id == mon_sw.screen.stream_id)
        {
            mon_sw.screen.radius = def.radius;
            mon_sw.screen.central_angle = def.central_angle;
            mon_sw
                .screen
                .set_transform(head_world * def.world_from_cylinder);
        }
    }
    keyboard.head_lock = None;
    keyboard.hud_view_offset = keyboard::VirtualKeyboard::default_view_pose();
    keyboard.world_from_quad =
        keyboard::VirtualKeyboard::default_pose_from_head(head_pos, head_rot);
    keyboard.mode = keyboard::PanelMode::Collapsed;
    keyboard.hovered = [None, None];
    keyboard.pressed = [None, None];
    keyboard.cursors = [None, None];
    keyboard.dirty = true;
    sync_keyboard_screens(monitor_swapchains, keyboard);
}

#[cfg(target_os = "android")]
fn toggle_keyboard_follow(
    keyboard: &mut keyboard::VirtualKeyboard,
    head_pose: Option<(glam::Vec3, glam::Quat)>,
) -> bool {
    if keyboard.head_lock.is_some() {
        keyboard.head_lock = None;
        keyboard.dirty = true;
        false
    } else if let Some((hp, hr)) = head_pose {
        let head_world = glam::Mat4::from_rotation_translation(hr, hp);
        keyboard.head_lock = Some(xr_session::compute_head_lock_offset(
            head_world,
            keyboard.world_from_quad,
        ));
        keyboard.dirty = true;
        true
    } else {
        false
    }
}

#[cfg(target_os = "android")]
fn toggle_screen_headlock(
    active_client: &net::QuicClient,
    mon_sw: &mut xr_session::MonitorSwapchain,
    head_pose: Option<(glam::Vec3, glam::Quat)>,
) -> bool {
    if mon_sw.head_lock.is_some() {
        mon_sw.head_lock = None;
        false
    } else if let Some((hp, hr)) = head_pose {
        let head_world = glam::Mat4::from_rotation_translation(hr, hp);
        if mon_sw.hidden {
            mon_sw.hidden = false;
            active_client.set_stream_active(mon_sw.screen.stream_id, true);
            let yaw_world = xr_session::centered_screen_transform_from_head(hp, hr);
            mon_sw.screen.set_transform(yaw_world);
        }
        mon_sw.head_lock = Some(xr_session::compute_head_lock_offset(
            head_world,
            mon_sw.screen.world_from_cylinder,
        ));
        true
    } else {
        false
    }
}

#[cfg(target_os = "android")]
fn bring_screen_to_head(
    active_client: &net::QuicClient,
    mon_sw: &mut xr_session::MonitorSwapchain,
    head_pose: Option<(glam::Vec3, glam::Quat)>,
) {
    if let Some((hp, hr)) = head_pose {
        if mon_sw.hidden {
            mon_sw.hidden = false;
            active_client.set_stream_active(mon_sw.screen.stream_id, true);
        }
        let yaw_world = xr_session::centered_screen_transform_from_head(hp, hr);
        mon_sw.screen.set_transform(yaw_world);
        if mon_sw.head_lock.is_some() {
            let head_world = glam::Mat4::from_rotation_translation(hr, hp);
            mon_sw.head_lock = Some(xr_session::compute_head_lock_offset(head_world, yaw_world));
        }
    }
}

#[cfg(target_os = "android")]
struct HeldKbKey {
    keycode: u32,
    pressed_at: std::time::Instant,
    last_repeat: std::time::Instant,
}

#[cfg(target_os = "android")]
fn is_repeatable_keycode(code: u32) -> bool {
    matches!(code, 14 | 57 | 103 | 105 | 106 | 108 | 111)
}

#[cfg(target_os = "android")]
fn release_one_shot_modifiers(
    active_client: &net::QuicClient,
    keyboard: &mut keyboard::VirtualKeyboard,
) {
    for (flag, mod_code) in [
        (&mut keyboard.shift, 42u32),
        (&mut keyboard.rshift, 54u32),
        (&mut keyboard.ctrl, 29u32),
        (&mut keyboard.rctrl, 97u32),
        (&mut keyboard.alt, 56u32),
        (&mut keyboard.ralt, 100u32),
        (&mut keyboard.super_key, 125u32),
        (&mut keyboard.rsuper_key, 126u32),
    ] {
        if *flag {
            *flag = false;
            keyboard.dirty = true;
            let _ = active_client.send_input(&vdesk_protocol::InputEvent::KeyboardKey {
                evdev_keycode: mod_code,
                pressed: false,
            });
        }
    }
}

#[cfg(target_os = "android")]
fn release_kb_key(
    active_client: &net::QuicClient,
    keyboard: &mut keyboard::VirtualKeyboard,
    hand_idx: usize,
    held: &mut Option<HeldKbKey>,
) {
    if keyboard.pressed[hand_idx].take().is_some() {
        keyboard.dirty = true;
    }
    if let Some(hk) = held.take() {
        let _ = active_client.send_input(&vdesk_protocol::InputEvent::KeyboardKey {
            evdev_keycode: hk.keycode,
            pressed: false,
        });
        release_one_shot_modifiers(active_client, keyboard);
    }
}

#[cfg(target_os = "android")]
#[allow(clippy::too_many_arguments)]
fn press_kb_key(
    xr_sess: &xr_session::QuestXrSession,
    display_time: openxr::Time,
    head_pose: Option<(glam::Vec3, glam::Quat)>,
    active_client: &net::QuicClient,
    monitor_swapchains: &mut [xr_session::MonitorSwapchain],
    keyboard: &mut keyboard::VirtualKeyboard,
    pointer_stabilizer: &mut pointer::PointerStabilizer,
    hand_idx: usize,
    kidx: usize,
    held: &mut Option<HeldKbKey>,
) {
    xr_sess.pulse_haptic(hand_idx == 1, 0.35, 18);
    keyboard.pressed[hand_idx] = Some(kidx);
    keyboard.dirty = true;
    match keyboard.active_rects()[kidx].def.action {
        keyboard::KeyAction::Key(code) => {
            let _ = active_client.send_input(&vdesk_protocol::InputEvent::KeyboardKey {
                evdev_keycode: code,
                pressed: true,
            });
            if is_repeatable_keycode(code) {
                let now = std::time::Instant::now();
                *held = Some(HeldKbKey {
                    keycode: code,
                    pressed_at: now,
                    last_repeat: now,
                });
            } else {
                let _ = active_client.send_input(&vdesk_protocol::InputEvent::KeyboardKey {
                    evdev_keycode: code,
                    pressed: false,
                });
                release_one_shot_modifiers(active_client, keyboard);
                *held = None;
            }
        }
        keyboard::KeyAction::Modifier(code) => {
            let active = match code {
                42 => {
                    keyboard.shift = !keyboard.shift;
                    keyboard.shift
                }
                54 => {
                    keyboard.rshift = !keyboard.rshift;
                    keyboard.rshift
                }
                29 => {
                    keyboard.ctrl = !keyboard.ctrl;
                    keyboard.ctrl
                }
                97 => {
                    keyboard.rctrl = !keyboard.rctrl;
                    keyboard.rctrl
                }
                56 => {
                    keyboard.alt = !keyboard.alt;
                    keyboard.alt
                }
                100 => {
                    keyboard.ralt = !keyboard.ralt;
                    keyboard.ralt
                }
                125 => {
                    keyboard.super_key = !keyboard.super_key;
                    keyboard.super_key
                }
                126 => {
                    keyboard.rsuper_key = !keyboard.rsuper_key;
                    keyboard.rsuper_key
                }
                _ => false,
            };
            let _ = active_client.send_input(&vdesk_protocol::InputEvent::KeyboardKey {
                evdev_keycode: code,
                pressed: active,
            });
        }
        keyboard::KeyAction::CapsLock => {
            keyboard.caps = !keyboard.caps;
            for p in [true, false] {
                let _ = active_client.send_input(&vdesk_protocol::InputEvent::KeyboardKey {
                    evdev_keycode: 58,
                    pressed: p,
                });
            }
        }
        keyboard::KeyAction::NumLock => {
            keyboard.num_lock = !keyboard.num_lock;
            for p in [true, false] {
                let _ = active_client.send_input(&vdesk_protocol::InputEvent::KeyboardKey {
                    evdev_keycode: 69,
                    pressed: p,
                });
            }
        }
        keyboard::KeyAction::ScrollLock => {
            keyboard.scroll_lock = !keyboard.scroll_lock;
            for p in [true, false] {
                let _ = active_client.send_input(&vdesk_protocol::InputEvent::KeyboardKey {
                    evdev_keycode: 70,
                    pressed: p,
                });
            }
        }
        keyboard::KeyAction::TogglePin => {
            let new_pin = !monitor_swapchains.iter().all(|m| m.pinned);
            for m in monitor_swapchains.iter_mut() {
                m.pinned = new_pin;
            }
            keyboard.monitors_pinned = new_pin;
            sync_keyboard_screens(monitor_swapchains, keyboard);
        }
        keyboard::KeyAction::ToggleFollow => {
            let following = toggle_keyboard_follow(keyboard, head_pose);
            xr_sess.pulse_haptic(
                hand_idx == 1,
                if following { 0.55 } else { 0.25 },
                if following { 35 } else { 18 },
            );
        }
        keyboard::KeyAction::Recenter => {
            recenter_unpinned(
                xr_sess,
                display_time,
                &active_client.monitors,
                monitor_swapchains,
                keyboard,
            );
        }
        keyboard::KeyAction::ShowKeyboard => {
            keyboard.mode = keyboard::PanelMode::Keyboard;
            keyboard.hovered = [None, None];
            keyboard.pressed = [None, None];
        }
        keyboard::KeyAction::ToggleScreens => {
            if keyboard.mode == keyboard::PanelMode::Screens {
                hide_hud(keyboard);
            } else {
                keyboard.mode = keyboard::PanelMode::Screens;
                keyboard.hovered = [None, None];
                keyboard.pressed = [None, None];
            }
        }
        keyboard::KeyAction::Collapse => {
            hide_hud(keyboard);
        }
        keyboard::KeyAction::ToggleScreenHidden(stream_id) => {
            if let Some(m) = monitor_swapchains
                .iter_mut()
                .find(|m| m.screen.stream_id == stream_id)
            {
                m.hidden = !m.hidden;
                active_client.set_stream_active(stream_id, !m.hidden);
                pointer_stabilizer.reset();
                sync_keyboard_screens(monitor_swapchains, keyboard);
            }
        }
        keyboard::KeyAction::ToggleScreenHeadlock(stream_id) => {
            if let Some(m) = monitor_swapchains
                .iter_mut()
                .find(|m| m.screen.stream_id == stream_id)
            {
                let _ = toggle_screen_headlock(active_client, m, head_pose);
                pointer_stabilizer.reset();
                sync_keyboard_screens(monitor_swapchains, keyboard);
            }
        }
        keyboard::KeyAction::BringScreen(stream_id) => {
            if let Some(m) = monitor_swapchains
                .iter_mut()
                .find(|m| m.screen.stream_id == stream_id)
            {
                bring_screen_to_head(active_client, m, head_pose);
                pointer_stabilizer.reset();
                sync_keyboard_screens(monitor_swapchains, keyboard);
            }
        }
        keyboard::KeyAction::ShowAllScreens => {
            for m in monitor_swapchains.iter_mut() {
                if m.hidden {
                    m.hidden = false;
                    active_client.set_stream_active(m.screen.stream_id, true);
                }
            }
            pointer_stabilizer.reset();
            sync_keyboard_screens(monitor_swapchains, keyboard);
        }
        keyboard::KeyAction::UnlockAllHeadlocks => {
            for m in monitor_swapchains.iter_mut() {
                m.head_lock = None;
            }
            sync_keyboard_screens(monitor_swapchains, keyboard);
        }
    }
}

#[cfg(target_os = "android")]
fn acquire_wifi_low_latency_lock(
    env: &mut jni::JNIEnv<'_>,
    activity_ptr: *mut std::ffi::c_void,
) -> Option<jni::objects::GlobalRef> {
    if activity_ptr.is_null() {
        log::warn!("Skipping Wi-Fi low-latency lock: NativeActivity pointer is null");
        return None;
    }
    let attempt = (|| -> jni::errors::Result<Option<jni::objects::GlobalRef>> {
        let activity = std::mem::ManuallyDrop::new(unsafe {
            jni::objects::JObject::from_raw(activity_ptr as jni::sys::jobject)
        });
        let ctx = env
            .call_method(
                &*activity,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )?
            .l()?;
        if ctx.is_null() {
            return Ok(None);
        }
        let wifi_service = env.new_string("wifi")?;
        let mgr = env
            .call_method(
                &ctx,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[jni::objects::JValue::Object(&wifi_service)],
            )?
            .l()?;
        if mgr.is_null() {
            return Ok(None);
        }
        let tag = env.new_string("vdesk-low-latency")?;
        // WifiManager.WIFI_MODE_FULL_LOW_LATENCY == 4 (API 29+)
        let lock = env
            .call_method(
                &mgr,
                "createWifiLock",
                "(ILjava/lang/String;)Landroid/net/wifi/WifiManager$WifiLock;",
                &[
                    jni::objects::JValue::Int(4),
                    jni::objects::JValue::Object(&tag),
                ],
            )?
            .l()?;
        if lock.is_null() {
            return Ok(None);
        }
        env.call_method(
            &lock,
            "setReferenceCounted",
            "(Z)V",
            &[jni::objects::JValue::Bool(0)],
        )?;
        env.call_method(&lock, "acquire", "()V", &[])?;
        let global = env.new_global_ref(&lock)?;
        log::info!("Acquired Android WIFI_MODE_FULL_LOW_LATENCY lock");
        Ok(Some(global))
    })();

    match attempt {
        Ok(lock) => lock,
        Err(e) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            log::warn!(
                "Failed to acquire Android WIFI_MODE_FULL_LOW_LATENCY lock: {:?}",
                e
            );
            None
        }
    }
}

#[cfg(target_os = "android")]
fn release_wifi_low_latency_lock(env: &mut jni::JNIEnv<'_>, lock: Option<jni::objects::GlobalRef>) {
    if let Some(lock) = lock {
        if let Err(e) = env.call_method(lock.as_obj(), "release", "()V", &[]) {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            log::warn!("Failed to release Android Wi-Fi lock: {:?}", e);
        } else {
            log::info!("Released Android WIFI_MODE_FULL_LOW_LATENCY lock");
        }
    }
}

#[cfg(target_os = "android")]
fn load_configured_host(app: &android_activity::AndroidApp) -> Option<std::net::SocketAddr> {
    if let Some(addr) = std::env::var("VDESK_HOST_ADDR")
        .ok()
        .and_then(|v| net::parse_host_addr(&v))
    {
        log::info!("Using VDESK_HOST_ADDR fallback: {}", addr);
        return Some(addr);
    }

    for path in [
        app.external_data_path().map(|p| p.join("host.txt")),
        app.internal_data_path().map(|p| p.join("host.txt")),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Some(addr) = net::parse_host_addr(&raw) {
                log::info!("Loaded host fallback {} from {}", addr, path.display());
                return Some(addr);
            }
        }
    }

    None
}

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: android_activity::AndroidApp) {
    use anyhow::Context;
    use openxr as xr;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("vdesk-quest starting");

    let run = || -> anyhow::Result<()> {
        let mut window_ready = app.native_window().is_some();
        let wait_start = std::time::Instant::now();
        while !window_ready && wait_start.elapsed() < std::time::Duration::from_secs(3) {
            app.poll_events(Some(std::time::Duration::from_millis(10)), |_ev| {});
            if app.native_window().is_some() {
                window_ready = true;
            }
        }
        log::info!("NativeActivity window ready: {}", window_ready);

        let rt = tokio::runtime::Runtime::new()?;
        let fallback_addr = load_configured_host(&app);
        if fallback_addr.is_none() {
            log::info!(
                "No fallback host configured; using LAN auto-discovery (UDP {})",
                vdesk_protocol::DISCOVER_PORT
            );
        }

        // Initialize OpenXR and JNI immediately so Quest 3 exits the 3-dots loading screen
        // into Passthrough without waiting on network connectivity.
        let mut xr_sess = xr_session::QuestXrSession::new()?;
        let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut _) }
            .context("Failed to wrap JavaVM")?;
        let mut jni_env = vm
            .attach_current_thread()
            .context("Failed to attach JNI thread")?;
        let wifi_lock = acquire_wifi_low_latency_lock(&mut jni_env, app.activity_as_ptr());
        let jni_raw = jni_env.get_raw();

        let (client_tx, mut client_rx) = mpsc::channel::<Arc<net::QuicClient>>(1);

        rt.spawn(async move {
            loop {
                let Some(target_addr) = net::discover_host(fallback_addr).await else {
                    log::info!(
                        "Searching for vdesk-host via LAN discovery (UDP {})...",
                        vdesk_protocol::DISCOVER_PORT
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                };
                log::info!("Connecting to vdesk-host at {}...", target_addr);
                match net::QuicClient::connect(target_addr).await {
                    Ok(c) => {
                        log::info!(
                            "Connected to vdesk-host with {} monitor(s)",
                            c.monitors.len()
                        );
                        let _ = client_tx.send(Arc::new(c)).await;
                        break;
                    }
                    Err(e) => {
                        log::warn!("Waiting for vdesk-host ({}): {:#}", target_addr, e);
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
        });

        const HUD_IDLE_HIDE: std::time::Duration = std::time::Duration::from_secs(5);
        const SCREENS_IDLE_HIDE: std::time::Duration = std::time::Duration::from_secs(10);

        let mut client: Option<Arc<net::QuicClient>> = None;
        let mut pending_client: Option<Arc<net::QuicClient>> = None;
        let mut monitor_swapchains: Vec<xr_session::MonitorSwapchain> = Vec::new();
        let mut keyboard = keyboard::VirtualKeyboard::new();
        let mut grabbed: Option<GrabTarget> = None;
        let mut kb_held: [Option<HeldKbKey>; 2] = [None, None];
        let mut event_storage = xr::EventDataBuffer::new();
        let mut session_running = false;
        let mut pointer_stabilizer = pointer::PointerStabilizer::new();
        let mut right_trig_engaged = false;
        let mut left_trig_engaged = false;
        let mut kb_trig = [keyboard::KbTriggerState::new(); 2];
        let mut trigger_down = [false; 2];
        let mut desktop_left_down = false;
        let mut right_click_down = false;
        let mut left_rclick_held = false;
        let mut kb_cursor_dirty = false;
        let mut last_kb_upload = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut last_hud_activity = std::time::Instant::now();
        let mut initial_recentered = false;
        let mut pending_system_recenter: Option<xr::Time> = None;
        let mut suppress_a_until_release = false;
        let mut suppress_b_until_release = false;
        let mut last_frame = std::time::Instant::now();

        let loop_res: anyhow::Result<()> = (|| {
            loop {
                if drain_android_events(&app, client.as_deref()) {
                    return Ok(());
                }

                while let Some(ev) = xr_sess.instance.poll_event(&mut event_storage)? {
                    match ev {
                        xr::Event::SessionStateChanged(state_ev) => {
                            log::info!("OpenXR session state -> {:?}", state_ev.state());
                            match state_ev.state() {
                                xr::SessionState::READY => {
                                    xr_sess
                                        .session
                                        .begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                                    session_running = true;
                                }
                                xr::SessionState::STOPPING => {
                                    xr_sess.session.end()?;
                                    session_running = false;
                                }
                                xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => {
                                    return Ok(());
                                }
                                _ => {}
                            }
                        }
                        xr::Event::ReferenceSpaceChangePending(change_ev) => {
                            if matches!(
                                change_ev.reference_space_type(),
                                xr::ReferenceSpaceType::LOCAL | xr::ReferenceSpaceType::STAGE
                            ) {
                                log::info!(
                                    "OpenXR reference space change ({:?}) at {:?}",
                                    change_ev.reference_space_type(),
                                    change_ev.change_time()
                                );
                                pending_system_recenter = Some(change_ev.change_time());
                            }
                        }
                        _ => {}
                    }
                }

                if let Ok(connected_client) = client_rx.try_recv() {
                    pending_client = Some(connected_client);
                }

                if !session_running {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }

                if let Some(connected_client) = pending_client.take() {
                    let screens = xr_session::layout_cylinder_screens(&connected_client.monitors);
                    monitor_swapchains.clear();
                    initial_recentered = false;
                    let mut queues = HashMap::with_capacity(screens.len());
                    let weak_client = Arc::downgrade(&connected_client);
                    let on_request_idr: Arc<dyn Fn(u32) + Send + Sync> =
                        Arc::new(move |stream_id| {
                            if let Some(c) = weak_client.upgrade() {
                                c.request_idr(stream_id);
                            }
                        });
                    for screen in screens {
                        if let Some(mon) = connected_client
                            .monitors
                            .iter()
                            .find(|m| m.stream_id == screen.stream_id)
                        {
                            let (sw, queue) = unsafe {
                                xr_sess.create_android_monitor_swapchain(
                                    jni_raw,
                                    mon,
                                    screen,
                                    Arc::clone(&on_request_idr),
                                )?
                            };
                            log::info!(
                                "Created Android surface swapchain & dedicated decoder worker for {} (stream_id={}, {}x{})",
                                mon.name,
                                mon.stream_id,
                                mon.width,
                                mon.height
                            );
                            queues.insert(mon.stream_id, queue);
                            monitor_swapchains.push(sw);
                        }
                    }
                    connected_client.start_video_streams(queues);
                    sync_keyboard_screens(&monitor_swapchains, &mut keyboard);
                    client = Some(connected_client);
                }

                let now = std::time::Instant::now();
                let dt = (now - last_frame).as_secs_f32().clamp(0.001, 0.1);
                last_frame = now;

                let frame_state = xr_sess.frame_waiter.wait()?;
                xr_sess.frame_stream.begin()?;

                if !frame_state.should_render {
                    xr_sess.frame_stream.end(
                        frame_state.predicted_display_time,
                        xr::EnvironmentBlendMode::OPAQUE,
                        &[],
                    )?;
                    continue;
                }
                xr_sess.step_keepalive_swapchain()?;

                if let Some(change_time) = pending_system_recenter {
                    if frame_state.predicted_display_time.as_nanos() >= change_time.as_nanos() {
                        pending_system_recenter = None;
                        grabbed = None;
                        pointer_stabilizer.reset();
                        last_hud_activity = now;
                        if let Some(ref active_client) = client {
                            if !monitor_swapchains.is_empty() {
                                recenter_all_default(
                                    &xr_sess,
                                    frame_state.predicted_display_time,
                                    active_client,
                                    &mut monitor_swapchains,
                                    &mut keyboard,
                                );
                                initial_recentered = true;
                                xr_sess.pulse_haptic(false, 0.35, 35);
                            } else if let Ok(Some((hp, hr))) =
                                xr_sess.locate_head_pose(frame_state.predicted_display_time)
                            {
                                keyboard.hud_view_offset =
                                    keyboard::VirtualKeyboard::default_view_pose();
                                keyboard.world_from_quad =
                                    keyboard::VirtualKeyboard::default_pose_from_head(hp, hr);
                                keyboard.mode = keyboard::PanelMode::Collapsed;
                                keyboard.dirty = true;
                            }
                        } else if let Ok(Some((hp, hr))) =
                            xr_sess.locate_head_pose(frame_state.predicted_display_time)
                        {
                            keyboard.hud_view_offset =
                                keyboard::VirtualKeyboard::default_view_pose();
                            keyboard.world_from_quad =
                                keyboard::VirtualKeyboard::default_pose_from_head(hp, hr);
                            keyboard.mode = keyboard::PanelMode::Collapsed;
                            keyboard.dirty = true;
                        }
                    }
                }

                if !initial_recentered && !monitor_swapchains.is_empty() {
                    if let Some(ref active_client) = client {
                        recenter_unpinned(
                            &xr_sess,
                            frame_state.predicted_display_time,
                            &active_client.monitors,
                            &mut monitor_swapchains,
                            &mut keyboard,
                        );
                        initial_recentered = true;
                        last_hud_activity = now;
                    }
                }

                let head_pose = xr_sess
                    .locate_head_pose(frame_state.predicted_display_time)
                    .ok()
                    .flatten();
                let head_world =
                    head_pose.map(|(p, r)| glam::Mat4::from_rotation_translation(r, p));
                if let Some(hw) = head_world {
                    for mon_sw in monitor_swapchains.iter_mut() {
                        if let Some(head_from_cyl) = mon_sw.head_lock {
                            mon_sw
                                .screen
                                .set_transform(xr_session::resolve_head_locked_world(
                                    hw,
                                    head_from_cyl,
                                ));
                        }
                    }
                    if let Some(view_offset) = keyboard.active_view_offset() {
                        keyboard.world_from_quad =
                            xr_session::resolve_head_locked_world(hw, view_offset);
                    }
                }

                if let Some(ref active_client) = client {
                    let _ = xr_sess
                        .session
                        .sync_actions(&[(&xr_sess.action_set).into()]);

                    // X button (left controller): toggle virtual keyboard
                    if let Ok(x_st) = xr_sess
                        .left_x_action
                        .state(&xr_sess.session, xr::Path::NULL)
                    {
                        if x_st.changed_since_last_sync && x_st.current_state {
                            for (h, held) in kb_held.iter_mut().enumerate() {
                                release_kb_key(active_client, &mut keyboard, h, held);
                            }
                            if keyboard.mode == keyboard::PanelMode::Keyboard {
                                hide_hud(&mut keyboard);
                            } else {
                                keyboard.mode = keyboard::PanelMode::Keyboard;
                                keyboard.hovered = [None, None];
                                keyboard.pressed = [None, None];
                                keyboard.cursors = [None, None];
                                keyboard.dirty = true;
                                if keyboard.head_lock.is_none() {
                                    if let Some((hp, hr)) = head_pose {
                                        keyboard.world_from_quad =
                                            keyboard::VirtualKeyboard::default_pose_from_head(
                                                hp, hr,
                                            );
                                    }
                                }
                            }
                            log::info!("HUD mode={:?}", keyboard.mode);
                        }
                    }

                    // Left Thumbstick Click: summon / toggle Screens management panel
                    if let Ok(ls_st) = xr_sess
                        .left_thumbstick_click_action
                        .state(&xr_sess.session, xr::Path::NULL)
                    {
                        if ls_st.changed_since_last_sync && ls_st.current_state {
                            for (h, held) in kb_held.iter_mut().enumerate() {
                                release_kb_key(active_client, &mut keyboard, h, held);
                            }
                            if keyboard.mode == keyboard::PanelMode::Screens {
                                hide_hud(&mut keyboard);
                            } else {
                                keyboard.mode = keyboard::PanelMode::Screens;
                                keyboard.hovered = [None, None];
                                keyboard.pressed = [None, None];
                                keyboard.cursors = [None, None];
                                keyboard.dirty = true;
                                last_hud_activity = now;
                                if let Some(hw) = head_world {
                                    keyboard.world_from_quad =
                                        xr_session::resolve_head_locked_world(
                                            hw,
                                            keyboard.hud_view_offset,
                                        );
                                }
                            }
                            xr_sess.pulse_haptic(true, 0.25, 16);
                            log::info!("HUD mode={:?}", keyboard.mode);
                        }
                    }

                    let aim_pose = xr_sess
                        .locate_aim_pose(frame_state.predicted_display_time)
                        .ok()
                        .flatten();
                    let aim_ray = aim_pose.map(|(o, q)| (o, (q * glam::Vec3::NEG_Z).normalize()));
                    let left_aim_pose = xr_sess
                        .locate_left_aim_pose(frame_state.predicted_display_time)
                        .ok()
                        .flatten();
                    let left_aim_ray =
                        left_aim_pose.map(|(o, q)| (o, (q * glam::Vec3::NEG_Z).normalize()));

                    let closest_monitor_hit = |ray: Option<(glam::Vec3, glam::Vec3)>| {
                        ray.and_then(|(origin, dir)| {
                            monitor_swapchains
                                .iter()
                                .filter(|m| !m.hidden && m.decoder.has_rendered_frame())
                                .filter_map(|m| {
                                    m.screen
                                        .intersect_ray(origin, dir)
                                        .map(|(t, uv)| (t, m.screen.stream_id, uv))
                                })
                                .min_by(|a, b| a.0.total_cmp(&b.0))
                        })
                    };
                    let monitor_hit = closest_monitor_hit(aim_ray);

                    // Keyboard / toolbar quad hit (pass-through transparent areas when collapsed, disabled when hidden)
                    let intersect_kb = |ray: Option<(glam::Vec3, glam::Vec3)>| {
                        if !keyboard.is_visible() {
                            return None;
                        }
                        ray.and_then(|(origin, dir)| {
                            raycast::intersect_quad(
                                keyboard.world_from_quad,
                                keyboard::KB_QUAD_WIDTH_M,
                                keyboard::KB_QUAD_HEIGHT_M,
                                origin,
                                dir,
                            )
                            .filter(|(_, uv)| {
                                keyboard.is_expanded() || keyboard.hit_test(uv.x, uv.y).is_some()
                            })
                        })
                    };
                    let kb_hits = [intersect_kb(aim_ray), intersect_kb(left_aim_ray)];

                    // Y button (left controller): recenter unpinned/unlocked monitors + keyboard
                    let y_pressed = xr_sess
                        .left_y_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .map(|s| s.changed_since_last_sync && s.current_state)
                        .unwrap_or(false);
                    if y_pressed {
                        recenter_unpinned(
                            &xr_sess,
                            frame_state.predicted_display_time,
                            &active_client.monitors,
                            &mut monitor_swapchains,
                            &mut keyboard,
                        );
                    }

                    let grip_val = xr_sess
                        .right_grip_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .map(|s| s.current_state)
                        .unwrap_or(0.0);
                    let stick = xr_sess
                        .right_thumbstick_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .map(|s| glam::Vec2::new(s.current_state.x, s.current_state.y))
                        .unwrap_or(glam::Vec2::ZERO);
                    let a_state = xr_sess
                        .right_a_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .ok();
                    let b_state = xr_sess
                        .right_b_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .ok();
                    if !a_state.is_some_and(|s| s.current_state) {
                        suppress_a_until_release = false;
                    }
                    if !b_state.is_some_and(|s| s.current_state) {
                        suppress_b_until_release = false;
                    }

                    // Grip grab / move / resize + Grab+A (Headlock/Follow) / Grab+B (Hide)
                    if grip_val > 0.5 {
                        if let Some((aim_pos, aim_rot)) = aim_pose {
                            let aim_mat = glam::Mat4::from_rotation_translation(aim_rot, aim_pos);
                            if grabbed.is_none() {
                                if kb_hits[0].is_some() {
                                    grabbed = Some(GrabTarget::Keyboard {
                                        rel_transform: aim_mat.inverse() * keyboard.world_from_quad,
                                    });
                                } else if let Some((_, hit_id, _)) = monitor_hit {
                                    if let Some(m) = monitor_swapchains
                                        .iter()
                                        .find(|m| m.screen.stream_id == hit_id)
                                    {
                                        if !m.pinned {
                                            if keyboard.is_hud_panel() {
                                                hide_hud(&mut keyboard);
                                            }
                                            grabbed = Some(GrabTarget::Monitor {
                                                stream_id: hit_id,
                                                rel_transform: aim_mat.inverse()
                                                    * m.screen.world_from_cylinder,
                                            });
                                        }
                                    }
                                }
                            }

                            let mut hide_grabbed = false;
                            match &mut grabbed {
                                Some(GrabTarget::Keyboard { rel_transform }) => {
                                    if stick.x.abs() > 0.15 {
                                        *rel_transform = glam::Mat4::from_translation(
                                            glam::Vec3::new(0.0, 0.0, -stick.x * dt * 1.2),
                                        ) * *rel_transform;
                                    }
                                    let new_world =
                                        xr_session::rigid_transform(aim_mat * *rel_transform);
                                    keyboard.world_from_quad = new_world;
                                    last_hud_activity = now;
                                    if let Some(hw) = head_world {
                                        if keyboard.is_hud_panel() {
                                            keyboard.hud_view_offset =
                                                xr_session::compute_head_lock_offset(hw, new_world);
                                        } else if keyboard.head_lock.is_some() {
                                            keyboard.head_lock = Some(
                                                xr_session::compute_head_lock_offset(hw, new_world),
                                            );
                                        }
                                    }
                                    if keyboard.mode == keyboard::PanelMode::Keyboard
                                        && a_state.is_some_and(|s| {
                                            s.changed_since_last_sync && s.current_state
                                        })
                                    {
                                        suppress_a_until_release = true;
                                        let following =
                                            toggle_keyboard_follow(&mut keyboard, head_pose);
                                        xr_sess.pulse_haptic(
                                            false,
                                            if following { 0.55 } else { 0.25 },
                                            if following { 40 } else { 18 },
                                        );
                                    }
                                }
                                Some(GrabTarget::Monitor {
                                    stream_id,
                                    rel_transform,
                                }) => {
                                    if stick.x.abs() > 0.15 {
                                        *rel_transform = glam::Mat4::from_translation(
                                            glam::Vec3::new(0.0, 0.0, -stick.x * dt * 1.5),
                                        ) * *rel_transform;
                                    }
                                    if let Some(m) = monitor_swapchains
                                        .iter_mut()
                                        .find(|m| m.screen.stream_id == *stream_id)
                                    {
                                        if stick.y.abs() > 0.15 {
                                            m.screen.central_angle = (m.screen.central_angle
                                                * (1.0 + stick.y * dt * 1.2))
                                                .clamp(0.25, 2.2);
                                        }
                                        let new_world =
                                            xr_session::rigid_transform(aim_mat * *rel_transform);
                                        m.screen.set_transform(new_world);
                                        if m.head_lock.is_some() {
                                            if let Some(hw) = head_world {
                                                m.head_lock =
                                                    Some(xr_session::compute_head_lock_offset(
                                                        hw, new_world,
                                                    ));
                                            }
                                        }
                                        if a_state.is_some_and(|s| {
                                            s.changed_since_last_sync && s.current_state
                                        }) {
                                            suppress_a_until_release = true;
                                            let locked =
                                                toggle_screen_headlock(active_client, m, head_pose);
                                            xr_sess.pulse_haptic(
                                                false,
                                                if locked { 0.55 } else { 0.25 },
                                                if locked { 40 } else { 18 },
                                            );
                                            sync_keyboard_screens(
                                                &monitor_swapchains,
                                                &mut keyboard,
                                            );
                                        } else if b_state.is_some_and(|s| {
                                            s.changed_since_last_sync && s.current_state
                                        }) {
                                            suppress_b_until_release = true;
                                            if !m.hidden {
                                                m.hidden = true;
                                                active_client
                                                    .set_stream_active(m.screen.stream_id, false);
                                            }
                                            hide_grabbed = true;
                                            xr_sess.pulse_haptic(false, 0.35, 25);
                                            sync_keyboard_screens(
                                                &monitor_swapchains,
                                                &mut keyboard,
                                            );
                                        }
                                    }
                                }
                                None => {}
                            }
                            if hide_grabbed {
                                grabbed = None;
                                pointer_stabilizer.reset();
                            }
                        }
                    } else {
                        grabbed = None;
                    }

                    if grabbed.is_some() {
                        pointer_stabilizer.reset();
                        for h in 0..2 {
                            release_kb_key(active_client, &mut keyboard, h, &mut kb_held[h]);
                            kb_trig[h].reset();
                            if keyboard.cursors[h].take().is_some() {
                                keyboard.dirty = true;
                            }
                            if keyboard.hovered[h].take().is_some() {
                                keyboard.dirty = true;
                            }
                        }
                        left_rclick_held = false;
                        if desktop_left_down {
                            desktop_left_down = false;
                            let _ = active_client.send_input(
                                &vdesk_protocol::InputEvent::PointerButton {
                                    button: 0x110, // BTN_LEFT
                                    pressed: false,
                                },
                            );
                        }
                        if right_click_down {
                            right_click_down = false;
                            let _ = active_client.send_input(
                                &vdesk_protocol::InputEvent::PointerButton {
                                    button: 0x111, // BTN_RIGHT
                                    pressed: false,
                                },
                            );
                        }
                    }

                    let a_down =
                        a_state.is_some_and(|s| s.current_state) && !suppress_a_until_release;
                    let b_down =
                        b_state.is_some_and(|s| s.current_state) && !suppress_b_until_release;
                    let stick_click = xr_sess
                        .right_thumbstick_click_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .map(|s| s.current_state)
                        .unwrap_or(false);
                    let right_trig_val = xr_sess
                        .trigger_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .map(|s| s.current_state)
                        .unwrap_or(0.0);
                    let left_trig_val = xr_sess
                        .left_trigger_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .map(|s| s.current_state)
                        .unwrap_or(0.0);
                    right_trig_engaged = if right_trig_engaged {
                        right_trig_val > 0.35
                    } else {
                        right_trig_val > 0.55
                    };
                    left_trig_engaged = if left_trig_engaged {
                        left_trig_val > 0.35
                    } else {
                        left_trig_val > 0.55
                    };
                    let trig_pressed = [right_trig_engaged || a_down, left_trig_engaged];

                    // Pointer & Dual-Hand Virtual Keyboard / HUD interaction (when not grabbing)
                    if grabbed.is_none() {
                        if keyboard.is_hud_panel() && (kb_hits[0].is_some() || kb_hits[1].is_some())
                        {
                            last_hud_activity = now;
                        }
                        let kb_closer = [kb_hits[0].is_some(), kb_hits[1].is_some()];

                        for h in 0..2 {
                            let (new_cursor, new_hover) = if kb_closer[h] {
                                if let Some((_, uv)) = kb_hits[h] {
                                    let cu = uv.x.clamp(0.0, 1.0);
                                    let cv = uv.y.clamp(0.0, 1.0);
                                    (Some((cu, cv)), keyboard.hit_test(cu, cv))
                                } else {
                                    (None, None)
                                }
                            } else {
                                (None, None)
                            };

                            let cursor_changed = match (keyboard.cursors[h], new_cursor) {
                                (None, None) => None,
                                (Some((ou, ov)), Some((nu, nv))) => {
                                    if (nu - ou).abs() >= 0.0025 || (nv - ov).abs() >= 0.0025 {
                                        Some(false)
                                    } else {
                                        None
                                    }
                                }
                                _ => Some(true),
                            };
                            match cursor_changed {
                                Some(true) => {
                                    keyboard.cursors[h] = new_cursor;
                                    keyboard.dirty = true;
                                }
                                Some(false) => {
                                    keyboard.cursors[h] = new_cursor;
                                    kb_cursor_dirty = true;
                                }
                                None => {}
                            }

                            if new_hover != keyboard.hovered[h] {
                                keyboard.hovered[h] = new_hover;
                                keyboard.dirty = true;
                                if new_hover.is_some() {
                                    xr_sess.pulse_haptic(h == 1, 0.12, 8);
                                }
                            }

                            if kb_closer[h] {
                                if h == 0 {
                                    pointer_stabilizer.reset();
                                }
                                let kb_pressed = if h == 0 {
                                    kb_trig[0].update(right_trig_val) || a_down
                                } else {
                                    kb_trig[1].update(left_trig_val)
                                };
                                if kb_pressed && !trigger_down[h] && !(h == 1 && left_rclick_held) {
                                    trigger_down[h] = true;
                                    if let Some(kidx) = new_hover {
                                        last_hud_activity = now;
                                        press_kb_key(
                                            &xr_sess,
                                            frame_state.predicted_display_time,
                                            head_pose,
                                            active_client,
                                            &mut monitor_swapchains,
                                            &mut keyboard,
                                            &mut pointer_stabilizer,
                                            h,
                                            kidx,
                                            &mut kb_held[h],
                                        );
                                    }
                                } else if !kb_pressed && trigger_down[h] {
                                    trigger_down[h] = false;
                                    release_kb_key(
                                        active_client,
                                        &mut keyboard,
                                        h,
                                        &mut kb_held[h],
                                    );
                                } else if kb_pressed && trigger_down[h] {
                                    if let Some(ref mut hk) = kb_held[h] {
                                        if is_repeatable_keycode(hk.keycode)
                                            && hk.pressed_at.elapsed()
                                                >= std::time::Duration::from_millis(350)
                                            && hk.last_repeat.elapsed()
                                                >= std::time::Duration::from_millis(65)
                                        {
                                            let _ = active_client.send_input(
                                                &vdesk_protocol::InputEvent::KeyboardKey {
                                                    evdev_keycode: hk.keycode,
                                                    pressed: false,
                                                },
                                            );
                                            let _ = active_client.send_input(
                                                &vdesk_protocol::InputEvent::KeyboardKey {
                                                    evdev_keycode: hk.keycode,
                                                    pressed: true,
                                                },
                                            );
                                            hk.last_repeat = now;
                                            xr_sess.pulse_haptic(h == 1, 0.15, 8);
                                        }
                                    }
                                }
                            } else if h == 0 {
                                kb_trig[0].reset();
                                let desktop_btn_down = (trig_pressed[0] && kb_held[0].is_none())
                                    || b_down
                                    || stick_click
                                    || (!kb_closer[1]
                                        && trig_pressed[1]
                                        && kb_held[1].is_none()
                                        && !trigger_down[1]);
                                if let Some((_, stream_id, uv)) = monitor_hit {
                                    let (mon_w, mon_h) = monitor_swapchains
                                        .iter()
                                        .find(|m| m.screen.stream_id == stream_id)
                                        .map(|m| (m.width as f32, m.height as f32))
                                        .unwrap_or((1920.0, 1080.0));
                                    if let Some(stab_uv) = pointer_stabilizer.update(
                                        stream_id,
                                        uv,
                                        mon_w,
                                        mon_h,
                                        right_trig_val,
                                        desktop_btn_down,
                                        dt,
                                        now,
                                    ) {
                                        let _ = active_client.send_input(
                                            &vdesk_protocol::InputEvent::PointerMotionAbsolute {
                                                stream_id,
                                                u: stab_uv.x,
                                                v: stab_uv.y,
                                            },
                                        );
                                    }
                                }
                                if trig_pressed[0] && !trigger_down[0] {
                                    trigger_down[0] = true;
                                    if kb_held[0].is_some() {
                                        release_kb_key(
                                            active_client,
                                            &mut keyboard,
                                            0,
                                            &mut kb_held[0],
                                        );
                                    } else if monitor_hit.is_some() {
                                        if keyboard.is_hud_panel() {
                                            hide_hud(&mut keyboard);
                                        }
                                        desktop_left_down = true;
                                        let _ = active_client.send_input(
                                            &vdesk_protocol::InputEvent::PointerButton {
                                                button: 0x110, // BTN_LEFT
                                                pressed: true,
                                            },
                                        );
                                    } else if keyboard.mode == keyboard::PanelMode::Hidden {
                                        keyboard.mode = keyboard::PanelMode::Collapsed;
                                        keyboard.hovered = [None, None];
                                        keyboard.pressed = [None, None];
                                        keyboard.cursors = [None, None];
                                        keyboard.dirty = true;
                                        last_hud_activity = now;
                                        if let Some(hw) = head_world {
                                            keyboard.world_from_quad =
                                                xr_session::resolve_head_locked_world(
                                                    hw,
                                                    keyboard.hud_view_offset,
                                                );
                                        }
                                        xr_sess.pulse_haptic(false, 0.22, 14);
                                    } else if keyboard.is_hud_panel() {
                                        hide_hud(&mut keyboard);
                                        xr_sess.pulse_haptic(false, 0.16, 10);
                                    }
                                } else if !trig_pressed[0] && trigger_down[0] {
                                    trigger_down[0] = false;
                                    if kb_held[0].is_some() {
                                        release_kb_key(
                                            active_client,
                                            &mut keyboard,
                                            0,
                                            &mut kb_held[0],
                                        );
                                    }
                                    if desktop_left_down {
                                        desktop_left_down = false;
                                        let _ = active_client.send_input(
                                            &vdesk_protocol::InputEvent::PointerButton {
                                                button: 0x110, // BTN_LEFT
                                                pressed: false,
                                            },
                                        );
                                    }
                                }

                                // Scroll via right thumbstick only while pointing at a monitor
                                if monitor_hit.is_some()
                                    && (stick.x.abs() > 0.15 || stick.y.abs() > 0.15)
                                {
                                    if keyboard.is_hud_panel() {
                                        hide_hud(&mut keyboard);
                                    }
                                    let _ = active_client.send_input(
                                        &vdesk_protocol::InputEvent::PointerAxisScroll {
                                            dx: stick.x * 0.5,
                                            dy: stick.y * 0.5,
                                        },
                                    );
                                }
                            } else {
                                kb_trig[1].reset();
                                if !trig_pressed[1] && trigger_down[1] {
                                    trigger_down[1] = false;
                                    release_kb_key(
                                        active_client,
                                        &mut keyboard,
                                        1,
                                        &mut kb_held[1],
                                    );
                                }
                            }
                        }

                        // Right-click via Right B button, Left Trigger (away from keyboard), or Right Thumbstick Click
                        left_rclick_held = !kb_closer[1]
                            && trig_pressed[1]
                            && kb_held[1].is_none()
                            && !trigger_down[1];
                        let raw_rclick = b_down || left_rclick_held || stick_click;
                        let rclick = if right_click_down {
                            raw_rclick
                        } else {
                            raw_rclick && monitor_hit.is_some()
                        };
                        if rclick != right_click_down {
                            right_click_down = rclick;
                            if rclick && keyboard.is_hud_panel() {
                                hide_hud(&mut keyboard);
                            }
                            let _ = active_client.send_input(
                                &vdesk_protocol::InputEvent::PointerButton {
                                    button: 0x111, // BTN_RIGHT
                                    pressed: rclick,
                                },
                            );
                        }

                        if (keyboard.mode == keyboard::PanelMode::Collapsed
                            && last_hud_activity.elapsed() >= HUD_IDLE_HIDE)
                            || (keyboard.mode == keyboard::PanelMode::Screens
                                && last_hud_activity.elapsed() >= SCREENS_IDLE_HIDE)
                        {
                            hide_hud(&mut keyboard);
                        }
                    }
                }

                if keyboard.is_visible()
                    && (keyboard.dirty
                        || (kb_cursor_dirty
                            && last_kb_upload.elapsed() >= std::time::Duration::from_millis(16)))
                {
                    let rgba = keyboard.render_rgba();
                    xr_sess.upload_keyboard_rgba(rgba)?;
                    keyboard.dirty = false;
                    kb_cursor_dirty = false;
                    last_kb_upload = now;
                }

                let settings = xr::sys::CompositionLayerSettingsFB {
                    ty: xr::sys::CompositionLayerSettingsFB::TYPE,
                    next: std::ptr::null(),
                    layer_flags: xr::sys::CompositionLayerSettingsFlagsFB::NORMAL_SHARPENING
                        | xr::sys::CompositionLayerSettingsFlagsFB::QUALITY_SUPER_SAMPLING,
                };

                let pt_raw = xr_sess.build_passthrough_layer();
                let mut cyl_layers: Vec<xr::sys::CompositionLayerCylinderKHR> =
                    Vec::with_capacity(monitor_swapchains.len() + 1);

                for mon_sw in monitor_swapchains.iter().filter(|m| {
                    !m.hidden && m.decoder.has_rendered_frame() && m.head_lock.is_none()
                }) {
                    let params = xr_session::build_cylinder_layer_params(&mon_sw.screen);
                    let layer = xr_session::build_cylinder_layer(
                        &params,
                        &xr_sess.local_space,
                        &mon_sw.swapchain,
                        mon_sw.width as i32,
                        mon_sw.height as i32,
                    );
                    let mut raw = layer.into_raw();
                    xr_session::apply_layer_settings_sharpening(&mut raw, &settings);
                    cyl_layers.push(raw);
                }

                for mon_sw in monitor_swapchains
                    .iter()
                    .filter(|m| !m.hidden && m.decoder.has_rendered_frame())
                {
                    let Some(head_from_cyl) = mon_sw.head_lock else {
                        continue;
                    };
                    let params = xr_session::build_cylinder_layer_params_with_transform(
                        &mon_sw.screen,
                        head_from_cyl,
                    );
                    let layer = xr_session::build_cylinder_layer(
                        &params,
                        &xr_sess.view_space,
                        &mon_sw.swapchain,
                        mon_sw.width as i32,
                        mon_sw.height as i32,
                    );
                    let mut raw = layer.into_raw();
                    xr_session::apply_layer_settings_sharpening(&mut raw, &settings);
                    cyl_layers.push(raw);
                }

                let any_monitor_ready = monitor_swapchains
                    .iter()
                    .any(|m| m.decoder.has_rendered_frame());
                let keepalive_params = if !any_monitor_ready {
                    let keepalive_screen = xr_session::build_default_cylinder_screen(0, 1280, 720);
                    xr_session::build_cylinder_layer_params(&keepalive_screen)
                } else {
                    xr_session::CylinderLayerParams {
                        stream_id: 0,
                        radius: 0.01,
                        central_angle: 0.01,
                        aspect_ratio: 1.0,
                        pose: xr::Posef {
                            orientation: xr::Quaternionf::IDENTITY,
                            position: xr::Vector3f {
                                x: 0.0,
                                y: 0.0,
                                z: 2.0,
                            },
                        },
                    }
                };
                let keepalive_layer = xr_session::build_cylinder_layer(
                    &keepalive_params,
                    &xr_sess.local_space,
                    &xr_sess.keepalive_swapchain,
                    64,
                    64,
                );
                cyl_layers.push(keepalive_layer.into_raw());

                let kb_quad_raw = if keyboard.is_visible() {
                    let (kb_space, kb_pose_mat) = match keyboard.active_view_offset() {
                        Some(view_offset) => (&xr_sess.view_space, view_offset),
                        None => (&xr_sess.local_space, keyboard.world_from_quad),
                    };
                    Some(
                        xr_session::build_quad_layer(
                            kb_pose_mat,
                            keyboard::KB_QUAD_WIDTH_M,
                            keyboard::KB_QUAD_HEIGHT_M,
                            kb_space,
                            &xr_sess.keyboard_swapchain,
                            keyboard::KB_TEX_WIDTH as i32,
                            keyboard::KB_TEX_HEIGHT as i32,
                        )
                        .into_raw(),
                    )
                } else {
                    None
                };

                let mut layer_ptrs: Vec<&xr::CompositionLayerBase<'_, xr::Vulkan>> =
                    Vec::with_capacity(2 + cyl_layers.len());
                if let Some(ref pt) = pt_raw {
                    layer_ptrs.push(unsafe {
                        &*(pt as *const _ as *const xr::CompositionLayerBase<'_, xr::Vulkan>)
                    });
                }
                for cyl in &cyl_layers {
                    layer_ptrs.push(unsafe {
                        &*(cyl as *const _ as *const xr::CompositionLayerBase<'_, xr::Vulkan>)
                    });
                }
                if let Some(ref kb_quad) = kb_quad_raw {
                    layer_ptrs.push(unsafe {
                        &*(kb_quad as *const _ as *const xr::CompositionLayerBase<'_, xr::Vulkan>)
                    });
                }

                xr_sess.frame_stream.end(
                    frame_state.predicted_display_time,
                    xr::EnvironmentBlendMode::OPAQUE,
                    &layer_ptrs,
                )?;
            }
        })();

        release_wifi_low_latency_lock(&mut jni_env, wifi_lock);
        loop_res
    };

    if let Err(e) = run() {
        log::error!("vdesk-quest runtime error: {:#}", e);
    }
}
