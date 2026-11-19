//! Velocity-adaptive cursor smoothing (2D One-Euro filter) and click/drag stabilization.

use glam::Vec2;
use std::time::{Duration, Instant};

const MIN_CUTOFF_HZ: f32 = 1.8;
const BETA_PER_PX_S: f32 = 0.012;
const D_CUTOFF_HZ: f32 = 1.0;

const PRE_CLICK_ENGAGE: f32 = 0.12;
const PRE_CLICK_RELEASE: f32 = 0.08;
const PRE_CLICK_DEADZONE_PX: f32 = 18.0;
const DRAG_BREAKOUT_PX: f32 = 20.0;
const POST_CLICK_DEADZONE_PX: f32 = 14.0;
const POST_CLICK_GRACE: Duration = Duration::from_millis(250);
const MIN_SEND_DELTA_PX: f32 = 0.25;

#[derive(Debug, Clone, Copy)]
enum LockState {
    Idle,
    PreClick { anchor: Vec2 },
    ClickHold { anchor: Vec2 },
    Dragging,
    PostClick { anchor: Vec2, until: Instant },
}

pub struct PointerStabilizer {
    last_stream_id: Option<u32>,
    prev_raw_uv: Vec2,
    filtered_uv: Vec2,
    ed_vel_px: Vec2,
    lock: LockState,
    was_button_down: bool,
    last_sent: Option<(u32, Vec2)>,
}

impl Default for PointerStabilizer {
    fn default() -> Self {
        Self::new()
    }
}

impl PointerStabilizer {
    pub fn new() -> Self {
        Self {
            last_stream_id: None,
            prev_raw_uv: Vec2::ZERO,
            filtered_uv: Vec2::ZERO,
            ed_vel_px: Vec2::ZERO,
            lock: LockState::Idle,
            was_button_down: false,
            last_sent: None,
        }
    }

