use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use glam::{Mat4, Quat, Vec3};
use std::borrow::Cow;

pub const KB_TEX_WIDTH: u32 = 2048;
pub const KB_TEX_HEIGHT: u32 = 704;
pub const KB_QUAD_WIDTH_M: f32 = 0.96;
pub const KB_QUAD_HEIGHT_M: f32 = 0.33;

const TOTAL_U_W: f32 = 24.0;
const TOTAL_U_H: f32 = 8.25;

static FONT_BYTES: &[u8] = include_bytes!("../assets/OpenSans-Bold.ttf");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMode {
    Hidden,
    Collapsed,
    Keyboard,
    Screens,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenUiEntry {
    pub stream_id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub hidden: bool,
    pub head_locked: bool,
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Key(u32),
    Modifier(u32),
    CapsLock,
    NumLock,
    ScrollLock,
    TogglePin,
    ToggleFollow,
    Recenter,
    ShowKeyboard,
    ToggleScreens,
    Collapse,
    ToggleScreenHidden(u32),
    ToggleScreenHeadlock(u32),
    BringScreen(u32),
    ShowAllScreens,
    UnlockAllHeadlocks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStyle {
    Standard,
    Modifier,
    Spacebar,
    Accent,
    Media,
    BezelPill,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyDef {
    pub label: &'static str,
    pub shift_label: &'static str,
    pub weight: f32,
    pub action: KeyAction,
    pub style: KeyStyle,
}

const fn act(label: &'static str, weight: f32, action: KeyAction) -> KeyDef {
    KeyDef {
        label,
        shift_label: label,
        weight,
        action,
        style: KeyStyle::Standard,
    }
}

const fn key_def(
    label: &'static str,
    shift_label: &'static str,
    action: KeyAction,
    style: KeyStyle,
) -> KeyDef {
    KeyDef {
        label,
        shift_label,
        weight: 1.0,
        action,
        style,
    }
}

#[derive(Debug, Clone)]
pub struct KeyRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub def: KeyDef,
}

fn push_u_rect(
    out: &mut Vec<KeyRect>,
    xu: f32,
    yu: f32,
    wu: f32,
    hu: f32,
    pad_u: f32,
    def: KeyDef,
) {
    out.push(KeyRect {
        x0: (xu + pad_u) / TOTAL_U_W,
        y0: (yu + pad_u) / TOTAL_U_H,
        x1: (xu + wu - pad_u) / TOTAL_U_W,
        y1: (yu + hu - pad_u) / TOTAL_U_H,
        def,
    });
}

fn build_full_ansi_rects() -> Vec<KeyRect> {
    let mut rects = Vec::with_capacity(112);
    let pad = 0.052;
    let main_x = 0.55;
    let nav_x = 16.00;
    let num_x = 19.45;
    let row_y0 = 2.55;
    let row_h = 1.04;

    // Top-left bezel pills
    push_u_rect(
        &mut rects,
        0.55,
        0.28,
        1.80,
        0.68,
        0.02,
        key_def(
            "Screens",
            "Screens",
            KeyAction::ToggleScreens,
            KeyStyle::BezelPill,
        ),
    );
    push_u_rect(
        &mut rects,
        2.50,
        0.28,
        1.80,
        0.68,
        0.02,
        key_def("Center", "Center", KeyAction::Recenter, KeyStyle::BezelPill),
    );

    // Top-right media bay directly below the Volume Roller: Hide, Pin, Follow
    let media_gap = 0.08;
    let media_w = (4.0 - media_gap * 2.0) / 3.0;
    for (i, (label, action)) in [
        ("Hide", KeyAction::Collapse),
        ("Pin", KeyAction::TogglePin),
        ("Follow", KeyAction::ToggleFollow),
    ]
    .into_iter()
    .enumerate()
    {
        let mx = num_x + (i as f32) * (media_w + media_gap);
        push_u_rect(
            &mut rects,
            mx,
            1.12,
            media_w,
            1.08,
            0.035,
            key_def(label, label, action, KeyStyle::Media),
        );
    }

    // Function Row (y = 1.20..2.20)
    let f_y = 1.20;
    let f_h = 1.00;
    push_u_rect(
        &mut rects,
        main_x,
        f_y,
        1.0,
        f_h,
        pad,
        key_def("Esc", "Esc", KeyAction::Key(1), KeyStyle::Modifier),
    );
    for (i, (lbl, code)) in [("F1", 59), ("F2", 60), ("F3", 61), ("F4", 62)]
        .into_iter()
        .enumerate()
    {
        push_u_rect(
            &mut rects,
            2.30 + i as f32,
            f_y,
            1.0,
            f_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Standard),
        );
    }
    for (i, (lbl, code)) in [("F5", 63), ("F6", 64), ("F7", 65), ("F8", 66)]
        .into_iter()
        .enumerate()
    {
        push_u_rect(
            &mut rects,
            6.925 + i as f32,
            f_y,
            1.0,
            f_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Modifier),
        );
    }
    for (i, (lbl, code)) in [("F9", 67), ("F10", 68), ("F11", 87), ("F12", 88)]
        .into_iter()
        .enumerate()
    {
        push_u_rect(
            &mut rects,
            11.55 + i as f32,
            f_y,
            1.0,
            f_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Standard),
        );
    }
    for (i, (lbl, action)) in [
        ("PrtSc", KeyAction::Key(99)),
        ("ScrLk", KeyAction::ScrollLock),
        ("Pause", KeyAction::Key(119)),
    ]
    .into_iter()
    .enumerate()
    {
        push_u_rect(
            &mut rects,
            nav_x + i as f32,
            f_y,
            1.0,
            f_h,
            pad,
            key_def(lbl, lbl, action, KeyStyle::Modifier),
        );
    }

    // Main Row 1: Number Row
    let y1 = row_y0;
    let row1: [(&str, &str, f32, KeyAction, KeyStyle); 14] = [
        ("`", "~", 1.0, KeyAction::Key(41), KeyStyle::Standard),
        ("1", "!", 1.0, KeyAction::Key(2), KeyStyle::Standard),
        ("2", "@", 1.0, KeyAction::Key(3), KeyStyle::Standard),
        ("3", "#", 1.0, KeyAction::Key(4), KeyStyle::Standard),
        ("4", "$", 1.0, KeyAction::Key(5), KeyStyle::Standard),
        ("5", "%", 1.0, KeyAction::Key(6), KeyStyle::Standard),
        ("6", "^", 1.0, KeyAction::Key(7), KeyStyle::Standard),
        ("7", "&", 1.0, KeyAction::Key(8), KeyStyle::Standard),
        ("8", "*", 1.0, KeyAction::Key(9), KeyStyle::Standard),
        ("9", "(", 1.0, KeyAction::Key(10), KeyStyle::Standard),
        ("0", ")", 1.0, KeyAction::Key(11), KeyStyle::Standard),
        ("-", "_", 1.0, KeyAction::Key(12), KeyStyle::Standard),
        ("=", "+", 1.0, KeyAction::Key(13), KeyStyle::Standard),
        ("Bksp", "Bksp", 2.0, KeyAction::Key(14), KeyStyle::Modifier),
    ];
    let mut cx = main_x;
    for (lbl, slbl, w_u, action, style) in row1 {
        push_u_rect(
            &mut rects,
            cx,
            y1,
            w_u,
            row_h,
            pad,
            key_def(lbl, slbl, action, style),
        );
        cx += w_u;
    }
    for (i, (lbl, code)) in [("Ins", 110), ("Home", 102), ("PgUp", 104)]
        .into_iter()
        .enumerate()
    {
        push_u_rect(
            &mut rects,
            nav_x + i as f32,
            y1,
            1.0,
            row_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Modifier),
        );
    }
    for (i, (lbl, action)) in [
        ("Num", KeyAction::NumLock),
        ("/", KeyAction::Key(98)),
        ("*", KeyAction::Key(55)),
        ("-", KeyAction::Key(74)),
    ]
    .into_iter()
    .enumerate()
    {
        push_u_rect(
            &mut rects,
            num_x + i as f32,
            y1,
            1.0,
            row_h,
            pad,
            key_def(lbl, lbl, action, KeyStyle::Modifier),
        );
    }

    // Main Row 2: QWERTY Row
    let y2 = row_y0 + row_h;
    let row2: [(&str, &str, f32, KeyAction, KeyStyle); 14] = [
        ("Tab", "Tab", 1.5, KeyAction::Key(15), KeyStyle::Modifier),
        ("q", "Q", 1.0, KeyAction::Key(16), KeyStyle::Standard),
        ("w", "W", 1.0, KeyAction::Key(17), KeyStyle::Accent),
        ("e", "E", 1.0, KeyAction::Key(18), KeyStyle::Standard),
        ("r", "R", 1.0, KeyAction::Key(19), KeyStyle::Standard),
        ("t", "T", 1.0, KeyAction::Key(20), KeyStyle::Standard),
        ("y", "Y", 1.0, KeyAction::Key(21), KeyStyle::Standard),
        ("u", "U", 1.0, KeyAction::Key(22), KeyStyle::Standard),
        ("i", "I", 1.0, KeyAction::Key(23), KeyStyle::Standard),
        ("o", "O", 1.0, KeyAction::Key(24), KeyStyle::Standard),
        ("p", "P", 1.0, KeyAction::Key(25), KeyStyle::Standard),
        ("[", "{", 1.0, KeyAction::Key(26), KeyStyle::Standard),
        ("]", "}", 1.0, KeyAction::Key(27), KeyStyle::Standard),
        ("\\", "|", 1.5, KeyAction::Key(43), KeyStyle::Standard),
    ];
    cx = main_x;
    for (lbl, slbl, w_u, action, style) in row2 {
        push_u_rect(
            &mut rects,
            cx,
            y2,
            w_u,
            row_h,
            pad,
            key_def(lbl, slbl, action, style),
        );
        cx += w_u;
    }
    for (i, (lbl, code)) in [("Del", 111), ("End", 107), ("PgDn", 109)]
        .into_iter()
        .enumerate()
    {
        push_u_rect(
            &mut rects,
            nav_x + i as f32,
            y2,
            1.0,
            row_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Modifier),
        );
    }
    for (i, (lbl, code)) in [("7", 71), ("8", 72), ("9", 73)].into_iter().enumerate() {
        push_u_rect(
            &mut rects,
            num_x + i as f32,
            y2,
            1.0,
            row_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Standard),
        );
    }
    // Tall NumPad '+' (spans Row 2 and Row 3)
    push_u_rect(
        &mut rects,
        num_x + 3.0,
        y2,
        1.0,
        row_h * 2.0,
        pad,
        key_def("+", "+", KeyAction::Key(78), KeyStyle::Modifier),
    );

    // Main Row 3: Home Row
    let y3 = row_y0 + row_h * 2.0;
    let row3: [(&str, &str, f32, KeyAction, KeyStyle); 13] = [
        (
            "Caps",
            "Caps",
            1.75,
            KeyAction::CapsLock,
            KeyStyle::Modifier,
        ),
        ("a", "A", 1.0, KeyAction::Key(30), KeyStyle::Accent),
        ("s", "S", 1.0, KeyAction::Key(31), KeyStyle::Accent),
        ("d", "D", 1.0, KeyAction::Key(32), KeyStyle::Accent),
        ("f", "F", 1.0, KeyAction::Key(33), KeyStyle::Standard),
        ("g", "G", 1.0, KeyAction::Key(34), KeyStyle::Standard),
        ("h", "H", 1.0, KeyAction::Key(35), KeyStyle::Standard),
        ("j", "J", 1.0, KeyAction::Key(36), KeyStyle::Standard),
        ("k", "K", 1.0, KeyAction::Key(37), KeyStyle::Standard),
        ("l", "L", 1.0, KeyAction::Key(38), KeyStyle::Standard),
        (";", ":", 1.0, KeyAction::Key(39), KeyStyle::Standard),
        ("'", "\"", 1.0, KeyAction::Key(40), KeyStyle::Standard),
        (
            "Enter",
            "Enter",
            2.25,
            KeyAction::Key(28),
            KeyStyle::Modifier,
        ),
    ];
    cx = main_x;
    for (lbl, slbl, w_u, action, style) in row3 {
        push_u_rect(
            &mut rects,
            cx,
            y3,
            w_u,
            row_h,
            pad,
            key_def(lbl, slbl, action, style),
        );
        cx += w_u;
    }
    for (i, (lbl, code)) in [("4", 75), ("5", 76), ("6", 77)].into_iter().enumerate() {
        push_u_rect(
            &mut rects,
            num_x + i as f32,
            y3,
            1.0,
            row_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Standard),
        );
    }

    // Main Row 4: Shift Row
    let y4 = row_y0 + row_h * 3.0;
    let row4: [(&str, &str, f32, KeyAction, KeyStyle); 12] = [
        (
            "Shift",
            "Shift",
            2.25,
            KeyAction::Modifier(42),
            KeyStyle::Modifier,
        ),
        ("z", "Z", 1.0, KeyAction::Key(44), KeyStyle::Standard),
        ("x", "X", 1.0, KeyAction::Key(45), KeyStyle::Standard),
        ("c", "C", 1.0, KeyAction::Key(46), KeyStyle::Standard),
        ("v", "V", 1.0, KeyAction::Key(47), KeyStyle::Standard),
        ("b", "B", 1.0, KeyAction::Key(48), KeyStyle::Standard),
        ("n", "N", 1.0, KeyAction::Key(49), KeyStyle::Standard),
        ("m", "M", 1.0, KeyAction::Key(50), KeyStyle::Standard),
        (",", "<", 1.0, KeyAction::Key(51), KeyStyle::Standard),
        (".", ">", 1.0, KeyAction::Key(52), KeyStyle::Standard),
        ("/", "?", 1.0, KeyAction::Key(53), KeyStyle::Standard),
        (
            "Shift",
            "Shift",
            2.75,
            KeyAction::Modifier(54),
            KeyStyle::Modifier,
        ),
    ];
    cx = main_x;
    for (lbl, slbl, w_u, action, style) in row4 {
        push_u_rect(
            &mut rects,
            cx,
            y4,
            w_u,
            row_h,
            pad,
            key_def(lbl, slbl, action, style),
        );
        cx += w_u;
    }
    push_u_rect(
        &mut rects,
        nav_x + 1.0,
        y4,
        1.0,
        row_h,
        pad,
        key_def("Up", "Up", KeyAction::Key(103), KeyStyle::Accent),
    );
    for (i, (lbl, code)) in [("1", 79), ("2", 80), ("3", 81)].into_iter().enumerate() {
        push_u_rect(
            &mut rects,
            num_x + i as f32,
            y4,
            1.0,
            row_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Standard),
        );
    }
    // Tall NumPad Enter (spans Row 4 and Row 5)
    push_u_rect(
        &mut rects,
        num_x + 3.0,
        y4,
        1.0,
        row_h * 2.0,
        pad,
        key_def("Ent", "Ent", KeyAction::Key(96), KeyStyle::Modifier),
    );

    // Main Row 5: 6.5u bottom row
    let y5 = row_y0 + row_h * 4.0;
    let row5: [(&str, f32, KeyAction, KeyStyle); 8] = [
        ("Ctrl", 1.5, KeyAction::Modifier(29), KeyStyle::Modifier),
        ("Win", 1.0, KeyAction::Modifier(125), KeyStyle::Modifier),
        ("Alt", 1.25, KeyAction::Modifier(56), KeyStyle::Modifier),
        ("Space", 6.5, KeyAction::Key(57), KeyStyle::Spacebar),
        ("Alt", 1.25, KeyAction::Modifier(100), KeyStyle::Modifier),
        ("Win", 1.0, KeyAction::Modifier(126), KeyStyle::Modifier),
        ("Menu", 1.0, KeyAction::Key(127), KeyStyle::Modifier),
        ("Ctrl", 1.5, KeyAction::Modifier(97), KeyStyle::Modifier),
    ];
    cx = main_x;
    for (lbl, w_u, action, style) in row5 {
        push_u_rect(
            &mut rects,
            cx,
            y5,
            w_u,
            row_h,
            pad,
            key_def(lbl, lbl, action, style),
        );
        cx += w_u;
    }
    for (i, (lbl, code)) in [("Left", 105), ("Down", 108), ("Right", 106)]
        .into_iter()
        .enumerate()
    {
        push_u_rect(
            &mut rects,
            nav_x + i as f32,
            y5,
            1.0,
            row_h,
            pad,
            key_def(lbl, lbl, KeyAction::Key(code), KeyStyle::Accent),
        );
    }
    push_u_rect(
        &mut rects,
        num_x,
        y5,
        2.0,
        row_h,
        pad,
        key_def("0", "0", KeyAction::Key(82), KeyStyle::Standard),
    );
    push_u_rect(
        &mut rects,
        num_x + 2.0,
        y5,
        1.0,
        row_h,
        pad,
        key_def(".", ".", KeyAction::Key(83), KeyStyle::Standard),
    );

    rects
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KbStaticState {
    mode: PanelMode,
    shift: bool,
    rshift: bool,
    caps: bool,
    num_lock: bool,
    scroll_lock: bool,
    ctrl: bool,
    rctrl: bool,
    alt: bool,
    ralt: bool,
    super_key: bool,
    rsuper_key: bool,
    monitors_pinned: bool,
    following: bool,
    screens: Vec<ScreenUiEntry>,
}

