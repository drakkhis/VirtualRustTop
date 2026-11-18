use glam::{Mat4, Vec2, Vec3};

#[derive(Debug, Clone)]
pub struct CylinderScreen {
    pub stream_id: u32,
    pub world_from_cylinder: Mat4,
    pub cylinder_from_world: Mat4,
    pub radius: f32,
    pub central_angle: f32,
    pub aspect_ratio: f32, // width / height
}

impl CylinderScreen {
    pub fn new(
        stream_id: u32,
        world_from_cylinder: Mat4,
        radius: f32,
        central_angle: f32,
        aspect_ratio: f32,
    ) -> Self {
        Self {
            stream_id,
            world_from_cylinder,
            cylinder_from_world: world_from_cylinder.inverse(),
            radius,
            central_angle,
            aspect_ratio,
        }
    }

    pub fn set_transform(&mut self, world_from_cylinder: Mat4) {
        self.world_from_cylinder = world_from_cylinder;
        self.cylinder_from_world = world_from_cylinder.inverse();
    }

    pub fn intersect_ray(&self, ray_origin_world: Vec3, ray_dir_world: Vec3) -> Option<(f32, Vec2)> {
        let o = self.cylinder_from_world.transform_point3(ray_origin_world);
        let d = self
            .cylinder_from_world
            .transform_vector3(ray_dir_world)
            .normalize();

        let a = d.x * d.x + d.z * d.z;
        let b = 2.0 * (o.x * d.x + o.z * d.z);
        let c = o.x * o.x + o.z * o.z - self.radius * self.radius;

        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 || a.abs() < 1e-6 {
            return None;
        }

        // Concave inner surface intersection along -Z
        let t = (-b + disc.sqrt()) / (2.0 * a);
        if t <= 0.0 {
            return None;
        }

        let p = o + d * t;
        let theta = p.x.atan2(-p.z);
        let half_angle = self.central_angle * 0.5;
        if theta.abs() > half_angle {
            return None;
        }

        let arc_width = self.radius * self.central_angle;
        let height = arc_width / self.aspect_ratio;
        let half_height = height * 0.5;

        if p.y.abs() > half_height {
            return None;
        }

        let u = (theta / self.central_angle) + 0.5;
        let v = 0.5 - (p.y / height);

        Some((t, Vec2::new(u, v)))
    }
}

/// Raycast against a flat 3D quad centered at `world_from_quad`'s origin in its local XY plane.
/// Returns `(hit_distance, uv)` with `u, v in [0.0, 1.0]` (top-left `(0, 0)`).
pub fn intersect_quad(
    world_from_quad: Mat4,
    width: f32,
    height: f32,
    ray_origin_world: Vec3,
    ray_dir_world: Vec3,
) -> Option<(f32, Vec2)> {
    let quad_from_world = world_from_quad.inverse();
    let o = quad_from_world.transform_point3(ray_origin_world);
    let d = quad_from_world.transform_vector3(ray_dir_world).normalize();

    if d.z.abs() < 1e-6 {
        return None;
    }
    let t = -o.z / d.z;
    if t <= 0.0 {
        return None;
    }
    let p = o + d * t;
    let u = (p.x / width) + 0.5;
    let v = 0.5 - (p.y / height);
    if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
        Some((t, Vec2::new(u, v)))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_ray_hits_uv_half_half() {
        let screen = CylinderScreen::new(1, Mat4::IDENTITY, 1.8, 1.1, 16.0 / 9.0);
        let (t, uv) = screen
            .intersect_ray(Vec3::ZERO, Vec3::NEG_Z)
            .expect("center ray should hit");
        assert!((t - 1.8).abs() < 1e-4);
        assert!((uv.x - 0.5).abs() < 1e-4);
        assert!((uv.y - 0.5).abs() < 1e-4);
    }

    #[test]
    fn quad_ray_hits_center_and_misses_outside() {
        let quad = Mat4::from_translation(Vec3::new(0.0, 0.0, -1.0));
        let (t, uv) = intersect_quad(quad, 0.8, 0.4, Vec3::ZERO, Vec3::NEG_Z)
            .expect("center ray should hit quad");
        assert!((t - 1.0).abs() < 1e-4);
        assert!((uv.x - 0.5).abs() < 1e-4);
        assert!((uv.y - 0.5).abs() < 1e-4);

        assert!(intersect_quad(quad, 0.8, 0.4, Vec3::new(1.0, 0.0, 0.0), Vec3::NEG_Z).is_none());
    }
}
