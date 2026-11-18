pub mod audio;
pub mod decoder;
pub mod keyboard;
pub mod net;
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
                                if let android_activity::input::InputEvent::KeyEvent(ke) = input_ev {
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
    let fwd = head_rot * glam::Vec3::NEG_Z;
    let yaw = (-fwd.x).atan2(-fwd.z);
    let head_world =
        glam::Mat4::from_translation(head_pos) * glam::Mat4::from_rotation_y(yaw);

    let defaults = xr_session::layout_cylinder_screens(monitors);
    for mon_sw in monitor_swapchains.iter_mut() {
        if mon_sw.pinned {
            continue;
        }
        if let Some(def) = defaults.iter().find(|s| s.stream_id == mon_sw.screen.stream_id) {
            mon_sw.screen.radius = def.radius;
            mon_sw.screen.central_angle = def.central_angle;
            mon_sw.screen.set_transform(head_world * def.world_from_cylinder);
        }
    }
    keyboard.world_from_quad = keyboard::VirtualKeyboard::default_pose_from_head(head_pos, head_rot);
}

#[cfg(target_os = "android")]
struct HeldKbKey {
    keycode: u32,
    pressed_at: std::time::Instant,
    last_repeat: std::time::Instant,
}

#[cfg(target_os = "android")]
fn is_repeatable_keycode(code: u32) -> bool {
    matches!(code, 14 | 57 | 103 | 105 | 106 | 108)
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
        for (flag, mod_code) in [
            (&mut keyboard.shift, 42u32),
            (&mut keyboard.ctrl, 29u32),
            (&mut keyboard.alt, 56u32),
            (&mut keyboard.super_key, 125u32),
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
}

#[cfg(target_os = "android")]
#[allow(clippy::too_many_arguments)]
fn press_kb_key(
    xr_sess: &xr_session::QuestXrSession,
    display_time: openxr::Time,
    active_client: &net::QuicClient,
    monitor_swapchains: &mut [xr_session::MonitorSwapchain],
    keyboard: &mut keyboard::VirtualKeyboard,
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
            let now = std::time::Instant::now();
            *held = Some(HeldKbKey {
                keycode: code,
                pressed_at: now,
                last_repeat: now,
            });
        }
        keyboard::KeyAction::Modifier(code) => {
            let active = match code {
                42 => {
                    keyboard.shift = !keyboard.shift;
                    keyboard.shift
                }
                29 => {
                    keyboard.ctrl = !keyboard.ctrl;
                    keyboard.ctrl
                }
                56 => {
                    keyboard.alt = !keyboard.alt;
                    keyboard.alt
                }
                125 => {
                    keyboard.super_key = !keyboard.super_key;
                    keyboard.super_key
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
        keyboard::KeyAction::TogglePin => {
            let new_pin = !monitor_swapchains.iter().all(|m| m.pinned);
            for m in monitor_swapchains.iter_mut() {
                m.pinned = new_pin;
            }
            keyboard.monitors_pinned = new_pin;
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
        keyboard::KeyAction::Show => {
            keyboard.visible = true;
            keyboard.hovered = [None, None];
            keyboard.pressed = [None, None];
        }
        keyboard::KeyAction::Hide => {
            keyboard.visible = false;
            keyboard.hovered = [None, None];
            keyboard.pressed = [None, None];
        }
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: android_activity::AndroidApp) {
    use anyhow::Context;
    use openxr as xr;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use vdesk_protocol::DEFAULT_PORT;

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
        let server_addr: SocketAddr = std::env::var("VDESK_HOST_ADDR")
            .unwrap_or_else(|_| format!("192.168.1.16:{}", DEFAULT_PORT))
            .parse()
            .context("Invalid VDESK_HOST_ADDR")?;

        // Initialize OpenXR and JNI immediately so Quest 3 exits the 3-dots loading screen
        // into Passthrough without waiting on network connectivity.
        let mut xr_sess = xr_session::QuestXrSession::new()?;
        let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut _) }
            .context("Failed to wrap JavaVM")?;
        let jni_env = vm
            .attach_current_thread()
            .context("Failed to attach JNI thread")?;

        let (frame_tx, mut frame_rx) = mpsc::channel::<net::DecodedStreamPacket>(64);
        let (client_tx, mut client_rx) = mpsc::channel::<Arc<net::QuicClient>>(1);

        rt.spawn(async move {
            loop {
                let target_addr = net::discover_host(server_addr).await;
                log::info!("Connecting to vdesk-host at {}...", target_addr);
                match net::QuicClient::connect(target_addr, frame_tx.clone()).await {
                    Ok(c) => {
                        log::info!(
                            "Connected to vdesk-host with {} monitor(s)",
                            c.monitors.len()
                        );
                        let _ = client_tx.send(Arc::new(c)).await;
                        break;
                    }
                    Err(e) => {
                        log::warn!("Waiting for vdesk-host ({}): {:#}", server_addr, e);
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
        });

        let mut client: Option<Arc<net::QuicClient>> = None;
        let mut pending_monitors: Vec<(vdesk_protocol::MonitorDescriptor, raycast::CylinderScreen)> =
            Vec::new();
        let mut monitor_swapchains: Vec<xr_session::MonitorSwapchain> = Vec::new();
        let mut keyboard = keyboard::VirtualKeyboard::new();
        let mut grabbed: Option<GrabTarget> = None;
        let mut kb_held: [Option<HeldKbKey>; 2] = [None, None];
        let mut event_storage = xr::EventDataBuffer::new();
        let mut session_running = false;
        let mut trigger_down = [false; 2];
        let mut right_click_down = false;
        let mut left_rclick_held = false;
        let mut kb_cursor_dirty = false;
        let mut last_kb_upload = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut initial_recentered = false;
        let mut last_frame = std::time::Instant::now();

        loop {
            if drain_android_events(&app, client.as_deref()) {
                return Ok(());
            }

            while let Some(ev) = xr_sess.instance.poll_event(&mut event_storage)? {
                if let xr::Event::SessionStateChanged(state_ev) = ev {
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
            }

            if let Ok(connected_client) = client_rx.try_recv() {
                let screens = xr_session::layout_cylinder_screens(&connected_client.monitors);
                monitor_swapchains.clear();
                pending_monitors.clear();
                initial_recentered = false;
                for screen in screens {
                    if let Some(mon) = connected_client
                        .monitors
                        .iter()
                        .find(|m| m.stream_id == screen.stream_id)
                    {
                        log::info!(
                            "Registered pending monitor {} (stream_id={}, {}x{})",
                            mon.name,
                            mon.stream_id,
                            mon.width,
                            mon.height
                        );
                        pending_monitors.push((mon.clone(), screen));
                    }
                }
                client = Some(connected_client);
            }

            if !session_running {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }

            // Drain any pending decoded frames directly onto their Android Surface swapchains
            while let Ok(pkt) = frame_rx.try_recv() {
                if !monitor_swapchains
                    .iter()
                    .any(|m| m.screen.stream_id == pkt.header.stream_id)
                {
                    if !pkt.header.is_idr {
                        continue;
                    }
                    if let Some(pos) = pending_monitors
                        .iter()
                        .position(|(mon, _)| mon.stream_id == pkt.header.stream_id)
                    {
                        let (mon, screen) = pending_monitors.remove(pos);
                        let sw = unsafe {
                            xr_sess.create_android_monitor_swapchain(
                                jni_env.get_raw(),
                                &mon,
                                screen,
                            )?
                        };
                        log::info!(
                            "Created Android surface swapchain & AMediaCodec on first IDR for {} (stream_id={}, {}x{})",
                            mon.name,
                            mon.stream_id,
                            mon.width,
                            mon.height
                        );
                        monitor_swapchains.push(sw);
                    }
                }

                if let Some(mon_sw) = monitor_swapchains
                    .iter_mut()
                    .find(|m| m.screen.stream_id == pkt.header.stream_id)
                {
                    match mon_sw.decoder.submit_and_render(
                        &pkt.payload,
                        pkt.header.frame_index,
                        pkt.header.capture_timestamp_us,
                        pkt.header.is_idr,
                    ) {
                        Ok(true) => {
                            if let Some(ref c) = client {
                                c.request_idr(pkt.header.stream_id);
                            }
                        }
                        Ok(false) => {}
                        Err(e) => {
                            log::warn!("Decoder submit failed: {:#}", e);
                            if let Some(ref c) = client {
                                c.request_idr(pkt.header.stream_id);
                            }
                        }
                    }
                }
            }

            for mon_sw in &mut monitor_swapchains {
                let _ = mon_sw.decoder.drain_output();
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
                }
            }

            if let Some(ref active_client) = client {
                let _ = xr_sess.session.sync_actions(&[(&xr_sess.action_set).into()]);

                // X button (left controller): toggle virtual keyboard
                if let Ok(x_st) = xr_sess.left_x_action.state(&xr_sess.session, xr::Path::NULL) {
                    if x_st.changed_since_last_sync && x_st.current_state {
                        for h in 0..2 {
                            release_kb_key(active_client, &mut keyboard, h, &mut kb_held[h]);
                        }
                        keyboard.visible = !keyboard.visible;
                        keyboard.hovered = [None, None];
                        keyboard.pressed = [None, None];
                        keyboard.cursors = [None, None];
                        keyboard.dirty = true;
                        if keyboard.visible {
                            if let Ok(Some((hp, hr))) =
                                xr_sess.locate_head_pose(frame_state.predicted_display_time)
                            {
                                keyboard.world_from_quad =
                                    keyboard::VirtualKeyboard::default_pose_from_head(hp, hr);
                            }
                        }
                        log::info!("Virtual keyboard visible={}", keyboard.visible);
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
                            .filter(|m| m.decoder.has_rendered_frame())
                            .filter_map(|m| {
                                m.screen
                                    .intersect_ray(origin, dir)
                                    .map(|(t, uv)| (t, m.screen.stream_id, uv))
                            })
                            .min_by(|a, b| a.0.total_cmp(&b.0))
                    })
                };
                let monitor_hit = closest_monitor_hit(aim_ray);
                let left_monitor_hit = closest_monitor_hit(left_aim_ray);
                let monitor_hits = [monitor_hit, left_monitor_hit];

                // Keyboard / toolbar quad hit (pass-through transparent areas when collapsed)
                let intersect_kb = |ray: Option<(glam::Vec3, glam::Vec3)>| {
                    ray.and_then(|(origin, dir)| {
                        raycast::intersect_quad(
                            keyboard.world_from_quad,
                            keyboard::KB_QUAD_WIDTH_M,
                            keyboard::KB_QUAD_HEIGHT_M,
                            origin,
                            dir,
                        )
                        .filter(|(_, uv)| keyboard.visible || keyboard.hit_test(uv.x, uv.y).is_some())
                    })
                };
                let kb_hits = [intersect_kb(aim_ray), intersect_kb(left_aim_ray)];

                // Y button (left controller): recenter unpinned monitors + keyboard
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

                // Grip grab / move / resize
                if grip_val > 0.5 {
                    if let Some((aim_pos, aim_rot)) = aim_pose {
                        let aim_mat = glam::Mat4::from_rotation_translation(aim_rot, aim_pos);
                        if grabbed.is_none() {
                            let kb_closer = match (kb_hits[0], monitor_hit) {
                                (Some((kt, _)), Some((mt, _, _))) => kt <= mt,
                                (Some(_), None) => true,
                                _ => false,
                            };
                            if kb_closer {
                                grabbed = Some(GrabTarget::Keyboard {
                                    rel_transform: aim_mat.inverse() * keyboard.world_from_quad,
                                });
                            } else if let Some((_, hit_id, _)) = monitor_hit {
                                if let Some(m) = monitor_swapchains
                                    .iter()
                                    .find(|m| m.screen.stream_id == hit_id)
                                {
                                    if !m.pinned {
                                        grabbed = Some(GrabTarget::Monitor {
                                            stream_id: hit_id,
                                            rel_transform: aim_mat.inverse()
                                                * m.screen.world_from_cylinder,
                                        });
                                    }
                                }
                            }
                        }

                        match &mut grabbed {
                            Some(GrabTarget::Keyboard { rel_transform }) => {
                                if stick.x.abs() > 0.15 {
                                    *rel_transform = glam::Mat4::from_translation(glam::Vec3::new(
                                        0.0,
                                        0.0,
                                        -stick.x * dt * 1.2,
                                    )) * *rel_transform;
                                }
                                keyboard.world_from_quad = aim_mat * *rel_transform;
                            }
                            Some(GrabTarget::Monitor {
                                stream_id,
                                rel_transform,
                            }) => {
                                if stick.x.abs() > 0.15 {
                                    *rel_transform = glam::Mat4::from_translation(glam::Vec3::new(
                                        0.0,
                                        0.0,
                                        -stick.x * dt * 1.5,
                                    )) * *rel_transform;
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
                                    m.screen.set_transform(aim_mat * *rel_transform);
                                }
                            }
                            None => {}
                        }
                    }
                } else {
                    grabbed = None;
                }

                if grabbed.is_some() {
                    for h in 0..2 {
                        release_kb_key(active_client, &mut keyboard, h, &mut kb_held[h]);
                        if keyboard.cursors[h].take().is_some() {
                            keyboard.dirty = true;
                        }
                        if keyboard.hovered[h].take().is_some() {
                            keyboard.dirty = true;
                        }
                    }
                    left_rclick_held = false;
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

                let a_down = xr_sess
                    .right_a_action
                    .state(&xr_sess.session, xr::Path::NULL)
                    .map(|s| s.current_state)
                    .unwrap_or(false);
                let b_down = xr_sess
                    .right_b_action
                    .state(&xr_sess.session, xr::Path::NULL)
                    .map(|s| s.current_state)
                    .unwrap_or(false);
                let right_trig = xr_sess
                    .trigger_action
                    .state(&xr_sess.session, xr::Path::NULL)
                    .map(|s| s.current_state > 0.5)
                    .unwrap_or(false);
                let left_trig = xr_sess
                    .left_trigger_action
                    .state(&xr_sess.session, xr::Path::NULL)
                    .map(|s| s.current_state > 0.5)
                    .unwrap_or(false);
                let trig_pressed = [right_trig || a_down, left_trig];

                // Pointer & Dual-Hand Virtual Keyboard interaction (when not grabbing)
                if grabbed.is_none() {
                    let kb_closer = [
                        match (kb_hits[0], monitor_hits[0]) {
                            (Some((kt, _)), Some((mt, _, _))) => kt <= mt,
                            (Some(_), None) => true,
                            _ => false,
                        },
                        match (kb_hits[1], monitor_hits[1]) {
                            (Some((kt, _)), Some((mt, _, _))) => kt <= mt,
                            (Some(_), None) => true,
                            _ => false,
                        },
                    ];

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
                                if (nu - ou).abs() >= 0.004 || (nv - ov).abs() >= 0.004 {
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
                            if trig_pressed[h]
                                && !trigger_down[h]
                                && !(h == 1 && left_rclick_held)
                            {
                                trigger_down[h] = true;
                                if let Some(kidx) = new_hover {
                                    press_kb_key(
                                        &xr_sess,
                                        frame_state.predicted_display_time,
                                        active_client,
                                        &mut monitor_swapchains,
                                        &mut keyboard,
                                        h,
                                        kidx,
                                        &mut kb_held[h],
                                    );
                                }
                            } else if !trig_pressed[h] && trigger_down[h] {
                                trigger_down[h] = false;
                                release_kb_key(active_client, &mut keyboard, h, &mut kb_held[h]);
                            } else if trig_pressed[h] && trigger_down[h] {
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
                            if let Some((_, stream_id, uv)) = monitor_hit {
                                let _ = active_client.send_input(
                                    &vdesk_protocol::InputEvent::PointerMotionAbsolute {
                                        stream_id,
                                        u: uv.x.clamp(0.0, 1.0),
                                        v: uv.y.clamp(0.0, 1.0),
                                    },
                                );
                            }
                            if trig_pressed[0] != trigger_down[0] {
                                trigger_down[0] = trig_pressed[0];
                                if kb_held[0].is_some() {
                                    release_kb_key(
                                        active_client,
                                        &mut keyboard,
                                        0,
                                        &mut kb_held[0],
                                    );
                                } else {
                                    let _ = active_client.send_input(
                                        &vdesk_protocol::InputEvent::PointerButton {
                                            button: 0x110, // BTN_LEFT
                                            pressed: trig_pressed[0],
                                        },
                                    );
                                }
                            }

                            // Scroll via right thumbstick when not grabbing
                            if stick.x.abs() > 0.15 || stick.y.abs() > 0.15 {
                                let _ = active_client.send_input(
                                    &vdesk_protocol::InputEvent::PointerAxisScroll {
                                        dx: stick.x * 0.5,
                                        dy: stick.y * 0.5,
                                    },
                                );
                            }
                        } else if !trig_pressed[1] && trigger_down[1] {
                            trigger_down[1] = false;
                            release_kb_key(active_client, &mut keyboard, 1, &mut kb_held[1]);
                        }
                    }

                    // Right-click via Right B button, Left Trigger (away from keyboard), or Right Thumbstick Click
                    left_rclick_held = !kb_closer[1]
                        && trig_pressed[1]
                        && kb_held[1].is_none()
                        && !trigger_down[1];
                    let stick_click = xr_sess
                        .right_thumbstick_click_action
                        .state(&xr_sess.session, xr::Path::NULL)
                        .map(|s| s.current_state)
                        .unwrap_or(false);
                    let rclick = b_down || left_rclick_held || stick_click;
                    if rclick != right_click_down {
                        right_click_down = rclick;
                        let _ = active_client.send_input(
                            &vdesk_protocol::InputEvent::PointerButton {
                                button: 0x111, // BTN_RIGHT
                                pressed: rclick,
                            },
                        );
                    }
                }
            }

            if keyboard.dirty
                || (kb_cursor_dirty
                    && last_kb_upload.elapsed() >= std::time::Duration::from_millis(33))
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

            for mon_sw in monitor_swapchains
                .iter()
                .filter(|m| m.decoder.has_rendered_frame())
            {
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

            let keepalive_params = if cyl_layers.is_empty() {
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

            let kb_quad_raw = xr_session::build_quad_layer(
                keyboard.world_from_quad,
                keyboard::KB_QUAD_WIDTH_M,
                keyboard::KB_QUAD_HEIGHT_M,
                &xr_sess.local_space,
                &xr_sess.keyboard_swapchain,
                keyboard::KB_TEX_WIDTH as i32,
                keyboard::KB_TEX_HEIGHT as i32,
            )
            .into_raw();

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
            layer_ptrs.push(unsafe {
                &*(&kb_quad_raw as *const _ as *const xr::CompositionLayerBase<'_, xr::Vulkan>)
            });

            xr_sess.frame_stream.end(
                frame_state.predicted_display_time,
                xr::EnvironmentBlendMode::OPAQUE,
                &layer_ptrs,
            )?;
        }
    };

    if let Err(e) = run() {
        log::error!("vdesk-quest runtime error: {:#}", e);
    }
}