pub struct VirtualKeyboard {
    pub mode: PanelMode,
    pub world_from_quad: Mat4,
    pub hud_view_offset: Mat4,
    pub head_lock: Option<Mat4>,
    pub rects: Vec<KeyRect>,
    pub toolbar_rects: Vec<KeyRect>,
    pub screen_rects: Vec<KeyRect>,
    pub screens: Vec<ScreenUiEntry>,
    pub cursors: [Option<(f32, f32)>; 2],
    pub hovered: [Option<usize>; 2],
    pub pressed: [Option<usize>; 2],
    pub shift: bool,
    pub rshift: bool,
    pub ctrl: bool,
    pub rctrl: bool,
    pub alt: bool,
    pub ralt: bool,
    pub super_key: bool,
    pub rsuper_key: bool,
    pub caps: bool,
    pub num_lock: bool,
    pub scroll_lock: bool,
    pub monitors_pinned: bool,
    pub dirty: bool,
    bg_layer: Vec<u8>,
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
        let mut kb = Self {
            mode: PanelMode::Collapsed,
            world_from_quad: Self::default_pose_from_head(Vec3::ZERO, Quat::IDENTITY),
            hud_view_offset: Self::default_view_pose(),
            head_lock: None,
            rects: build_full_ansi_rects(),
            toolbar_rects: Vec::new(),
            screen_rects: Vec::new(),
            screens: Vec::new(),
            cursors: [None, None],
            hovered: [None, None],
            pressed: [None, None],
            shift: false,
            rshift: false,
            ctrl: false,
            rctrl: false,
            alt: false,
            ralt: false,
            super_key: false,
            rsuper_key: false,
            caps: false,
            num_lock: true,
            scroll_lock: false,
            monitors_pinned: false,
            dirty: true,
            bg_layer: Vec::new(),
            base_layer: Vec::new(),
            base_state: None,
            frame_buf: Vec::new(),
            prev_active_keys: [None; 4],
            prev_cursors: [None; 2],
        };
        kb.rebuild_toolbar_rects();
        kb.rebuild_screen_rects();
        kb
    }

    pub fn is_visible(&self) -> bool {
        self.mode != PanelMode::Hidden
    }

    pub fn is_hud_panel(&self) -> bool {
        matches!(self.mode, PanelMode::Collapsed | PanelMode::Screens)
    }

    pub fn is_expanded(&self) -> bool {
        matches!(self.mode, PanelMode::Keyboard | PanelMode::Screens)
    }

    pub fn is_following(&self) -> bool {
        self.head_lock.is_some()
    }

    pub fn active_view_offset(&self) -> Option<Mat4> {
        match self.mode {
            PanelMode::Hidden => None,
            PanelMode::Collapsed | PanelMode::Screens => Some(self.hud_view_offset),
            PanelMode::Keyboard => self.head_lock,
        }
    }

    pub fn any_shift(&self) -> bool {
        self.shift || self.rshift
    }

    pub fn active_rects(&self) -> &[KeyRect] {
        match self.mode {
            PanelMode::Hidden => &[],
            PanelMode::Collapsed => &self.toolbar_rects,
            PanelMode::Keyboard => &self.rects,
            PanelMode::Screens => &self.screen_rects,
        }
    }

    pub fn sync_screens(&mut self, entries: Vec<ScreenUiEntry>) {
        if self.screens == entries {
            return;
        }
        self.screens = entries;
        self.rebuild_toolbar_rects();
        self.rebuild_screen_rects();
        self.hovered = [None, None];
        self.pressed = [None, None];
        self.dirty = true;
    }

    fn rebuild_toolbar_rects(&mut self) {
        self.toolbar_rects.clear();
        if !self.screens.is_empty() {
            let y0 = 0.56;
            let y1 = 0.73;
            if self.screens.len() == 1 {
                self.toolbar_rects.push(KeyRect {
                    x0: 0.36,
                    y0,
                    x1: 0.64,
                    y1,
                    def: act(
                        "Screen 1",
                        1.0,
                        KeyAction::ToggleScreenHidden(self.screens[0].stream_id),
                    ),
                });
            } else {
                let start_x = 0.16;
                let end_x = 0.84;
                let gap_x = 0.012;
                let n = self.screens.len();
                let avail_w = (end_x - start_x) - gap_x * (n.saturating_sub(1) as f32);
                let btn_w = avail_w / (n as f32);
                for (i, entry) in self.screens.iter().enumerate() {
                    let x0 = start_x + (i as f32) * (btn_w + gap_x);
                    self.toolbar_rects.push(KeyRect {
                        x0,
                        y0,
                        x1: x0 + btn_w,
                        y1,
                        def: act(
                            "Screen",
                            1.0,
                            KeyAction::ToggleScreenHidden(entry.stream_id),
                        ),
                    });
                }
            }
        }

        let row2_y0 = 0.765;
        let row2_y1 = 0.955;
        self.toolbar_rects.extend([
            KeyRect {
                x0: 0.16,
                y0: row2_y0,
                x1: 0.35,
                y1: row2_y1,
                def: act("Keyboard", 1.0, KeyAction::ShowKeyboard),
            },
            KeyRect {
                x0: 0.365,
                y0: row2_y0,
                x1: 0.535,
                y1: row2_y1,
                def: act("Screens", 1.0, KeyAction::ToggleScreens),
            },
            KeyRect {
                x0: 0.55,
                y0: row2_y0,
                x1: 0.685,
                y1: row2_y1,
                def: act("Pin", 1.0, KeyAction::TogglePin),
            },
            KeyRect {
                x0: 0.70,
                y0: row2_y0,
                x1: 0.84,
                y1: row2_y1,
                def: act("Center", 1.0, KeyAction::Recenter),
            },
        ]);
    }

    fn rebuild_screen_rects(&mut self) {
        self.screen_rects.clear();
        self.screen_rects.push(KeyRect {
            x0: 0.85,
            y0: 0.035,
            x1: 0.97,
            y1: 0.145,
            def: act("Close", 1.0, KeyAction::Collapse),
        });

        if !self.screens.is_empty() {
            let area_top = 0.175;
            let area_bottom = 0.745;
            let row_h = ((area_bottom - area_top) / (self.screens.len() as f32)).min(0.155);
            let gap_y = (row_h * 0.14).min(0.016);
            let btn_h = (row_h - gap_y).max(0.02);

            for (i, entry) in self.screens.iter().enumerate() {
                let y0 = area_top + (i as f32) * row_h + gap_y * 0.5;
                let y1 = (y0 + btn_h).min(area_bottom);
                self.screen_rects.push(KeyRect {
                    x0: 0.40,
                    y0,
                    x1: 0.58,
                    y1,
                    def: act("Hide", 1.0, KeyAction::ToggleScreenHidden(entry.stream_id)),
                });
                self.screen_rects.push(KeyRect {
                    x0: 0.60,
                    y0,
                    x1: 0.80,
                    y1,
                    def: act(
                        "Headlock",
                        1.0,
                        KeyAction::ToggleScreenHeadlock(entry.stream_id),
                    ),
                });
                self.screen_rects.push(KeyRect {
                    x0: 0.82,
                    y0,
                    x1: 0.97,
                    y1,
                    def: act("Bring", 1.0, KeyAction::BringScreen(entry.stream_id)),
                });
            }
        }

        let bot_y0 = 0.79;
        let bot_y1 = 0.955;
        self.screen_rects.extend([
            KeyRect {
                x0: 0.03,
                y0: bot_y0,
                x1: 0.24,
                y1: bot_y1,
                def: act("Keyboard", 1.0, KeyAction::ShowKeyboard),
            },
            KeyRect {
                x0: 0.26,
                y0: bot_y0,
                x1: 0.47,
                y1: bot_y1,
                def: act("Show All", 1.0, KeyAction::ShowAllScreens),
            },
            KeyRect {
                x0: 0.49,
                y0: bot_y0,
                x1: 0.72,
                y1: bot_y1,
                def: act("Unlock All", 1.0, KeyAction::UnlockAllHeadlocks),
            },
            KeyRect {
                x0: 0.74,
                y0: bot_y0,
                x1: 0.97,
                y1: bot_y1,
                def: act("Center", 1.0, KeyAction::Recenter),
            },
        ]);
    }

    pub fn default_view_pose() -> Mat4 {
        let pos = Vec3::new(0.0, -0.36, -0.95);
        let tilt = Quat::from_rotation_x(-25.0_f32.to_radians());
        Mat4::from_rotation_translation(tilt, pos)
    }

    pub fn default_pose_from_head(head_pos: Vec3, head_rot: Quat) -> Mat4 {
        let fwd = head_rot * Vec3::NEG_Z;
        let yaw = (-fwd.x).atan2(-fwd.z);
        let yaw_rot = Quat::from_rotation_y(yaw);
        let pos = head_pos + yaw_rot * Vec3::new(0.0, -0.36, -0.95);
        let tilt = Quat::from_rotation_x(-25.0_f32.to_radians());
        Mat4::from_rotation_translation(yaw_rot * tilt, pos)
    }

    pub fn hit_test(&self, u: f32, v: f32) -> Option<usize> {
        if self.mode == PanelMode::Hidden {
            return None;
        }
        if let Some(exact) = self
            .active_rects()
            .iter()
            .position(|r| u >= r.x0 && u <= r.x1 && v >= r.y0 && v <= r.y1)
        {
            return Some(exact);
        }
        if self.mode != PanelMode::Keyboard {
            return None;
        }
        self.rects
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| {
                let dx = (r.x0 - u).max(0.0).max(u - r.x1);
                let dy = (r.y0 - v).max(0.0).max(v - r.y1);
                if dx <= 0.006 && dy <= 0.016 {
                    let dist_px = (dx * KB_TEX_WIDTH as f32).hypot(dy * KB_TEX_HEIGHT as f32);
                    Some((idx, dist_px))
                } else {
                    None
                }
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(idx, _)| idx)
    }

    fn static_state(&self) -> KbStaticState {
        KbStaticState {
            mode: self.mode,
            shift: self.shift,
            rshift: self.rshift,
            caps: self.caps,
            num_lock: self.num_lock,
            scroll_lock: self.scroll_lock,
            ctrl: self.ctrl,
            rctrl: self.rctrl,
            alt: self.alt,
            ralt: self.ralt,
            super_key: self.super_key,
            rsuper_key: self.rsuper_key,
            monitors_pinned: self.monitors_pinned,
            following: self.is_following(),
            screens: self.screens.clone(),
        }
    }

    pub fn is_modifier_active(&self, action: KeyAction) -> bool {
        action_is_active(action, &self.static_state())
    }

    pub fn render_rgba(&mut self) -> &[u8] {
        let w = KB_TEX_WIDTH as usize;
        let h = KB_TEX_HEIGHT as usize;
        let expected_len = w * h * 4;
        let any_shift = self.any_shift();
        let upper = any_shift ^ self.caps;
        let following = self.is_following();
        let static_key = self.static_state();
        let font = FontRef::try_from_slice(FONT_BYTES).ok();

        if self.base_state.as_ref() != Some(&static_key) || self.base_layer.len() != expected_len {
            self.bg_layer.resize(expected_len, 0);
            self.bg_layer.fill(0);

            match self.mode {
                PanelMode::Hidden => {}
                PanelMode::Keyboard => {
                    draw_keyboard_chassis(
                        &mut self.bg_layer,
                        w,
                        h,
                        font.as_ref(),
                        self.num_lock,
                        self.caps,
                        self.scroll_lock,
                    );
                }
                PanelMode::Screens => {
                    draw_rounded_rect_sdf(
                        &mut self.bg_layer,
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
                    draw_screens_panel_chrome(
                        &mut self.bg_layer,
                        w,
                        h,
                        font.as_ref(),
                        &self.screens,
                    );
                }
                PanelMode::Collapsed => {
                    let pill_y0 = if self.screens.is_empty() { 0.735 } else { 0.53 };
                    draw_rounded_rect_sdf(
                        &mut self.bg_layer,
                        w,
                        h,
                        0.14 * w as f32,
                        pill_y0 * h as f32,
                        0.86 * w as f32,
                        0.985 * h as f32,
                        22.0,
                        [16, 20, 28, 224],
                        [64, 82, 112, 230],
                        2.0,
                    );
                }
            }

            if self.base_layer.len() != expected_len {
                self.base_layer.resize(expected_len, 0);
            }
            self.base_layer.copy_from_slice(&self.bg_layer);

            let rects = match self.mode {
                PanelMode::Hidden => &[][..],
                PanelMode::Collapsed => &self.toolbar_rects,
                PanelMode::Keyboard => &self.rects,
                PanelMode::Screens => &self.screen_rects,
            };
            for r in rects {
                let is_active = action_is_active(r.def.action, &static_key);
                draw_panel_button(
                    &mut self.base_layer,
                    w,
                    h,
                    font.as_ref(),
                    self.mode,
                    upper,
                    any_shift,
                    self.monitors_pinned,
                    following,
                    &self.screens,
                    r,
                    false,
                    false,
                    is_active,
                );
            }

            self.base_state = Some(static_key.clone());
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
            let rects = match self.mode {
                PanelMode::Hidden => &[][..],
                PanelMode::Collapsed => &self.toolbar_rects,
                PanelMode::Keyboard => &self.rects,
                PanelMode::Screens => &self.screen_rects,
            };
            for kidx in self.prev_active_keys.into_iter().flatten() {
                if let Some(r) = rects.get(kidx) {
                    let (x0, y0, x1, y1) = key_bounds_px(r, w, h);
                    copy_rect(&mut self.frame_buf, &self.base_layer, w, h, x0, y0, x1, y1);
                }
            }
        }

        let cur_active = [
            self.hovered[0],
            self.hovered[1],
            self.pressed[0],
            self.pressed[1],
        ];
        let mut drawn = [None::<usize>; 4];
        let mut drawn_len = 0usize;
        let rects = match self.mode {
            PanelMode::Hidden => &[][..],
            PanelMode::Collapsed => &self.toolbar_rects,
            PanelMode::Keyboard => &self.rects,
            PanelMode::Screens => &self.screen_rects,
        };
        for kidx in cur_active.into_iter().flatten() {
            if drawn[..drawn_len].contains(&Some(kidx)) {
                continue;
            }
            drawn[drawn_len] = Some(kidx);
            drawn_len += 1;

            if let Some(r) = rects.get(kidx) {
                let (x0, y0, x1, y1) = key_bounds_px(r, w, h);
                copy_rect(&mut self.frame_buf, &self.bg_layer, w, h, x0, y0, x1, y1);
                let is_hovered = self.hovered.contains(&Some(kidx));
                let is_pressed = self.pressed.contains(&Some(kidx));
                let is_active = action_is_active(r.def.action, &static_key);
                draw_panel_button(
                    &mut self.frame_buf,
                    w,
                    h,
                    font.as_ref(),
                    self.mode,
                    upper,
                    any_shift,
                    self.monitors_pinned,
                    following,
                    &self.screens,
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

fn draw_screens_panel_chrome(
    buf: &mut [u8],
    w: usize,
    h: usize,
    font: Option<&FontRef<'_>>,
    screens: &[ScreenUiEntry],
) {
    if let Some(f) = font {
        draw_ttf_aligned(
            buf,
            w,
            h,
            f,
            30.0,
            0.04 * w as f32,
            0.09 * h as f32,
            "Screens",
            [244, 247, 252, 255],
            false,
        );
        draw_ttf_centered(
            buf,
            w,
            h,
            f,
            19.0,
            0.50 * w as f32,
            0.09 * h as f32,
            "Grab + A: Headlock   |   Grab + B: Hide",
            [160, 178, 204, 255],
        );
    }

    if screens.is_empty() {
        if let Some(f) = font {
            draw_ttf_centered(
                buf,
                w,
                h,
                f,
                26.0,
                0.50 * w as f32,
                0.46 * h as f32,
                "Waiting for host screens...",
                [176, 192, 214, 255],
            );
        }
        return;
    }

    let area_top = 0.175;
    let area_bottom = 0.745;
    let row_h = ((area_bottom - area_top) / (screens.len() as f32)).min(0.155);
    let gap_y = (row_h * 0.14).min(0.016);
    let btn_h = (row_h - gap_y).max(0.02);

    for (i, entry) in screens.iter().enumerate() {
        let y0 = area_top + (i as f32) * row_h + gap_y * 0.5;
        let y1 = (y0 + btn_h).min(area_bottom);
        let cx0 = 0.03 * w as f32;
        let cx1 = 0.38 * w as f32;
        let cy0 = y0 * h as f32;
        let cy1 = y1 * h as f32;

        let (fill, border, text_col) = if entry.hidden {
            ([26, 26, 36, 235], [74, 62, 74, 255], [168, 160, 174, 255])
        } else if entry.head_locked {
            ([22, 46, 56, 240], [56, 178, 172, 255], [232, 252, 250, 255])
        } else {
            ([26, 34, 48, 240], [62, 78, 104, 255], [236, 242, 250, 255])
        };
        draw_rounded_rect_sdf(buf, w, h, cx0, cy0, cx1, cy1, 12.0, fill, border, 1.5);

        if let Some(f) = font {
            let label = if entry.name.is_empty() {
                format!("Screen {}  {}x{}", i + 1, entry.width, entry.height)
            } else {
                format!("{}  {}x{}", entry.name, entry.width, entry.height)
            };
            let px_size = ((cx1 - cx0 - 24.0).max(40.0)
                / (label.chars().count().max(1) as f32 * 0.56))
                .clamp(14.0, 22.0);
            draw_ttf_aligned(
                buf,
                w,
                h,
                f,
                px_size,
                cx0 + 18.0,
                (cy0 + cy1) * 0.5,
                &label,
                text_col,
                false,
            );
        }
    }
}

fn rgb_spectrum(t: f32) -> [u8; 3] {
    let phase = t.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (phase % 2.0 - 1.0).abs();
    let (r, g, b) = match phase as usize {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    [
        (45.0 + r * 205.0).round() as u8,
        (45.0 + g * 205.0).round() as u8,
        (45.0 + b * 205.0).round() as u8,
    ]
}

fn draw_keyboard_chassis(
    buf: &mut [u8],
    w: usize,
    h: usize,
    font: Option<&FontRef<'_>>,
    num_lock: bool,
    caps: bool,
    scroll_lock: bool,
) {
    let ux = w as f32 / TOTAL_U_W;
    let uy = h as f32 / TOTAL_U_H;

    // Outer anodized dark gunmetal chassis
    draw_rounded_rect_sdf(
        buf,
        w,
        h,
        2.0,
        2.0,
        (w - 2) as f32,
        (h - 2) as f32,
        18.0,
        [28, 32, 39, 246],
        [92, 102, 118, 255],
        2.5,
    );

    // Brushed aluminum horizontal texture & subtle metallic sheen across chassis interior
    for py in 6..(h - 6) {
        let y_norm = py as f32 / h as f32;
        let grad = ((0.5 - y_norm) * 14.0).round() as i32;
        let brush = match py % 4 {
            0 => 2,
            2 => -2,
            _ => 0,
        };
        let delta = grad + brush;
        let row_start = (py * w + 8) * 4;
        let row_end = (py * w + (w - 8)) * 4;
        for px in buf[row_start..row_end].as_chunks_mut::<4>().0 {
            if px[3] > 235 {
                px[0] = (px[0] as i32 + delta).clamp(16, 68) as u8;
                px[1] = (px[1] as i32 + delta).clamp(18, 72) as u8;
                px[2] = (px[2] as i32 + delta).clamp(22, 82) as u8;
            }
        }
    }

    // Top RGB LightEdge bar + soft downward glow onto top bezel
    let edge_x0 = 22usize;
    let edge_x1 = w - 22;
    let span = (edge_x1 - edge_x0).max(1) as f32;
    for py in 4..26 {
        let cov = if py < 11 {
            1.0
        } else {
            let d = (py - 10) as f32 / 16.0;
            (1.0 - d).powf(2.0) * 0.28
        };
        for px in edge_x0..edge_x1 {
            let t = (px - edge_x0) as f32 / span;
            let [r, g, b] = rgb_spectrum(t);
            let off = (py * w + px) * 4;
            blend_pixel(&mut buf[off..off + 4], [r, g, b, 255], cov);
        }
    }

    // Subtle engraved VirtualRustTop branding centered in the top bezel
    if let Some(f) = font {
        draw_ttf_centered(
            buf,
            w,
            h,
            f,
            19.0,
            10.0 * ux,
            0.62 * uy,
            "VIRTUALRUSTTOP   ANSI-104",
            [148, 160, 180, 215],
        );
    }

    // Status LED strip above Nav cluster (NUM, CAPS, SCR)
    let led_x0 = 16.00 * ux;
    let led_x1 = 19.00 * ux;
    let led_y0 = 0.34 * uy;
    let led_y1 = 0.90 * uy;
    draw_rounded_rect_sdf(
        buf,
        w,
        h,
        led_x0,
        led_y0,
        led_x1,
        led_y1,
        8.0,
        [12, 15, 20, 252],
        [54, 62, 76, 255],
        1.5,
    );
    let led_cy = 0.54 * uy;
    let lbl_cy = 0.77 * uy;
    for (i, (lbl, active)) in [("NUM", num_lock), ("CAPS", caps), ("SCR", scroll_lock)]
        .into_iter()
        .enumerate()
    {
        let lx = (16.50 + i as f32) * ux;
        let led_col = if active {
            [110, 236, 255, 255]
        } else {
            [44, 52, 66, 255]
        };
        let r_outer = if active { 11.0 } else { 5.0 };
        let px0 = ((lx - r_outer).floor().max(0.0)) as usize;
        let py0 = ((led_cy - r_outer).floor().max(0.0)) as usize;
        let px1 = ((lx + r_outer).ceil().min(w as f32)) as usize;
        let py1 = ((led_cy + r_outer).ceil().min(h as f32)) as usize;
        for py in py0..py1 {
            let dy = py as f32 + 0.5 - led_cy;
            for px in px0..px1 {
                let dx = px as f32 + 0.5 - lx;
                let dist = (dx * dx + dy * dy).sqrt();
                let off = (py * w + px) * 4;
                if active && dist < r_outer {
                    let glow = (1.0 - dist / r_outer).powf(2.0) * 0.45;
                    blend_pixel(&mut buf[off..off + 4], led_col, glow);
                }
                let dot = (4.2 - dist).clamp(0.0, 1.0);
                if dot > 0.01 {
                    blend_pixel(&mut buf[off..off + 4], led_col, dot);
                }
            }
        }
        if let Some(f) = font {
            let tcol = if active {
                [210, 244, 255, 255]
            } else {
                [108, 120, 138, 220]
            };
            draw_ttf_centered(buf, w, h, f, 12.0, lx, lbl_cy, lbl, tcol);
        }
    }

    // Recessed Media Controls Bay housing + knurled aluminum Volume Roller graphic
    draw_rounded_rect_sdf(
        buf,
        w,
        h,
        19.35 * ux,
        0.20 * uy,
        23.55 * ux,
        2.30 * uy,
        10.0,
        [14, 17, 23, 252],
        [62, 72, 88, 255],
        1.5,
    );
    let vr_x0 = 19.52 * ux;
    let vr_y0 = 0.28 * uy;
    let vr_x1 = 23.38 * ux;
    let vr_y1 = 0.88 * uy;
    draw_rounded_rect_sdf(
        buf,
        w,
        h,
        vr_x0,
        vr_y0,
        vr_x1,
        vr_y1,
        8.0,
        [52, 58, 68, 255],
        [110, 122, 138, 255],
        1.5,
    );
    let vx0 = (vr_x0 as usize + 6).min(w);
    let vx1 = (vr_x1 as usize).saturating_sub(6).min(w);
    let vy0 = (vr_y0 as usize + 3).min(h);
    let vy1 = (vr_y1 as usize).saturating_sub(3).min(h);
    let vh = (vy1.saturating_sub(vy0)).max(1) as f32;
    for py in vy0..vy1 {
        let v_rel = (py - vy0) as f32 / vh;
        let cyl_shade = ((1.0 - (v_rel - 0.35).abs() * 2.1).max(0.0) * 42.0).round() as i32;
        for px in vx0..vx1 {
            let knurl = if (px % 8) < 4 { 14 } else { -12 };
            let off = (py * w + px) * 4;
            let base = 46 + cyl_shade + knurl;
            buf[off] = base.clamp(22, 132) as u8;
            buf[off + 1] = (base + 4).clamp(24, 138) as u8;
            buf[off + 2] = (base + 10).clamp(28, 148) as u8;
            buf[off + 3] = 255;
        }
    }
    if let Some(f) = font {
        draw_ttf_centered(
            buf,
            w,
            h,
            f,
            13.0,
            21.45 * ux,
            0.58 * uy,
            "VOLUME",
            [205, 218, 236, 215],
        );
    }

    // Floating recessed dark key wells for Main, F-Row, Nav, and NumPad
    for (wx0, wy0, wx1, wy1) in [
        (0.43, 1.08, 15.67, 2.32),
        (0.43, 2.43, 15.67, 7.87),
        (15.88, 1.08, 19.12, 2.32),
        (15.88, 2.43, 19.12, 7.87),
        (19.33, 2.43, 23.57, 7.87),
    ] {
        draw_rounded_rect_sdf(
            buf,
            w,
            h,
            wx0 * ux,
            wy0 * uy,
            wx1 * ux,
            wy1 * uy,
            10.0,
            [9, 11, 15, 252],
            [42, 48, 58, 255],
            1.5,
        );
    }
}

fn action_is_active(action: KeyAction, kb: &KbStaticState) -> bool {
    match action {
        KeyAction::Modifier(42) => kb.shift,
        KeyAction::Modifier(54) => kb.rshift,
        KeyAction::Modifier(29) => kb.ctrl,
        KeyAction::Modifier(97) => kb.rctrl,
        KeyAction::Modifier(56) => kb.alt,
        KeyAction::Modifier(100) => kb.ralt,
        KeyAction::Modifier(125) => kb.super_key,
        KeyAction::Modifier(126) => kb.rsuper_key,
        KeyAction::CapsLock => kb.caps,
        KeyAction::NumLock => kb.num_lock,
        KeyAction::ScrollLock => kb.scroll_lock,
        KeyAction::TogglePin => kb.monitors_pinned,
        KeyAction::ToggleFollow => kb.following,
        KeyAction::ToggleScreenHidden(id) => kb
            .screens
            .iter()
            .find(|s| s.stream_id == id)
            .is_some_and(|s| s.hidden),
        KeyAction::ToggleScreenHeadlock(id) => kb
            .screens
            .iter()
            .find(|s| s.stream_id == id)
            .is_some_and(|s| s.head_locked),
        KeyAction::ToggleScreens => kb.screens.iter().any(|s| s.hidden || s.head_locked),
        KeyAction::ShowAllScreens => kb.screens.iter().any(|s| s.hidden),
        KeyAction::UnlockAllHeadlocks => kb.screens.iter().any(|s| s.head_locked),
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

fn key_bounds_px(r: &KeyRect, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let x0 = ((r.x0 * w as f32) - 5.0).floor().max(0.0) as usize;
    let y0 = ((r.y0 * h as f32) - 5.0).floor().max(0.0) as usize;
    let x1 = ((r.x1 * w as f32) + 5.0).ceil().min(w as f32) as usize;
    let y1 = ((r.y1 * h as f32) + 8.0).ceil().min(h as f32) as usize;
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
fn resolve_key_visual<'a>(
    r: &'a KeyRect,
    mode: PanelMode,
    upper: bool,
    shift: bool,
    monitors_pinned: bool,
    following: bool,
    screens: &[ScreenUiEntry],
    is_hovered: bool,
    is_pressed: bool,
    is_active: bool,
) -> (Cow<'a, str>, [u8; 4], [u8; 4], [u8; 4]) {
    let label: Cow<'a, str> = match r.def.action {
        KeyAction::TogglePin if monitors_pinned => Cow::Borrowed("Pinned"),
        KeyAction::ToggleFollow if following => Cow::Borrowed("Following"),
        KeyAction::ToggleScreenHidden(id) => {
            if let Some((idx, entry)) = screens.iter().enumerate().find(|(_, s)| s.stream_id == id)
            {
                if mode == PanelMode::Collapsed {
                    let prefix = if screens.len() > 4 {
                        format!("S{}", idx + 1)
                    } else {
                        format!("Screen {}", idx + 1)
                    };
                    if entry.hidden {
                        Cow::Owned(format!("{} (Off)", prefix))
                    } else if entry.head_locked {
                        Cow::Owned(format!("{} (HL)", prefix))
                    } else {
                        Cow::Owned(prefix)
                    }
                } else if entry.hidden {
                    Cow::Borrowed("Hidden")
                } else {
                    Cow::Borrowed("Hide")
                }
            } else {
                Cow::Borrowed(r.def.label)
            }
        }
        KeyAction::ToggleScreenHeadlock(id) => {
            if screens
                .iter()
                .find(|s| s.stream_id == id)
                .is_some_and(|s| s.head_locked)
            {
                Cow::Borrowed("Headlocked")
            } else {
                Cow::Borrowed("Headlock")
            }
        }
        _ if shift
            || (upper
                && r.def.label.len() == 1
                && r.def.label.chars().all(|c| c.is_ascii_alphabetic())) =>
        {
            Cow::Borrowed(r.def.shift_label)
        }
        _ => Cow::Borrowed(r.def.label),
    };

    let (bg, border, text_col) = if is_pressed {
        (
            [59, 130, 246, 255],
            [191, 219, 254, 255],
            [255, 255, 255, 255],
        )
    } else if is_active {
        let (b, mut brd, t) = match r.def.action {
            KeyAction::TogglePin => (
                [22, 138, 86, 255],
                [110, 231, 183, 255],
                [240, 255, 248, 255],
            ),
            KeyAction::ToggleFollow
            | KeyAction::ToggleScreenHeadlock(_)
            | KeyAction::UnlockAllHeadlocks => (
                [18, 126, 134, 255],
                [94, 234, 212, 255],
                [240, 255, 254, 255],
            ),
            KeyAction::ToggleScreenHidden(_) => {
                if mode == PanelMode::Collapsed {
                    ([76, 34, 42, 248], [148, 72, 86, 255], [220, 198, 204, 255])
                } else {
                    (
                        [154, 52, 56, 255],
                        [248, 113, 113, 255],
                        [255, 242, 244, 255],
                    )
                }
            }
            KeyAction::ToggleScreens => (
                [28, 102, 148, 255],
                [104, 198, 248, 255],
                [244, 252, 255, 255],
            ),
            KeyAction::ShowAllScreens => (
                [34, 98, 178, 250],
                [96, 165, 250, 255],
                [245, 250, 255, 255],
            ),
            _ => (
                [37, 99, 235, 255],
                [147, 197, 253, 255],
                [255, 255, 255, 255],
            ),
        };
        if is_hovered {
            brd = [235, 248, 255, 255];
        }
        (b, brd, t)
    } else if is_hovered {
        (
            [72, 86, 112, 255],
            [148, 178, 226, 255],
            [255, 255, 255, 255],
        )
    } else {
        match r.def.action {
            KeyAction::ShowKeyboard => (
                [34, 98, 178, 250],
                [96, 165, 250, 255],
                [245, 250, 255, 255],
            ),
            KeyAction::BringScreen(_) => (
                [34, 82, 146, 250],
                [88, 152, 232, 255],
                [242, 248, 255, 255],
            ),
            KeyAction::ToggleScreenHidden(id) if mode == PanelMode::Collapsed => {
                if screens
                    .iter()
                    .find(|s| s.stream_id == id)
                    .is_some_and(|s| s.head_locked)
                {
                    (
                        [18, 126, 134, 255],
                        [94, 234, 212, 255],
                        [240, 255, 254, 255],
                    )
                } else {
                    (
                        [34, 98, 178, 250],
                        [96, 165, 250, 255],
                        [245, 250, 255, 255],
                    )
                }
            }
            KeyAction::ShowAllScreens | KeyAction::UnlockAllHeadlocks => {
                ([34, 44, 60, 248], [68, 84, 108, 255], [178, 192, 212, 255])
            }
            KeyAction::TogglePin
            | KeyAction::ToggleFollow
            | KeyAction::Recenter
            | KeyAction::ToggleScreens
            | KeyAction::ToggleScreenHeadlock(_)
            | KeyAction::ToggleScreenHidden(_) => {
                ([36, 56, 76, 248], [82, 116, 148, 255], [226, 238, 248, 255])
            }
            KeyAction::Collapse => (
                [104, 42, 52, 248],
                [186, 86, 102, 255],
                [255, 236, 240, 255],
            ),
            KeyAction::Key(_) if r.def.label.len() > 1 && r.def.label != "Space" => {
                ([34, 41, 56, 252], [68, 80, 104, 255], [216, 224, 238, 255])
            }
            KeyAction::Modifier(_)
            | KeyAction::CapsLock
            | KeyAction::NumLock
            | KeyAction::ScrollLock => {
                ([34, 41, 56, 252], [68, 80, 104, 255], [216, 224, 238, 255])
            }
            _ => ([44, 52, 68, 252], [80, 94, 120, 255], [244, 247, 252, 255]),
        }
    };

    (label, bg, border, text_col)
}

#[allow(clippy::too_many_arguments)]
fn draw_panel_button(
    buf: &mut [u8],
    w: usize,
    h: usize,
    font: Option<&FontRef<'_>>,
    mode: PanelMode,
    upper: bool,
    shift: bool,
    monitors_pinned: bool,
    following: bool,
    screens: &[ScreenUiEntry],
    r: &KeyRect,
    is_hovered: bool,
    is_pressed: bool,
    is_active: bool,
) {
    if mode == PanelMode::Keyboard {
        draw_keyboard_key(
            buf,
            w,
            h,
            font,
            upper,
            shift,
            monitors_pinned,
            following,
            screens,
            r,
            is_hovered,
            is_pressed,
            is_active,
        );
        return;
    }

    let x0 = r.x0 * w as f32;
    let y0 = r.y0 * h as f32;
    let x1 = r.x1 * w as f32;
    let y1 = r.y1 * h as f32;

    let (label, bg, border, text_col) = resolve_key_visual(
        r,
        mode,
        upper,
        shift,
        monitors_pinned,
        following,
        screens,
        is_hovered,
        is_pressed,
        is_active,
    );

    let y_shift = if is_pressed { 2.0 } else { 0.0 };
    if !is_pressed {
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

    if let Some(f) = font {
        let char_count = label.chars().count();
        let base_px = match char_count {
            1 => 34.0,
            2..=4 => 25.0,
            _ => 21.0,
        };
        let fit_px = ((x1 - x0 - 16.0).max(20.0) / (char_count.max(1) as f32 * 0.56)).min(base_px);
        let px_size = fit_px.clamp(14.0, 34.0);
        draw_ttf_centered(
            buf,
            w,
            h,
            f,
            px_size,
            (x0 + x1) * 0.5,
            (y0 + y1) * 0.5 + y_shift,
            &label,
            text_col,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_keyboard_key(
    buf: &mut [u8],
    w: usize,
    h: usize,
    font: Option<&FontRef<'_>>,
    upper: bool,
    shift: bool,
    monitors_pinned: bool,
    following: bool,
    screens: &[ScreenUiEntry],
    r: &KeyRect,
    is_hovered: bool,
    is_pressed: bool,
    is_active: bool,
) {
    let x0 = r.x0 * w as f32;
    let y0 = r.y0 * h as f32;
    let x1 = r.x1 * w as f32;
    let y1 = r.y1 * h as f32;
    let y_shift = if is_pressed { 2.5 } else { 0.0 };

    let (label, _, _, _) = resolve_key_visual(
        r,
        PanelMode::Keyboard,
        upper,
        shift,
        monitors_pinned,
        following,
        screens,
        is_hovered,
        is_pressed,
        is_active,
    );

    let [sr, sg, sb] = rgb_spectrum(((r.x0 + r.x1) * 0.5).clamp(0.0, 1.0));

    if r.def.style == KeyStyle::BezelPill {
        let (fill, border, text_col) = if is_pressed {
            (
                [59, 130, 246, 255],
                [191, 219, 254, 255],
                [255, 255, 255, 255],
            )
        } else if is_active {
            (
                [24, 92, 134, 255],
                if is_hovered {
                    [225, 244, 255, 255]
                } else {
                    [98, 194, 246, 255]
                },
                [242, 250, 255, 255],
            )
        } else if is_hovered {
            (
                [64, 78, 102, 255],
                [156, 184, 224, 255],
                [255, 255, 255, 255],
            )
        } else {
            ([34, 41, 52, 250], [82, 94, 114, 255], [220, 230, 244, 255])
        };
        draw_rounded_rect_sdf(
            buf,
            w,
            h,
            x0,
            y0 + y_shift,
            x1,
            y1 + y_shift,
            10.0,
            fill,
            border,
            1.5,
        );
        if let Some(f) = font {
            draw_ttf_centered(
                buf,
                w,
                h,
                f,
                19.0,
                (x0 + x1) * 0.5,
                (y0 + y1) * 0.5 + y_shift,
                &label,
                text_col,
            );
        }
        return;
    }

    if r.def.style == KeyStyle::Media {
        let (fill, border, led_col, text_col) = if is_pressed {
            (
                [46, 98, 178, 255],
                [180, 214, 255, 255],
                [190, 224, 255, 255],
                [255, 255, 255, 255],
            )
        } else if is_active {
            match r.def.action {
                KeyAction::TogglePin => (
                    [20, 108, 72, 255],
                    if is_hovered {
                        [224, 255, 242, 255]
                    } else {
                        [98, 226, 172, 255]
                    },
                    [110, 246, 188, 255],
                    [242, 255, 248, 255],
                ),
                _ => (
                    [18, 102, 116, 255],
                    if is_hovered {
                        [224, 252, 255, 255]
                    } else {
                        [94, 234, 212, 255]
                    },
                    [110, 248, 236, 255],
                    [240, 255, 254, 255],
                ),
            }
        } else if is_hovered {
            (
                [60, 72, 94, 255],
                [148, 178, 226, 255],
                [148, 184, 236, 255],
                [255, 255, 255, 255],
            )
        } else {
            match r.def.action {
                KeyAction::Collapse => (
                    [62, 30, 38, 255],
                    [146, 74, 88, 255],
                    [236, 86, 104, 255],
                    [252, 228, 234, 255],
                ),
                _ => (
                    [32, 38, 48, 255],
                    [78, 90, 110, 255],
                    [92, 128, 168, 220],
                    [226, 236, 248, 255],
                ),
            }
        };

        draw_rounded_rect_sdf(
            buf,
            w,
            h,
            x0,
            y0 + y_shift,
            x1,
            y1 + y_shift,
            10.0,
            fill,
            border,
            1.5,
        );
        // Media button top status LED accent bar
        draw_rounded_rect_sdf(
            buf,
            w,
            h,
            x0 + 14.0,
            y0 + 7.0 + y_shift,
            x1 - 14.0,
            y0 + 12.0 + y_shift,
            2.5,
            led_col,
            led_col,
            0.0,
        );
        if let Some(f) = font {
            let char_count = label.chars().count();
            let px_size = ((x1 - x0 - 16.0).max(20.0) / (char_count.max(1) as f32 * 0.56))
                .min(20.0)
                .clamp(13.0, 20.0);
            draw_ttf_centered(
                buf,
                w,
                h,
                f,
                px_size,
                (x0 + x1) * 0.5,
                (y0 + y1) * 0.5 + 4.0 + y_shift,
                &label,
                text_col,
            );
        }
        return;
    }

    // Per-key RGB underglow halo
    let glow_alpha = if is_pressed {
        225
    } else if is_active || is_hovered {
        175
    } else {
        82
    };
    let (gr, gg, gb) = if is_active {
        (96, 210, 255)
    } else {
        (sr, sg, sb)
    };
    draw_rounded_rect_sdf(
        buf,
        w,
        h,
        x0 - 2.5,
        y0 - 1.5 + y_shift,
        x1 + 2.5,
        y1 + 3.5 + y_shift,
        10.0,
        [gr, gg, gb, glow_alpha],
        [gr, gg, gb, glow_alpha],
        0.0,
    );

    // Sculpted 3D keycap skirt + top dished cap
    let (skirt_fill, skirt_border, top_fill, top_border, text_col) = if is_pressed {
        (
            [42, 92, 186, 255],
            [176, 214, 255, 255],
            [59, 130, 246, 255],
            [205, 228, 255, 255],
            [255, 255, 255, 255],
        )
    } else if is_active {
        let brd = if is_hovered {
            [230, 246, 255, 255]
        } else {
            [132, 198, 255, 255]
        };
        (
            [26, 68, 152, 255],
            brd,
            [37, 99, 228, 255],
            [162, 212, 255, 255],
            [255, 255, 255, 255],
        )
    } else if is_hovered {
        (
            [48, 58, 76, 255],
            [144, 174, 222, 255],
            [72, 84, 108, 255],
            [172, 198, 240, 255],
            [255, 255, 255, 255],
        )
    } else {
        match r.def.style {
            KeyStyle::Modifier => (
                [18, 21, 27, 255],
                [56, 64, 78, 255],
                [28, 32, 41, 255],
                [70, 80, 96, 255],
                [222, 230, 244, 255],
            ),
            KeyStyle::Accent => (
                [30, 35, 44, 255],
                [104, 116, 136, 255],
                [50, 56, 70, 255],
                [128, 140, 162, 255],
                [248, 252, 255, 255],
            ),
            KeyStyle::Spacebar => (
                [18, 21, 27, 255],
                [64, 72, 86, 255],
                [30, 34, 44, 255],
                [84, 94, 112, 255],
                [220, 232, 248, 255],
            ),
            KeyStyle::Standard | KeyStyle::Media | KeyStyle::BezelPill => (
                [21, 25, 32, 255],
                [66, 74, 90, 255],
                [36, 42, 54, 255],
                [86, 98, 118, 255],
                [246, 249, 255, 255],
            ),
        }
    };

    draw_rounded_rect_sdf(
        buf,
        w,
        h,
        x0,
        y0 + y_shift,
        x1,
        y1 + y_shift,
        9.0,
        skirt_fill,
        skirt_border,
        1.2,
    );
    let tx0 = x0 + 4.5;
    let ty0 = y0 + 3.0 + y_shift;
    let tx1 = x1 - 4.5;
    let ty1 = y1 - 6.5 + y_shift;
    draw_rounded_rect_sdf(
        buf, w, h, tx0, ty0, tx1, ty1, 7.0, top_fill, top_border, 1.0,
    );

    if r.def.style == KeyStyle::Spacebar {
        // Diagonal textured micro-hatch & glowing center bar
        let ix0 = (tx0 as usize + 12).min(w);
        let ix1 = (tx1 as usize).saturating_sub(12).min(w);
        let iy0 = (ty0 as usize + 5).min(h);
        let iy1 = (ty1 as usize).saturating_sub(5).min(h);
        for py in iy0..iy1 {
            for px in ix0..ix1 {
                if ((px + py) % 10) < 3 {
                    let off = (py * w + px) * 4;
                    blend_pixel(&mut buf[off..off + 4], [120, 134, 158, 255], 0.16);
                }
            }
        }
        let mid_x = (tx0 + tx1) * 0.5;
        let mid_y = (ty0 + ty1) * 0.5;
        draw_rounded_rect_sdf(
            buf,
            w,
            h,
            mid_x - 70.0,
            mid_y - 2.5,
            mid_x + 70.0,
            mid_y + 2.5,
            2.5,
            [sr, sg, sb, 225],
            [sr, sg, sb, 225],
            0.0,
        );
        return;
    }

    if let Some(f) = font {
        let is_dual_symbol = r.def.label != r.def.shift_label
            && !(r.def.label.len() == 1 && r.def.label.chars().all(|c| c.is_ascii_alphabetic()));
        if is_dual_symbol {
            let sec_col = if shift {
                [136, 148, 170, 210]
            } else {
                [152, 166, 190, 220]
            };
            let primary_lbl = if shift {
                r.def.label
            } else {
                r.def.shift_label
            };
            let active_lbl = if shift {
                r.def.shift_label
            } else {
                r.def.label
            };
            draw_ttf_aligned(
                buf,
                w,
                h,
                f,
                14.5,
                tx0 + 8.0,
                ty0 + 13.0,
                primary_lbl,
                sec_col,
                false,
            );
            draw_ttf_centered(
                buf,
                w,
                h,
                f,
                25.0,
                (tx0 + tx1) * 0.5 + 4.0,
                (ty0 + ty1) * 0.5 + 7.0,
                active_lbl,
                text_col,
            );
        } else {
            let char_count = label.chars().count();
            let base_px = match char_count {
                1 => 28.0,
                2..=3 => 21.0,
                _ => 17.5,
            };
            let fit_px =
                ((tx1 - tx0 - 10.0).max(16.0) / (char_count.max(1) as f32 * 0.56)).min(base_px);
            let px_size = fit_px.clamp(12.5, 28.0);
            draw_ttf_centered(
                buf,
                w,
                h,
                f,
                px_size,
                (tx0 + tx1) * 0.5,
                (ty0 + ty1) * 0.5,
                &label,
                text_col,
            );
        }
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
        7 => 11,                            // 0
        8..=16 => (keycode - 8 + 2) as u32, // 1..9 -> KEY_1..KEY_9
        29 => 30,                           // A
        30 => 48,                           // B
        31 => 46,                           // C
        32 => 32,                           // D
        33 => 18,                           // E
        34 => 33,                           // F
        35 => 34,                           // G
        36 => 35,                           // H
        37 => 23,                           // I
        38 => 36,                           // J
        39 => 37,                           // K
        40 => 38,                           // L
        41 => 50,                           // M
        42 => 49,                           // N
        43 => 24,                           // O
        44 => 25,                           // P
        45 => 16,                           // Q
        46 => 19,                           // R
        47 => 31,                           // S
        48 => 20,                           // T
        49 => 22,                           // U
        50 => 47,                           // V
        51 => 17,                           // W
        52 => 45,                           // X
        53 => 21,                           // Y
        54 => 44,                           // Z
        55 => 51,                           // ,
        56 => 52,                           // .
        57 => 56,                           // AltLeft
        58 => 100,                          // AltRight
        59 => 42,                           // ShiftLeft
        60 => 54,                           // ShiftRight
        61 => 15,                           // Tab
        62 => 57,                           // Space
        66 => 28,                           // Enter
        67 => 14,                           // Del (Backspace)
        68 => 41,                           // Grave
        69 => 12,                           // Minus
        70 => 13,                           // Equals
        71 => 26,                           // LeftBracket
        72 => 27,                           // RightBracket
        73 => 43,                           // Backslash
        74 => 39,                           // Semicolon
        75 => 40,                           // Apostrophe
        76 => 53,                           // Slash
        19 => 103,                          // DpadUp
        20 => 108,                          // DpadDown
        21 => 105,                          // DpadLeft
        22 => 106,                          // DpadRight
        111 => 1,                           // Escape
        112 => 111,                         // ForwardDel (Delete)
        113 => 29,                          // CtrlLeft
        114 => 97,                          // CtrlRight
        115 => 58,                          // CapsLock
        117 => 125,                         // MetaLeft
        118 => 126,                         // MetaRight
        122 => 102,                         // MoveHome
        123 => 107,                         // MoveEnd
        92 => 104,                          // PageUp
        93 => 109,                          // PageDown
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
    dst[0] = (src[0] as f32 * a + dst[0] as f32 * inv)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[1] = (src[1] as f32 * a + dst[1] as f32 * inv)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[2] = (src[2] as f32 * a + dst[2] as f32 * inv)
        .round()
        .clamp(0.0, 255.0) as u8;
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
fn draw_ttf_aligned(
    buf: &mut [u8],
    tex_w: usize,
    tex_h: usize,
    font: &FontRef<'_>,
    px_size: f32,
    anchor_x: f32,
    center_y: f32,
    text: &str,
    color: [u8; 4],
    center_x: bool,
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

    let mut pen_x = if center_x {
        anchor_x - total_w * 0.5
    } else {
        anchor_x
    };
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
    draw_ttf_aligned(
        buf, tex_w, tex_h, font, px_size, center_x, center_y, text, color, true,
    );
}

fn draw_reticle(buf: &mut [u8], tex_w: usize, tex_h: usize, cx: f32, cy: f32, core: [u8; 4]) {
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

    fn assert_no_overlaps(rects: &[KeyRect], context: &str) {
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let a = &rects[i];
                let b = &rects[j];
                let overlap_x = a.x0 < b.x1 - 1e-4 && a.x1 > b.x0 + 1e-4;
                let overlap_y = a.y0 < b.y1 - 1e-4 && a.y1 > b.y0 + 1e-4;
                assert!(
                    !(overlap_x && overlap_y),
                    "{}: overlapping rects {} ({}) and {} ({})",
                    context,
                    i,
                    a.def.label,
                    j,
                    b.def.label
                );
            }
        }
    }

    #[test]
    fn ansi_layout_completeness_non_overlap_and_render() {
        let mut kb = VirtualKeyboard::new();
        assert_eq!(kb.rects.len(), 109);
        assert_no_overlaps(&kb.rects, "Full ANSI Keyboard");
        assert_no_overlaps(&kb.toolbar_rects, "Collapsed Dock");

        // Verify presence of full ANSI sections, right-side modifiers, NumPad, and media bay
        for expected_action in [
            KeyAction::Key(1),  // Esc
            KeyAction::Key(59), // F1
            KeyAction::Key(88), // F12
            KeyAction::Key(99), // PrtSc
            KeyAction::ScrollLock,
            KeyAction::Key(119),      // Pause
            KeyAction::Key(41),       // `
            KeyAction::Key(43),       // Backslash
            KeyAction::Key(28),       // Enter
            KeyAction::Modifier(54),  // Right Shift
            KeyAction::Modifier(97),  // Right Ctrl
            KeyAction::Modifier(100), // Right Alt
            KeyAction::Modifier(126), // Right Win
            KeyAction::Key(127),      // Menu
            KeyAction::NumLock,
            KeyAction::Key(78), // NumPad +
            KeyAction::Key(96), // NumPad Enter
            KeyAction::ToggleScreens,
            KeyAction::Recenter,
            KeyAction::Collapse,
            KeyAction::TogglePin,
            KeyAction::ToggleFollow,
        ] {
            assert!(
                kb.rects.iter().any(|r| r.def.action == expected_action),
                "missing action {:?}",
                expected_action
            );
        }

        // Top-right media bay buttons: Hide, Pin, Follow
        let hide_idx = kb
            .rects
            .iter()
            .position(|r| r.def.action == KeyAction::Collapse)
            .unwrap();
        let pin_idx = kb
            .rects
            .iter()
            .position(|r| r.def.action == KeyAction::TogglePin)
            .unwrap();
        let follow_idx = kb
            .rects
            .iter()
            .position(|r| r.def.action == KeyAction::ToggleFollow)
            .unwrap();
        assert!(kb.rects[hide_idx].x1 < kb.rects[pin_idx].x0);
        assert!(kb.rects[pin_idx].x1 < kb.rects[follow_idx].x0);

        kb.mode = PanelMode::Keyboard;
        let hm = &kb.rects[hide_idx];
        assert_eq!(
            kb.hit_test((hm.x0 + hm.x1) * 0.5, (hm.y0 + hm.y1) * 0.5),
            Some(hide_idx)
        );
        let fm = &kb.rects[follow_idx];
        assert_eq!(
            kb.hit_test((fm.x0 + fm.x1) * 0.5, (fm.y0 + fm.y1) * 0.5),
            Some(follow_idx)
        );

        // Tall 2u NumPad '+' and 'Ent' span two rows
        let plus_rect = kb
            .rects
            .iter()
            .find(|r| r.def.action == KeyAction::Key(78))
            .unwrap();
        let key9_rect = kb
            .rects
            .iter()
            .find(|r| r.def.action == KeyAction::Key(73))
            .unwrap();
        assert!((plus_rect.y1 - plus_rect.y0) > (key9_rect.y1 - key9_rect.y0) * 1.8);

        let expected_len = (KB_TEX_WIDTH * KB_TEX_HEIGHT * 4) as usize;
        kb.cursors[0] = Some((0.5, 0.5));
        assert_eq!(kb.render_rgba().len(), expected_len);

        // Incremental hover, press, cursor movement, and unhover fast paths
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
    fn hit_test_snaps_inter_key_gaps_but_not_inter_block_gaps() {
        let mut kb = VirtualKeyboard::new();
        // Outside collapsed toolbar dock still returns None for click-through
        assert!(kb.hit_test(0.05, 0.20).is_none());

        kb.mode = PanelMode::Keyboard;
        // Two horizontally adjacent keys in the number row ('1' and '2')
        let r1 = kb
            .rects
            .iter()
            .find(|r| r.def.action == KeyAction::Key(2))
            .unwrap();
        let r2 = kb
            .rects
            .iter()
            .find(|r| r.def.action == KeyAction::Key(3))
            .unwrap();
        let gap_u = (r1.x1 + r2.x0) * 0.5;
        let mid_v = (r1.y0 + r1.y1) * 0.5;
        assert!(!(r1.x0..=r1.x1).contains(&gap_u));
        assert!(!(r2.x0..=r2.x1).contains(&gap_u));
        assert!(kb.hit_test(gap_u, mid_v).is_some());

        // Wide inter-block gap between Main block (15.55u) and Nav cluster (16.00u) does not snap
        assert!(kb.hit_test(15.77 / TOTAL_U_W, 4.0 / TOTAL_U_H).is_none());
        // Empty gap above Up arrow in Nav Row 3 does not snap
        assert!(kb.hit_test(16.5 / TOTAL_U_W, 5.15 / TOTAL_U_H).is_none());
    }

    #[test]
    fn collapsed_hud_shows_per_screen_toggles_follow_and_preserves_click_through() {
        let mut kb = VirtualKeyboard::new();
        kb.sync_screens(vec![
            ScreenUiEntry {
                stream_id: 101,
                name: "Monitor-1".into(),
                width: 2560,
                height: 1440,
                hidden: false,
                head_locked: false,
                pinned: false,
            },
            ScreenUiEntry {
                stream_id: 102,
                name: "Monitor-2".into(),
                width: 1920,
                height: 1080,
                hidden: true,
                head_locked: false,
                pinned: false,
            },
        ]);
        assert_no_overlaps(&kb.toolbar_rects, "Collapsed Dock 2 screens");

        // Upper region above the dock remains pass-through
        assert!(kb.hit_test(0.5, 0.20).is_none());

        // Row 1 screen visibility toggles hit-test accurately
        let left_toggle = kb.hit_test(0.32, 0.64).expect("screen 1 toggle");
        assert_eq!(
            kb.active_rects()[left_toggle].def.action,
            KeyAction::ToggleScreenHidden(101)
        );
        let right_toggle = kb.hit_test(0.68, 0.64).expect("screen 2 toggle");
        assert_eq!(
            kb.active_rects()[right_toggle].def.action,
            KeyAction::ToggleScreenHidden(102)
        );

        // Row 2 main controls hit-test accurately (Keyboard, Screens, Pin, Center)
        let kb_btn = kb.hit_test(0.25, 0.85).expect("keyboard dock btn");
        assert_eq!(
            kb.active_rects()[kb_btn].def.action,
            KeyAction::ShowKeyboard
        );
        let screens_btn = kb.hit_test(0.45, 0.85).expect("screens dock btn");
        assert_eq!(
            kb.active_rects()[screens_btn].def.action,
            KeyAction::ToggleScreens
        );
        let pin_btn = kb.hit_test(0.62, 0.85).expect("pin dock btn");
        assert_eq!(kb.active_rects()[pin_btn].def.action, KeyAction::TogglePin);
        let center_btn = kb.hit_test(0.77, 0.85).expect("center dock btn");
        assert_eq!(
            kb.active_rects()[center_btn].def.action,
            KeyAction::Recenter
        );

        let expected_len = (KB_TEX_WIDTH * KB_TEX_HEIGHT * 4) as usize;
        assert_eq!(kb.render_rgba().len(), expected_len);
    }

    #[test]
    fn hud_always_follows_and_hidden_mode_is_non_interactive() {
        let mut kb = VirtualKeyboard::new();
        assert!(kb.is_visible());
        assert!(kb.is_hud_panel());
        assert_eq!(kb.active_view_offset(), Some(kb.hud_view_offset));

        kb.mode = PanelMode::Screens;
        assert!(kb.is_hud_panel());
        assert!(kb.is_expanded());
        assert_eq!(kb.active_view_offset(), Some(kb.hud_view_offset));

        kb.mode = PanelMode::Keyboard;
        assert!(!kb.is_hud_panel());
        assert!(kb.is_expanded());
        assert_eq!(kb.active_view_offset(), None);
        let custom_lock = Mat4::from_translation(Vec3::new(0.1, -0.2, -0.8));
        kb.head_lock = Some(custom_lock);
        assert_eq!(kb.active_view_offset(), Some(custom_lock));

        kb.mode = PanelMode::Hidden;
        assert!(!kb.is_visible());
        assert!(!kb.is_hud_panel());
        assert!(!kb.is_expanded());
        assert!(kb.active_rects().is_empty());
        assert_eq!(kb.active_view_offset(), None);
        assert!(kb.hit_test(0.25, 0.85).is_none());
        let expected_len = (KB_TEX_WIDTH * KB_TEX_HEIGHT * 4) as usize;
        assert_eq!(kb.render_rgba().len(), expected_len);
    }

    #[test]
    fn screens_panel_layout_hit_testing_and_render_for_multiple_counts() {
        let expected_len = (KB_TEX_WIDTH * KB_TEX_HEIGHT * 4) as usize;
        let mut kb = VirtualKeyboard::new();
        kb.mode = PanelMode::Screens;
        assert_eq!(kb.render_rgba().len(), expected_len);

        for count in 1usize..=4usize {
            let entries: Vec<ScreenUiEntry> = (0..count)
                .map(|i| ScreenUiEntry {
                    stream_id: 200 + i as u32,
                    name: format!("Monitor-{}", i + 1),
                    width: 2560,
                    height: 1440,
                    hidden: i % 2 == 1,
                    head_locked: i == 0,
                    pinned: false,
                })
                .collect();
            kb.sync_screens(entries);
            assert_no_overlaps(&kb.screen_rects, "Screens Panel");
            assert_eq!(kb.render_rgba().len(), expected_len);

            // Close button at top-right
            let close_idx = kb.hit_test(0.91, 0.09).expect("close button");
            assert_eq!(kb.active_rects()[close_idx].def.action, KeyAction::Collapse);

            // Bottom bar buttons
            let show_all_idx = kb.hit_test(0.36, 0.87).expect("show all");
            assert_eq!(
                kb.active_rects()[show_all_idx].def.action,
                KeyAction::ShowAllScreens
            );
            let unlock_idx = kb.hit_test(0.60, 0.87).expect("unlock all");
            assert_eq!(
                kb.active_rects()[unlock_idx].def.action,
                KeyAction::UnlockAllHeadlocks
            );

            // First row controls (row 0 center y)
            let row0_y = kb.active_rects()[1].y0 * 0.5 + kb.active_rects()[1].y1 * 0.5;
            let hide_idx = kb.hit_test(0.49, row0_y).expect("row0 hide");
            assert_eq!(
                kb.active_rects()[hide_idx].def.action,
                KeyAction::ToggleScreenHidden(200)
            );
            let hl_idx = kb.hit_test(0.70, row0_y).expect("row0 headlock");
            assert_eq!(
                kb.active_rects()[hl_idx].def.action,
                KeyAction::ToggleScreenHeadlock(200)
            );
            let bring_idx = kb.hit_test(0.89, row0_y).expect("row0 bring");
            assert_eq!(
                kb.active_rects()[bring_idx].def.action,
                KeyAction::BringScreen(200)
            );

            // Label area on the left should not snap to buttons in Screens mode
            assert!(kb.hit_test(0.15, row0_y).is_none());
        }
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
