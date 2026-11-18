use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    AbsInfo, AbsoluteAxisType, AttributeSet, EventType, InputEvent as EvdevEvent, Key,
    RelativeAxisType, UinputAbsSetup,
};
use vdesk_protocol::InputEvent;

#[allow(dead_code)]
pub struct KernelInputInjector {
    device: VirtualDevice,
    pub desktop_width: i32,
    pub desktop_height: i32,
}

impl KernelInputInjector {
    pub fn new(desktop_width: i32, desktop_height: i32) -> anyhow::Result<Self> {
        let mut keys = AttributeSet::<Key>::new();
        // Register standard mouse buttons
        keys.insert(Key::BTN_LEFT);
        keys.insert(Key::BTN_RIGHT);
        keys.insert(Key::BTN_MIDDLE);
        // Register all standard keyboard keys (1..248)
        for code in 1..248 {
            keys.insert(Key::new(code));
        }

        let mut rel_axes = AttributeSet::<RelativeAxisType>::new();
        rel_axes.insert(RelativeAxisType::REL_WHEEL_HI_RES);
        rel_axes.insert(RelativeAxisType::REL_HWHEEL_HI_RES);

        let abs_x = UinputAbsSetup::new(
            AbsoluteAxisType::ABS_X,
            AbsInfo::new(0, 0, desktop_width, 0, 0, 1),
        );
        let abs_y = UinputAbsSetup::new(
            AbsoluteAxisType::ABS_Y,
            AbsInfo::new(0, 0, desktop_height, 0, 0, 1),
        );

        let device = VirtualDeviceBuilder::new()?
            .name("vdesk-rs Quest 3 Spatial Input")
            .with_keys(&keys)?
            .with_relative_axes(&rel_axes)?
            .with_absolute_axis(&abs_x)?
            .with_absolute_axis(&abs_y)?
            .build()?;

        Ok(Self {
            device,
            desktop_width,
            desktop_height,
        })
    }

    pub fn inject(
        &mut self,
        event: InputEvent,
        mon_offset_x: i32,
        mon_offset_y: i32,
        mon_w: u32,
        mon_h: u32,
    ) -> anyhow::Result<()> {
        match event {
            InputEvent::PointerMotionAbsolute { u, v, .. } => {
                let global_x = mon_offset_x + (u.clamp(0.0, 1.0) * mon_w as f32) as i32;
                let global_y = mon_offset_y + (v.clamp(0.0, 1.0) * mon_h as f32) as i32;
                self.device.emit(&[
                    EvdevEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_X.0, global_x),
                    EvdevEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_Y.0, global_y),
                ])?;
            }
            InputEvent::PointerButton { button, pressed } => {
                self.device.emit(&[EvdevEvent::new(
                    EventType::KEY,
                    button as u16,
                    if pressed { 1 } else { 0 },
                )])?;
            }
            InputEvent::PointerAxisScroll { dx, dy } => {
                self.device.emit(&[
                    EvdevEvent::new(
                        EventType::RELATIVE,
                        RelativeAxisType::REL_HWHEEL_HI_RES.0,
                        (dx * 120.0) as i32,
                    ),
                    EvdevEvent::new(
                        EventType::RELATIVE,
                        RelativeAxisType::REL_WHEEL_HI_RES.0,
                        (dy * 120.0) as i32,
                    ),
                ])?;
            }
            InputEvent::KeyboardKey {
                evdev_keycode,
                pressed,
            } => {
                self.device.emit(&[EvdevEvent::new(
                    EventType::KEY,
                    evdev_keycode as u16,
                    if pressed { 1 } else { 0 },
                )])?;
            }
        }
        Ok(())
    }
}
