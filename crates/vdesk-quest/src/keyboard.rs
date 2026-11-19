use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use glam::{Mat4, Quat, Vec3};

pub const KB_TEX_WIDTH: u32 = 1536;
pub const KB_TEX_HEIGHT: u32 = 576;
pub const KB_QUAD_WIDTH_M: f32 = 0.78;
pub const KB_QUAD_HEIGHT_M: f32 = 0.2925;

static FONT_BYTES: &[u8] = include_bytes!("../assets/OpenSans-Bold.ttf");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Key(u32),
    Modifier(u32),
    CapsLock,
    TogglePin,
    Recenter,
    Show,
    Hide,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyDef {
    pub label: &'static str,
    pub shift_label: &'static str,
    pub weight: f32,
    pub action: KeyAction,
}

const fn k(label: &'static str, shift_label: &'static str, weight: f32, code: u32) -> KeyDef {
    KeyDef {
        label,
        shift_label,
        weight,
        action: KeyAction::Key(code),
    }
}

const fn m(label: &'static str, weight: f32, code: u32) -> KeyDef {
    KeyDef {
        label,
        shift_label: label,
        weight,
        action: KeyAction::Modifier(code),
    }
}

const fn act(label: &'static str, weight: f32, action: KeyAction) -> KeyDef {
    KeyDef {
        label,
        shift_label: label,
        weight,
        action,
    }
}

const ROWS: [&[KeyDef]; 5] = [
    &[
        k("Esc", "Esc", 1.1, 1),
        k("1", "!", 1.0, 2),
        k("2", "@", 1.0, 3),
        k("3", "#", 1.0, 4),
        k("4", "$", 1.0, 5),
        k("5", "%", 1.0, 6),
        k("6", "^", 1.0, 7),
        k("7", "&", 1.0, 8),
        k("8", "*", 1.0, 9),
        k("9", "(", 1.0, 10),
        k("0", ")", 1.0, 11),
        k("-", "_", 1.0, 12),
        k("=", "+", 1.0, 13),
        k("Bksp", "Bksp", 1.8, 14),
    ],
    &[
        k("Tab", "Tab", 1.4, 15),
        k("q", "Q", 1.0, 16),
        k("w", "W", 1.0, 17),
        k("e", "E", 1.0, 18),
        k("r", "R", 1.0, 19),
        k("t", "T", 1.0, 20),
        k("y", "Y", 1.0, 21),
        k("u", "U", 1.0, 22),
        k("i", "I", 1.0, 23),
        k("o", "O", 1.0, 24),
        k("p", "P", 1.0, 25),
        k("[", "{", 1.0, 26),
        k("]", "}", 1.0, 27),
        k("\\", "|", 1.4, 43),
    ],
    &[
        act("Caps", 1.6, KeyAction::CapsLock),
        k("a", "A", 1.0, 30),
        k("s", "S", 1.0, 31),
        k("d", "D", 1.0, 32),
        k("f", "F", 1.0, 33),
        k("g", "G", 1.0, 34),
        k("h", "H", 1.0, 35),
        k("j", "J", 1.0, 36),
        k("k", "K", 1.0, 37),
        k("l", "L", 1.0, 38),
        k(";", ":", 1.0, 39),
        k("'", "\"", 1.0, 40),
        k("Enter", "Enter", 2.2, 28),
    ],
    &[
        m("Shift", 2.0, 42),
        k("z", "Z", 1.0, 44),
        k("x", "X", 1.0, 45),
        k("c", "C", 1.0, 46),
        k("v", "V", 1.0, 47),
        k("b", "B", 1.0, 48),
        k("n", "N", 1.0, 49),
        k("m", "M", 1.0, 50),
        k(",", "<", 1.0, 51),
        k(".", ">", 1.0, 52),
        k("/", "?", 1.0, 53),
        k("Up", "Up", 1.0, 103),
        act("Pin", 1.8, KeyAction::TogglePin),
    ],
    &[
        m("Ctrl", 1.3, 29),
        m("Super", 1.3, 125),
        m("Alt", 1.2, 56),
        k("Space", "Space", 4.4, 57),
        k("Left", "Left", 1.0, 105),
        k("Down", "Down", 1.0, 108),
        k("Right", "Right", 1.0, 106),
        act("Center", 1.5, KeyAction::Recenter),
        act("Hide", 1.3, KeyAction::Hide),
    ],
];