    pub fn reset(&mut self) {
        self.last_stream_id = None;
        self.lock = LockState::Idle;
        self.was_button_down = false;
        self.last_sent = None;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        stream_id: u32,
        raw_uv: Vec2,
        mon_w: f32,
        mon_h: f32,
        analog_trigger: f32,
        button_down: bool,
        dt: f32,
        now: Instant,
    ) -> Option<Vec2> {
        let raw = raw_uv.clamp(Vec2::ZERO, Vec2::ONE);
        let mon_w = mon_w.max(1.0);
        let mon_h = mon_h.max(1.0);

        if self.last_stream_id != Some(stream_id) {
            self.last_stream_id = Some(stream_id);
            self.prev_raw_uv = raw;
            self.filtered_uv = raw;
            self.ed_vel_px = Vec2::ZERO;
            self.was_button_down = button_down;
            self.lock = if button_down {
                LockState::ClickHold { anchor: raw }
            } else {
                LockState::Idle
            };
            self.last_sent = Some((stream_id, raw));
            return Some(raw);
        }

        // Update 2D isotropic One-Euro filter every frame to keep filter state warm.
        let dt = dt.max(1e-4);
        let mon_size = Vec2::new(mon_w, mon_h);
        let raw_vel_px = ((raw - self.prev_raw_uv) * mon_size) / dt;
        self.prev_raw_uv = raw;

        let a_d = smoothing_alpha(D_CUTOFF_HZ, dt);
        self.ed_vel_px = self.ed_vel_px.lerp(raw_vel_px, a_d);
        let speed = self.ed_vel_px.length();
        let cutoff = MIN_CUTOFF_HZ + BETA_PER_PX_S * speed;
        let a = smoothing_alpha(cutoff, dt);
        self.filtered_uv = self.filtered_uv.lerp(raw, a);

        if button_down && !self.was_button_down {
            let anchor = match self.lock {
                LockState::PreClick { anchor } => anchor,
                LockState::PostClick { anchor, until }
                    if now <= until && dist_px(raw, anchor, mon_w, mon_h) <= POST_CLICK_DEADZONE_PX =>
                {
                    anchor
                }
                _ => self.filtered_uv,
            };
            self.lock = LockState::ClickHold { anchor };
        } else if !button_down && self.was_button_down {
            self.lock = match self.lock {
                LockState::ClickHold { anchor } => LockState::PostClick {
                    anchor,
                    until: now + POST_CLICK_GRACE,
                },
                _ => LockState::Idle,
            };
        }
        self.was_button_down = button_down;

        let output_uv = match self.lock {
            LockState::ClickHold { anchor } => {
                if dist_px(self.filtered_uv, anchor, mon_w, mon_h) > DRAG_BREAKOUT_PX {
                    self.lock = LockState::Dragging;
                    self.filtered_uv
                } else {
                    anchor
                }
            }
            LockState::Dragging => self.filtered_uv,
            LockState::PreClick { anchor } => {
                if analog_trigger <= PRE_CLICK_RELEASE
                    || dist_px(self.filtered_uv, anchor, mon_w, mon_h) > PRE_CLICK_DEADZONE_PX
                {
                    self.lock = LockState::Idle;
                    self.filtered_uv
                } else {
                    anchor
                }
            }
            LockState::PostClick { anchor, until } => {
                if now > until
                    || dist_px(self.filtered_uv, anchor, mon_w, mon_h) > POST_CLICK_DEADZONE_PX
                {
                    self.lock = if analog_trigger > PRE_CLICK_ENGAGE {
                        LockState::PreClick {
                            anchor: self.filtered_uv,
                        }
                    } else {
                        LockState::Idle
                    };
                    self.filtered_uv
                } else {
                    anchor
                }
            }
            LockState::Idle => {
                if analog_trigger > PRE_CLICK_ENGAGE {
                    self.lock = LockState::PreClick {
                        anchor: self.filtered_uv,
                    };
                }
                self.filtered_uv
            }
        };

        if let Some((prev_id, prev_uv)) = self.last_sent {
            if prev_id == stream_id && dist_px(output_uv, prev_uv, mon_w, mon_h) < MIN_SEND_DELTA_PX {
                return None;
            }
        }
        self.last_sent = Some((stream_id, output_uv));
        Some(output_uv)
    }
}

fn smoothing_alpha(cutoff_hz: f32, dt: f32) -> f32 {
    let dt = dt.max(1e-4);
    let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz.max(1e-4));
    1.0 / (1.0 + tau / dt)
}

