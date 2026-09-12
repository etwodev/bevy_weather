//! Wind, including gusts.
//!
//! One resource, sampled by the clouds, the fog scroll and the precipitation,
//! so everything blows the same way.

use bevy::app::{App, Plugin, Update};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Res, ResMut};
use bevy::math::{Vec2, Vec3};
use bevy::reflect::Reflect;
use bevy::time::Time;

use crate::WeatherSystems;
use crate::math::fbm;
use crate::state::Weather;
use bevy::ecs::reflect::ReflectResource;
use bevy::reflect::std_traits::ReflectDefault;

/// The current wind, derived from [`Weather`] plus a gust simulation.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct Wind {
    /// Horizontal wind velocity in metres per second, gusts included.
    ///
    /// `x` is east, `y` is south (Bevy's `+Z`). Use [`velocity3`](Self::velocity3)
    /// for a world-space vector.
    pub velocity: Vec2,
    /// Steady wind velocity, without gusts.
    pub base_velocity: Vec2,
    /// Current gust multiplier around `1.0`.
    pub gust: f32,
    /// Cumulative wind displacement in metres. Clouds and fog scroll by this,
    /// so changing wind speed doesn't make them jump.
    pub offset: Vec2,
    /// Seed for the gust noise.
    pub seed: u32,
    /// Wraps [`offset`](Self::offset) at this many metres to keep shader-side
    /// `f32` precision usable during very long sessions.
    pub offset_wrap: f32,
}

impl Default for Wind {
    fn default() -> Self {
        Self {
            velocity: Vec2::ZERO,
            base_velocity: Vec2::ZERO,
            gust: 1.0,
            offset: Vec2::ZERO,
            seed: 0x5EED_1234,
            offset_wrap: 1.0e6,
        }
    }
}

impl Wind {
    /// Wind velocity as a world-space vector on the horizontal plane.
    #[inline]
    pub fn velocity3(&self) -> Vec3 {
        Vec3::new(self.velocity.x, 0.0, self.velocity.y)
    }

    /// Wind speed in metres per second.
    #[inline]
    pub fn speed(&self) -> f32 {
        self.velocity.length()
    }

    /// Accumulated displacement as a world-space vector.
    #[inline]
    pub fn offset3(&self) -> Vec3 {
        Vec3::new(self.offset.x, 0.0, self.offset.y)
    }
}

/// Converts a compass bearing to a horizontal direction vector.
///
/// `bearing` is radians clockwise from north, matching
/// [`WeatherConditions::wind_direction`](crate::state::WeatherConditions::wind_direction).
/// North is `-Z` and east is `+X`.
#[inline]
pub fn bearing_to_vec2(bearing: f32) -> Vec2 {
    let (sin, cos) = bearing.sin_cos();
    Vec2::new(sin, -cos)
}

/// Simulates gusts and integrates the wind offset.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindPlugin;

impl Plugin for WindPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wind>()
            .register_type::<Wind>()
            .add_systems(Update, update_wind.in_set(WeatherSystems::Apply));
    }
}

fn update_wind(mut wind: ResMut<Wind>, weather: Res<Weather>, time: Res<Time>) {
    let conditions = weather.current;
    let direction = bearing_to_vec2(conditions.wind_direction);
    wind.base_velocity = direction * conditions.wind_speed;

    // Two octaves at different rates: a slow swell plus a sharper gust front.
    let t = time.elapsed_secs();
    let seed = wind.seed;
    let swell = fbm(t * 0.07, 3, seed);
    let gust_front = fbm(t * 0.45, 2, seed ^ 0x9E37_79B9);
    // Turbulence controls how far gusts stray from the mean. A calm day barely
    // varies; a squall can double the speed and briefly drop it by half.
    let amount = conditions.turbulence;
    let gust = 1.0 + amount * (0.7 * (swell * 2.0 - 1.0) + 0.5 * (gust_front * 2.0 - 1.0));
    wind.gust = gust.max(0.0);

    wind.velocity = wind.base_velocity * wind.gust;

    let wrap = wind.offset_wrap.max(1.0);
    let offset = wind.offset + wind.velocity * time.delta_secs();
    wind.offset = Vec2::new(offset.x.rem_euclid(wrap), offset.y.rem_euclid(wrap));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearing_zero_points_north() {
        let v = bearing_to_vec2(0.0);
        assert!((v.x).abs() < 1e-6);
        assert!((v.y + 1.0).abs() < 1e-6, "north is -Z, got {v:?}");
    }

    #[test]
    fn bearing_ninety_degrees_points_east() {
        let v = bearing_to_vec2(core::f32::consts::FRAC_PI_2);
        assert!((v.x - 1.0).abs() < 1e-6, "{v:?}");
        assert!(v.y.abs() < 1e-6, "{v:?}");
    }

    #[test]
    fn bearings_are_unit_length() {
        for i in 0..64 {
            let b = i as f32 / 64.0 * core::f32::consts::TAU;
            assert!((bearing_to_vec2(b).length() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn velocity3_lies_in_the_horizontal_plane() {
        let wind = Wind {
            velocity: Vec2::new(3.0, -4.0),
            ..Default::default()
        };
        assert_eq!(wind.velocity3(), Vec3::new(3.0, 0.0, -4.0));
        assert!((wind.speed() - 5.0).abs() < 1e-6);
    }
}