#[derive(Debug, Clone)]
pub struct KeyRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub def: KeyDef,
}

type KbStaticState = (bool, bool, bool, bool, bool, bool, bool);

pub struct VirtualKeyboard {
    pub visible: bool,
    pub world_from_quad: Mat4,
    pub rects: Vec<KeyRect>,
    pub toolbar_rects: Vec<KeyRect>,
    pub cursors: [Option<(f32, f32)>; 2],
    pub hovered: [Option<usize>; 2],
    pub pressed: [Option<usize>; 2],
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
    pub caps: bool,
    pub monitors_pinned: bool,
    pub dirty: bool,
    base_layer: Vec<u8>,
    base_state: Option<KbStaticState>,
    frame_buf: Vec<u8>,
    prev_active_keys: [Option<usize>; 4],
    prev_cursors: [Option<(f32, f32)>; 2],
}

impl Default for VirtualKeyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualKeyboard {
    pub fn new() -> Self {
        let mut rects = Vec::new();
        let pad_x = 0.018;
        let pad_y = 0.032;
        let row_h = (1.0 - pad_y * 2.0) / (ROWS.len() as f32);
        let gap_x = 0.008;
        let gap_y = 0.016;

        for (r_idx, row) in ROWS.iter().enumerate() {
            let total_w: f32 = row.iter().map(|k| k.weight).sum();
            let avail_w = 1.0 - pad_x * 2.0 - gap_x * ((row.len().saturating_sub(1)) as f32);
            let mut cursor_x = pad_x;
            let y0 = pad_y + (r_idx as f32) * row_h + gap_y * 0.5;
            let y1 = pad_y + ((r_idx + 1) as f32) * row_h - gap_y * 0.5;

            for key in *row {
                let w = avail_w * (key.weight / total_w);
                rects.push(KeyRect {
                    x0: cursor_x,
                    y0,
                    x1: cursor_x + w,
                    y1,
                    def: *key,
                });
                cursor_x += w + gap_x;
            }
        }

        let toolbar_rects = vec![
            KeyRect {
                x0: 0.18,
                y0: 0.76,
                x1: 0.45,
                y1: 0.96,
                def: act("Keyboard", 1.0, KeyAction::Show),
            },
            KeyRect {
                x0: 0.47,
                y0: 0.76,
                x1: 0.63,
                y1: 0.96,
                def: act("Pin", 1.0, KeyAction::TogglePin),
            },
            KeyRect {
                x0: 0.65,
                y0: 0.76,
                x1: 0.82,
                y1: 0.96,
                def: act("Center", 1.0, KeyAction::Recenter),
            },
        ];

        Self {
            visible: false,
            world_from_quad: Self::default_pose_from_head(Vec3::ZERO, Quat::IDENTITY),
            rects,
            toolbar_rects,
            cursors: [None, None],
            hovered: [None, None],
            pressed: [None, None],
            shift: false,
            ctrl: false,
            alt: false,
            super_key: false,
            caps: false,
            monitors_pinned: false,
            dirty: true,
            base_layer: Vec::new(),
            base_state: None,
            frame_buf: Vec::new(),
            prev_active_keys: [None; 4],
            prev_cursors: [None; 2],
        }
    }

    pub fn active_rects(&self) -> &[KeyRect] {
        if self.visible {
            &self.rects
        } else {
            &self.toolbar_rects
        }
    }

    pub fn default_pose_from_head(head_pos: Vec3, head_rot: Quat) -> Mat4 {
        let fwd = head_rot * Vec3::NEG_Z;
        let yaw = (-fwd.x).atan2(-fwd.z);
        let yaw_rot = Quat::from_rotation_y(yaw);
        let pos = head_pos + yaw_rot * Vec3::new(0.0, -0.35, -0.90);
        let tilt = Quat::from_rotation_x(-25.0_f32.to_radians());
        Mat4::from_rotation_translation(yaw_rot * tilt, pos)
    }