fn dist_px(a: Vec2, b: Vec2, mon_w: f32, mon_h: f32) -> f32 {
    ((a - b) * Vec2::new(mon_w, mon_h)).length()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 2560.0;
    const H: f32 = 1440.0;
    const DT: f32 = 1.0 / 90.0;

    #[test]
    fn pre_click_trigger_dip_holds_anchor_and_click_emits_zero_motion() {
        let mut stab = PointerStabilizer::new();
        let t0 = Instant::now();
        let center = Vec2::new(0.5, 0.5);

        assert_eq!(stab.update(1, center, W, H, 0.0, false, DT, t0), Some(center));

        // Begin squeezing trigger past PRE_CLICK_ENGAGE (0.12) before full click
        let t1 = t0 + Duration::from_millis(11);
        assert_eq!(stab.update(1, center, W, H, 0.25, false, DT, t1), None);

        // Controller dips by 12 px during trigger squeeze
        let dip_uv = Vec2::new(0.5, 0.5 + 12.0 / H);
        let t2 = t1 + Duration::from_millis(11);
        assert_eq!(stab.update(1, dip_uv, W, H, 0.45, false, DT, t2), None);

        // Full click down + 15 px twitch during hold emits zero pointer motion
        let twitch_uv = Vec2::new(0.5 + 10.0 / W, 0.5 + 11.0 / H);
        let t3 = t2 + Duration::from_millis(11);
        assert_eq!(stab.update(1, twitch_uv, W, H, 0.70, true, DT, t3), None);

        // Release stays locked cleanly at anchor
        let t4 = t3 + Duration::from_millis(11);
        assert_eq!(stab.update(1, twitch_uv, W, H, 0.0, false, DT, t4), None);
    }

    #[test]
    fn intentional_drag_breaks_out_past_deadzone() {
        let mut stab = PointerStabilizer::new();
        let mut t = Instant::now();
        let start = Vec2::new(0.4, 0.5);

        assert_eq!(stab.update(1, start, W, H, 0.0, false, DT, t), Some(start));
        t += Duration::from_millis(11);
        assert_eq!(stab.update(1, start, W, H, 0.8, true, DT, t), None);

        // Small 10 px jitter stays locked inside DRAG_BREAKOUT_PX
        t += Duration::from_millis(11);
        let jitter = Vec2::new(start.x + 10.0 / W, start.y);
        assert_eq!(stab.update(1, jitter, W, H, 0.8, true, DT, t), None);

        // Deliberate 80 px drag breaks out into smooth motion
        let target = Vec2::new(start.x + 80.0 / W, start.y);
        let mut broke_out = None;
        for _ in 0..20 {
            t += Duration::from_millis(11);
            if let Some(uv) = stab.update(1, target, W, H, 0.8, true, DT, t) {
                broke_out = Some(uv);
            }
        }
        let final_uv = broke_out.expect("intentional drag should break out of deadzone");
        assert!((final_uv.x - start.x) * W > DRAG_BREAKOUT_PX);
    }

    #[test]
    fn double_click_within_grace_snaps_to_same_pixel() {
        let mut stab = PointerStabilizer::new();
        let t0 = Instant::now();
        let anchor = Vec2::new(0.5, 0.5);

        assert_eq!(stab.update(1, anchor, W, H, 0.0, false, DT, t0), Some(anchor));
        let t1 = t0 + Duration::from_millis(11);
        assert_eq!(stab.update(1, anchor, W, H, 0.7, true, DT, t1), None);
        let t2 = t1 + Duration::from_millis(22);
        assert_eq!(stab.update(1, anchor, W, H, 0.0, false, DT, t2), None);

        // 100 ms later within POST_CLICK_DEADZONE_PX (8 px offset), second click still hits anchor
        let drifted = Vec2::new(anchor.x + 6.0 / W, anchor.y + 5.0 / H);
        let t3 = t2 + Duration::from_millis(100);
        assert_eq!(stab.update(1, drifted, W, H, 0.25, false, DT, t3), None);
        let t4 = t3 + Duration::from_millis(11);
        assert_eq!(stab.update(1, drifted, W, H, 0.7, true, DT, t4), None);
    }

    #[test]
    fn one_euro_damps_small_tremor_and_tracks_fast_sweep() {
        let mut stab = PointerStabilizer::new();
        let mut t = Instant::now();
        let center = Vec2::new(0.5, 0.5);
        stab.update(1, center, W, H, 0.0, false, DT, t);

        // Alternating +/- 4 px tremor should be strongly attenuated
        let mut max_dev_px = 0.0f32;
        for i in 0..30 {
            t += Duration::from_millis(11);
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            let raw = Vec2::new(center.x + sign * 4.0 / W, center.y);
            if let Some(uv) = stab.update(1, raw, W, H, 0.0, false, DT, t) {
                max_dev_px = max_dev_px.max(((uv - center) * Vec2::new(W, H)).length());
            }
        }
        assert!(max_dev_px < 2.0, "expected tremor < 2 px, got {}", max_dev_px);

        // Fast sweep (40 px per 90 Hz frame) scales cutoff up and tracks closely
        let mut latest = center;
        let mut raw = center;
        for _ in 0..20 {
            t += Duration::from_millis(11);
            raw.x += 40.0 / W;
            if let Some(uv) = stab.update(1, raw, W, H, 0.0, false, DT, t) {
                latest = uv;
            }
        }
        let lag_px = ((raw - latest) * Vec2::new(W, H)).length();
        assert!(lag_px < 20.0, "expected fast sweep lag < 20 px, got {}", lag_px);
    }
}