    pub fn hit_test(&self, u: f32, v: f32) -> Option<usize> {
        if let Some(exact) = self
            .active_rects()
            .iter()
            .position(|r| u >= r.x0 && u <= r.x1 && v >= r.y0 && v <= r.y1)
        {
            return Some(exact);
        }
        if !self.visible {
            return None;
        }
        self.rects
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| {
                let dx = (r.x0 - u).max(0.0).max(u - r.x1);
                let dy = (r.y0 - v).max(0.0).max(v - r.y1);
                if dx <= 0.012 && dy <= 0.024 {
                    let dist_px = (dx * KB_TEX_WIDTH as f32).hypot(dy * KB_TEX_HEIGHT as f32);
                    Some((idx, dist_px))
                } else {
                    None
                }
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(idx, _)| idx)
    }

    pub fn is_modifier_active(&self, action: KeyAction) -> bool {
        modifier_active_from_flags(
            action,
            self.shift,
            self.caps,
            self.ctrl,
            self.alt,
            self.super_key,
            self.monitors_pinned,
        )
    }

    pub fn render_rgba(&mut self) -> &[u8] {
        let w = KB_TEX_WIDTH as usize;
        let h = KB_TEX_HEIGHT as usize;
        let expected_len = w * h * 4;
        let static_key: KbStaticState = (
            self.visible,
            self.shift,
            self.caps,
            self.ctrl,
            self.alt,
            self.super_key,
            self.monitors_pinned,
        );
        let font = FontRef::try_from_slice(FONT_BYTES).ok();
        let upper = self.shift ^ self.caps;
        let rects = if self.visible {
            &self.rects
        } else {
            &self.toolbar_rects
        };

        if self.base_state != Some(static_key) || self.base_layer.len() != expected_len {
            self.base_layer.resize(expected_len, 0);
            self.base_layer.fill(0);

            if self.visible {
                // Frosted dark glass panel backing with rounded outer silhouette
                draw_rounded_rect_sdf(
                    &mut self.base_layer,
                    w,
                    h,
                    2.0,
                    2.0,
                    (w - 2) as f32,
                    (h - 2) as f32,
                    24.0,
                    [16, 20, 28, 232],
                    [58, 72, 96, 235],
                    2.0,
                );
            } else {
                // Compact frosted pill dock behind collapsed toolbar buttons
                draw_rounded_rect_sdf(
                    &mut self.base_layer,
                    w,
                    h,
                    0.165 * w as f32,
                    0.735 * h as f32,
                    0.835 * w as f32,
                    0.985 * h as f32,
                    22.0,
                    [16, 20, 28, 224],
                    [64, 82, 112, 230],
                    2.0,
                );
            }

            for r in rects {
                let is_active = modifier_active_from_flags(
                    r.def.action,
                    self.shift,
                    self.caps,
                    self.ctrl,
                    self.alt,
                    self.super_key,
                    self.monitors_pinned,
                );
                draw_key(
                    &mut self.base_layer,
                    w,
                    h,
                    font.as_ref(),
                    upper,
                    self.shift,
                    self.monitors_pinned,
                    r,
                    false,
                    false,
                    is_active,
                );
            }

            self.base_state = Some(static_key);
            if self.frame_buf.len() != expected_len {
                self.frame_buf.resize(expected_len, 0);
            }
            self.frame_buf.copy_from_slice(&self.base_layer);
            self.prev_active_keys = [None; 4];
            self.prev_cursors = [None; 2];
        } else {
            if self.frame_buf.len() != expected_len {
                self.frame_buf.resize(expected_len, 0);
                self.frame_buf.copy_from_slice(&self.base_layer);
            }
            for (cu, cv) in self.prev_cursors.into_iter().flatten() {
                let (x0, y0, x1, y1) = cursor_bounds_px(cu, cv, w, h);
                copy_rect(&mut self.frame_buf, &self.base_layer, w, h, x0, y0, x1, y1);
            }
            for kidx in self.prev_active_keys.into_iter().flatten() {
                if let Some(r) = rects.get(kidx) {
                    let (x0, y0, x1, y1) = key_bounds_px(r, w, h);
                    copy_rect(&mut self.frame_buf, &self.base_layer, w, h, x0, y0, x1, y1);
                }
            }
        }

        let panel_pm = if self.visible {
            [15, 18, 25, 232]
        } else {
            [14, 18, 25, 224]
        };
        let cur_active = [
            self.hovered[0],
            self.hovered[1],
            self.pressed[0],
            self.pressed[1],
        ];
        let mut drawn = [None::<usize>; 4];
        let mut drawn_len = 0usize;
        for kidx in cur_active.into_iter().flatten() {
            if drawn[..drawn_len].contains(&Some(kidx)) {
                continue;
            }
            drawn[drawn_len] = Some(kidx);
            drawn_len += 1;

            if let Some(r) = rects.get(kidx) {
                let (x0, y0, x1, y1) = key_bounds_px(r, w, h);
                fill_rect_pm(&mut self.frame_buf, w, h, x0, y0, x1, y1, panel_pm);
                let is_hovered = self.hovered.contains(&Some(kidx));
                let is_pressed = self.pressed.contains(&Some(kidx));
                let is_active = modifier_active_from_flags(
                    r.def.action,
                    self.shift,
                    self.caps,
                    self.ctrl,
                    self.alt,
                    self.super_key,
                    self.monitors_pinned,
                );
                draw_key(
                    &mut self.frame_buf,
                    w,
                    h,
                    font.as_ref(),
                    upper,
                    self.shift,
                    self.monitors_pinned,
                    r,
                    is_hovered,
                    is_pressed,
                    is_active,
                );
            }
        }

        // Draw glowing dual-hand laser reticles on top
        for (hand_idx, cursor) in self.cursors.iter().enumerate() {
            if let Some((cu, cv)) = cursor {
                let cx = cu * w as f32;
                let cy = cv * h as f32;
                let core = if hand_idx == 0 {
                    [96, 224, 255, 255]
                } else {
                    [168, 240, 160, 255]
                };
                draw_reticle(&mut self.frame_buf, w, h, cx, cy, core);
            }
        }

        self.prev_active_keys = cur_active;
        self.prev_cursors = self.cursors;
        &self.frame_buf
    }
}

fn modifier_active_from_flags(
    action: KeyAction,
    shift: bool,
    caps: bool,
    ctrl: bool,
    alt: bool,
    super_key: bool,
    monitors_pinned: bool,
) -> bool {
    match action {
        KeyAction::Modifier(42) => shift,
        KeyAction::Modifier(29) => ctrl,
        KeyAction::Modifier(56) => alt,
        KeyAction::Modifier(125) => super_key,
        KeyAction::CapsLock => caps,
        KeyAction::TogglePin => monitors_pinned,
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_rect(
    dst: &mut [u8],
    src: &[u8],
    tex_w: usize,
    tex_h: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) {
    let x0 = x0.min(tex_w);
    let x1 = x1.min(tex_w);
    let y0 = y0.min(tex_h);
    let y1 = y1.min(tex_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for py in y0..y1 {
        let row_start = (py * tex_w + x0) * 4;
        let row_end = (py * tex_w + x1) * 4;
        dst[row_start..row_end].copy_from_slice(&src[row_start..row_end]);
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect_pm(
    buf: &mut [u8],
    tex_w: usize,
    tex_h: usize,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    rgba_pm: [u8; 4],
) {
    let x0 = x0.min(tex_w);
    let x1 = x1.min(tex_w);
    let y0 = y0.min(tex_h);
    let y1 = y1.min(tex_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for py in y0..y1 {
        let row_start = (py * tex_w + x0) * 4;
        let row_end = (py * tex_w + x1) * 4;
        for px in buf[row_start..row_end].chunks_exact_mut(4) {
            px.copy_from_slice(&rgba_pm);
        }
    }
}

fn key_bounds_px(r: &KeyRect, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let x0 = ((r.x0 * w as f32) - 3.0).floor().max(0.0) as usize;
    let y0 = ((r.y0 * h as f32) - 3.0).floor().max(0.0) as usize;
    let x1 = ((r.x1 * w as f32) + 3.0).ceil().min(w as f32) as usize;
    let y1 = ((r.y1 * h as f32) + 7.0).ceil().min(h as f32) as usize;
    (x0, y0, x1, y1)
}

fn cursor_bounds_px(cu: f32, cv: f32, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let cx = cu * w as f32;
    let cy = cv * h as f32;
    let x0 = (cx - 14.0).floor().max(0.0) as usize;
    let y0 = (cy - 14.0).floor().max(0.0) as usize;
    let x1 = (cx + 14.0).ceil().min(w as f32) as usize;
    let y1 = (cy + 14.0).ceil().min(h as f32) as usize;
    (x0, y0, x1, y1)
}

#[allow(clippy::too_many_arguments)]
fn draw_key(
    buf: &mut [u8],
    w: usize,
    h: usize,
    font: Option<&FontRef<'_>>,
    upper: bool,
    shift: bool,
    monitors_pinned: bool,
    r: &KeyRect,
    is_hovered: bool,
    is_pressed: bool,
    is_active: bool,
) {
    let x0 = r.x0 * w as f32;
    let y0 = r.y0 * h as f32;
    let x1 = r.x1 * w as f32;
    let y1 = r.y1 * h as f32;

    let (bg, border, text_col) = if is_pressed {
        ([59, 130, 246, 255], [191, 219, 254, 255], [255, 255, 255, 255])
    } else if is_active {
        if r.def.action == KeyAction::TogglePin {
            ([22, 138, 86, 255], [110, 231, 183, 255], [240, 255, 248, 255])
        } else {
            ([37, 99, 235, 255], [147, 197, 253, 255], [255, 255, 255, 255])
        }
    } else if is_hovered {
        ([72, 86, 112, 255], [148, 178, 226, 255], [255, 255, 255, 255])
    } else {
        match r.def.action {
            KeyAction::Show => {
                ([34, 98, 178, 250], [96, 165, 250, 255], [245, 250, 255, 255])
            }
            KeyAction::TogglePin | KeyAction::Recenter => {
                ([36, 56, 76, 248], [82, 116, 148, 255], [226, 238, 248, 255])
            }
            KeyAction::Hide => {
                ([104, 42, 52, 248], [186, 86, 102, 255], [255, 236, 240, 255])
            }
            KeyAction::Key(_) if r.def.label.len() > 1 && r.def.label != "Space" => {
                ([34, 41, 56, 252], [68, 80, 104, 255], [216, 224, 238, 255])
            }
            KeyAction::Modifier(_) | KeyAction::CapsLock => {
                ([34, 41, 56, 252], [68, 80, 104, 255], [216, 224, 238, 255])
            }
            _ => ([44, 52, 68, 252], [80, 94, 120, 255], [244, 247, 252, 255]),
        }
    };

    let y_shift = if is_pressed { 2.0 } else { 0.0 };
    if !is_pressed {
        // Subtle bottom 3D keycap bevel shadow
        draw_rounded_rect_sdf(
            buf,
            w,
            h,
            x0,
            y0 + 3.5,
            x1,
            y1 + 3.5,
            12.0,
            [8, 10, 14, 190],
            [8, 10, 14, 190],
            0.0,
        );
    }

    draw_rounded_rect_sdf(
        buf,
        w,
        h,
        x0,
        y0 + y_shift,
        x1,
        y1 + y_shift,
        12.0,
        bg,
        border,
        1.5,
    );

    let label = if r.def.action == KeyAction::TogglePin && monitors_pinned {
        "Pinned"
    } else if shift
        || (upper
            && r.def.label.len() == 1
            && r.def.label.chars().all(|c| c.is_ascii_alphabetic()))
    {
        r.def.shift_label
    } else {
        r.def.label
    };

    if let Some(f) = font {
        let px_size = match label.chars().count() {
            1 => 34.0,
            2..=4 => 25.0,
            _ => 21.0,
        };
        draw_ttf_centered(
            buf,
            w,
            h,
            f,
            px_size,
            (x0 + x1) * 0.5,
            (y0 + y1) * 0.5 + y_shift,
            label,
            text_col,
        );
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct KbTriggerState {
    engaged: bool,
    peak: f32,
    trough: f32,
}

impl KbTriggerState {
    pub fn new() -> Self {
        Self {
            engaged: false,
            peak: 0.0,
            trough: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.engaged = false;
        self.peak = 0.0;
        self.trough = 0.0;
    }

    pub fn update(&mut self, val: f32) -> bool {
        if !self.engaged {
            self.trough = self.trough.min(val);
            if val > 0.32 && (val - self.trough) > 0.14 {
                self.engaged = true;
                self.peak = val;
            }
        } else {
            self.peak = self.peak.max(val);
            if val < 0.18 || (self.peak - val) > 0.22 {
                self.engaged = false;
                self.trough = val;
            }
        }
        self.engaged
    }
}

/// Map Android `AKEYCODE_*` hardware/Bluetooth keyboard codes to Linux `evdev` `KEY_*` codes.
pub fn android_keycode_to_evdev(keycode: i32) -> Option<u32> {
    Some(match keycode {
        7 => 11,  // 0
        8..=16 => (keycode - 8 + 2) as u32, // 1..9 -> KEY_1..KEY_9
        29 => 30, // A
        30 => 48, // B
        31 => 46, // C
        32 => 32, // D
        33 => 18, // E
        34 => 33, // F
        35 => 34, // G
        36 => 35, // H
        37 => 23, // I
        38 => 36, // J
        39 => 37, // K
        40 => 38, // L
        41 => 50, // M
        42 => 49, // N
        43 => 24, // O
        44 => 25, // P
        45 => 16, // Q
        46 => 19, // R
        47 => 31, // S
        48 => 20, // T
        49 => 22, // U
        50 => 47, // V
        51 => 17, // W
        52 => 45, // X
        53 => 21, // Y
        54 => 44, // Z
        55 => 51, // ,
        56 => 52, // .
        57 => 56, // AltLeft
        58 => 100,// AltRight
        59 => 42, // ShiftLeft
        60 => 54, // ShiftRight
        61 => 15, // Tab
        62 => 57, // Space
        66 => 28, // Enter
        67 => 14, // Del (Backspace)
        68 => 41, // Grave
        69 => 12, // Minus
        70 => 13, // Equals
        71 => 26, // LeftBracket
        72 => 27, // RightBracket
        73 => 43, // Backslash
        74 => 39, // Semicolon
        75 => 40, // Apostrophe
        76 => 53, // Slash
        19 => 103,// DpadUp
        20 => 108,// DpadDown
        21 => 105,// DpadLeft
        22 => 106,// DpadRight
        111 => 1, // Escape
        112 => 111,// ForwardDel (Delete)
        113 => 29,// CtrlLeft
        114 => 97,// CtrlRight
        115 => 58,// CapsLock
        117 => 125,// MetaLeft
        118 => 126,// MetaRight
        122 => 102,// MoveHome
        123 => 107,// MoveEnd
        92 => 104,// PageUp
        93 => 109,// PageDown
        131..=142 => {
            let f = keycode - 131;
            if f < 10 {
                (59 + f) as u32
            } else if f == 10 {
                87
            } else {
                88
            }
        }
        _ => return None,
    })
}

fn blend_pixel(dst: &mut [u8], src: [u8; 4], coverage: f32) {
    let a = (src[3] as f32 / 255.0) * coverage.clamp(0.0, 1.0);
    if a <= 0.001 {
        return;
    }
    let inv = 1.0 - a;
    dst[0] = (src[0] as f32 * a + dst[0] as f32 * inv).round().clamp(0.0, 255.0) as u8;
    dst[1] = (src[1] as f32 * a + dst[1] as f32 * inv).round().clamp(0.0, 255.0) as u8;
    dst[2] = (src[2] as f32 * a + dst[2] as f32 * inv).round().clamp(0.0, 255.0) as u8;
    let out_a = a + (dst[3] as f32 / 255.0) * inv;
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

#[allow(clippy::too_many_arguments)]
fn draw_rounded_rect_sdf(
    buf: &mut [u8],
    tex_w: usize,
    tex_h: usize,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radius: f32,
    fill: [u8; 4],
    border: [u8; 4],
    border_w: f32,
) {
    let cx = (x0 + x1) * 0.5;
    let cy = (y0 + y1) * 0.5;
    let hx = ((x1 - x0) * 0.5).max(1.0);
    let hy = ((y1 - y0) * 0.5).max(1.0);
    let r = radius.min(hx).min(hy);

    let px0 = ((x0 - 2.0).floor().max(0.0)) as usize;
    let py0 = ((y0 - 2.0).floor().max(0.0)) as usize;
    let px1 = ((x1 + 2.0).ceil().min(tex_w as f32)) as usize;
    let py1 = ((y1 + 2.0).ceil().min(tex_h as f32)) as usize;

    let fa = fill[3] as f32 / 255.0;
    let fill_pm = [
        (fill[0] as f32 * fa).round().clamp(0.0, 255.0) as u8,
        (fill[1] as f32 * fa).round().clamp(0.0, 255.0) as u8,
        (fill[2] as f32 * fa).round().clamp(0.0, 255.0) as u8,
        fill[3],
    ];
    let interior_thresh = -(border_w + 1.0);

    for py in py0..py1 {
        let dy = ((py as f32 + 0.5) - cy).abs() - (hy - r);
        for px in px0..px1 {
            let dx = ((px as f32 + 0.5) - cx).abs() - (hx - r);
            let off = (py * tex_w + px) * 4;
            if dx < interior_thresh && dy < interior_thresh {
                if fill[3] == 255 || buf[off + 3] == 0 {
                    buf[off..off + 4].copy_from_slice(&fill_pm);
                } else {
                    blend_pixel(&mut buf[off..off + 4], fill, 1.0);
                }
                continue;
            }
            let qx = dx.max(0.0);
            let qy = dy.max(0.0);
            let dist = (qx * qx + qy * qy).sqrt() + dx.min(0.0).max(dy.min(0.0)) - r;

            let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
            if outer_cov <= 0.001 {
                continue;
            }
            if border_w > 0.0 {
                let inner_cov = (0.5 - (dist + border_w)).clamp(0.0, 1.0);
                let border_cov = (outer_cov - inner_cov).max(0.0);
                if border_cov > 0.001 {
                    blend_pixel(&mut buf[off..off + 4], border, border_cov);
                }
                if inner_cov > 0.001 {
                    blend_pixel(&mut buf[off..off + 4], fill, inner_cov);
                }
            } else {
                blend_pixel(&mut buf[off..off + 4], fill, outer_cov);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_ttf_centered(
    buf: &mut [u8],
    tex_w: usize,
    tex_h: usize,
    font: &FontRef<'_>,
    px_size: f32,
    center_x: f32,
    center_y: f32,
    text: &str,
    color: [u8; 4],
) {
    let scale = PxScale::from(px_size);
    let scaled = font.as_scaled(scale);

    let mut total_w = 0.0f32;
    let mut prev = None;
    for ch in text.chars() {
        let gid = scaled.glyph_id(ch);
        if let Some(p) = prev {
            total_w += scaled.kern(p, gid);
        }
        total_w += scaled.h_advance(gid);
        prev = Some(gid);
    }

    let mut pen_x = center_x - total_w * 0.5;
    let baseline_y = center_y + (scaled.ascent() + scaled.descent()) * 0.5;
    let mut prev_id = None;

    for ch in text.chars() {
        let gid = scaled.glyph_id(ch);
        if let Some(p) = prev_id {
            pen_x += scaled.kern(p, gid);
        }
        let glyph = gid.with_scale_and_position(scale, point(pen_x, baseline_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, cov| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px >= 0 && py >= 0 && (px as usize) < tex_w && (py as usize) < tex_h {
                    let off = (py as usize * tex_w + px as usize) * 4;
                    blend_pixel(&mut buf[off..off + 4], color, cov);
                }
            });
        }
        pen_x += scaled.h_advance(gid);
        prev_id = Some(gid);
    }
}

fn draw_reticle(
    buf: &mut [u8],
    tex_w: usize,
    tex_h: usize,
    cx: f32,
    cy: f32,
    core: [u8; 4],
) {
    let r_outer = 11.0f32;
    let px0 = ((cx - r_outer - 1.0).floor().max(0.0)) as usize;
    let py0 = ((cy - r_outer - 1.0).floor().max(0.0)) as usize;
    let px1 = ((cx + r_outer + 1.0).ceil().min(tex_w as f32)) as usize;
    let py1 = ((cy + r_outer + 1.0).ceil().min(tex_h as f32)) as usize;

    for py in py0..py1 {
        let dy = (py as f32 + 0.5) - cy;
        for px in px0..px1 {
            let dx = (px as f32 + 0.5) - cx;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > r_outer {
                continue;
            }
            let off = (py * tex_w + px) * 4;
            let halo = (1.0 - dist / r_outer).powf(2.0) * 0.55;
            if halo > 0.01 {
                blend_pixel(&mut buf[off..off + 4], core, halo);
            }
            let dot_cov = (5.0 - dist).clamp(0.0, 1.0);
            if dot_cov > 0.01 {
                blend_pixel(&mut buf[off..off + 4], [255, 255, 255, 255], dot_cov);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_layout_hit_and_render() {
        let mut kb = VirtualKeyboard::new();
        assert!(!kb.rects.is_empty());
        assert!(kb.hit_test(0.30, 0.86).is_some());
        kb.visible = true;
        kb.cursors[0] = Some((0.5, 0.5));
        let expected_len = (KB_TEX_WIDTH * KB_TEX_HEIGHT * 4) as usize;
        assert_eq!(kb.render_rgba().len(), expected_len);

        // Exercise incremental hover, press, cursor movement, and unhover fast paths
        let k0 = kb.hit_test(0.5, 0.5);
        assert!(k0.is_some());
        kb.hovered[0] = k0;
        kb.pressed[0] = k0;
        kb.cursors[0] = Some((0.52, 0.51));
        assert_eq!(kb.render_rgba().len(), expected_len);

        kb.pressed[0] = None;
        kb.hovered[0] = None;
        kb.cursors[0] = Some((0.25, 0.25));
        assert_eq!(kb.render_rgba().len(), expected_len);

        assert_eq!(android_keycode_to_evdev(66), Some(28));
    }

    #[test]
    fn hit_test_snaps_across_inter_key_gaps() {
        let mut kb = VirtualKeyboard::new();
        // Outside collapsed toolbar dock still returns None for click-through
        assert!(kb.hit_test(0.05, 0.20).is_none());

        kb.visible = true;
        // Pick two horizontally adjacent keys in row 0 and probe their exact midpoint gap
        let r0 = &kb.rects[1];
        let r1 = &kb.rects[2];
        assert!((r0.y0 - r1.y0).abs() < 1e-4);
        let gap_u = (r0.x1 + r1.x0) * 0.5;
        let mid_v = (r0.y0 + r0.y1) * 0.5;
        assert!(!(r0.x0..=r0.x1).contains(&gap_u));
        assert!(!(r1.x0..=r1.x1).contains(&gap_u));
        assert!(kb.hit_test(gap_u, mid_v).is_some());

        // Pick two vertically adjacent rows and probe their exact vertical gap
        let r_row1 = kb
            .rects
            .iter()
            .find(|r| r.y0 > r0.y1 && gap_u >= r.x0 && gap_u <= r.x1)
            .expect("row 1 key below gap");
        let gap_v = (r0.y1 + r_row1.y0) * 0.5;
        assert!(kb.hit_test((r_row1.x0 + r_row1.x1) * 0.5, gap_v).is_some());
    }

    #[test]
    fn kb_trigger_hair_trigger_and_rapid_double_tap() {
        let mut trig = KbTriggerState::new();
        assert!(!trig.update(0.10));
        // Hair-trigger engages above 0.32
        assert!(trig.update(0.35));
        // Stays engaged up to 0.85 and through minor 0.07 jitter (0.85 -> 0.78)
        assert!(trig.update(0.85));
        assert!(trig.update(0.78));
        // Rebounds by > 0.22 (0.85 -> 0.40) -> releases immediately even above 0.18
        assert!(!trig.update(0.40));
        // Continues descending -> stays released without spurious re-trigger
        assert!(!trig.update(0.36));
        // Rapid second squeeze (0.36 -> 0.55) -> re-engages immediately
        assert!(trig.update(0.55));
    }
}
