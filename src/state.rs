//! The weather itself: what it is now, what it is becoming, and how fast.

use bevy::app::{App, Plugin, Update};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Res, ResMut};
use bevy::reflect::Reflect;
use bevy::time::Time;

use crate::WeatherSystems;
use crate::math::damp;
use crate::presets::WeatherPreset;
use bevy::ecs::reflect::ReflectResource;
use bevy::reflect::std_traits::ReflectDefault;

/// A complete description of the weather at one instant.
///
/// Every field is independent, so you are not limited to the presets: sunlit
/// snow, a dry thunderstorm or fog under a clear sky are all expressible.
#[derive(Debug, Clone, Copy, PartialEq, Reflect)]
pub struct WeatherConditions {
    /// How much of the sky the cloud layer covers, `0.0..=1.0`.
    pub cloud_coverage: f32,
    /// How optically thick those clouds are, `0.0..=1.0`. Coverage decides
    /// *where* clouds are; density decides how dark and solid they look.
    pub cloud_density: f32,
    /// Altitude of the bottom of the cloud layer, in metres.
    pub cloud_altitude: f32,
    /// Vertical extent of the cloud layer, in metres. Tall values give
    /// cumulonimbus towers, short ones give flat stratus.
    pub cloud_thickness: f32,

    /// Rain intensity, `0.0..=1.0`.
    pub rain: f32,
    /// Snow intensity, `0.0..=1.0`. Rain and snow can overlap for sleet.
    pub snow: f32,

    /// Ground fog density, `0.0..=1.0`.
    pub fog: f32,
    /// Expected lightning strikes per minute.
    pub thunder: f32,

    /// Wind speed in metres per second.
    pub wind_speed: f32,
    /// Compass bearing the wind blows *toward*, in radians clockwise from north.
    pub wind_direction: f32,
    /// How gusty the wind is, `0.0..=1.0`.
    pub turbulence: f32,

    /// Relative humidity, `0.0..=1.0`. The procedural driver uses it; nothing
    /// renders from it directly.
    pub humidity: f32,
    /// Air temperature in degrees Celsius. The procedural driver uses this to
    /// decide between rain and snow.
    pub temperature: f32,
}

impl Default for WeatherConditions {
    fn default() -> Self {
        WeatherPreset::Clear.conditions()
    }
}

impl WeatherConditions {
    /// Linearly interpolates every field. `t` is clamped to `0.0..=1.0`.
    ///
    /// Wind direction takes the shorter way round the compass, so blending from
    /// 350° to 10° passes through north rather than sweeping all the way back.
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        // `a * (1 - t) + b * t` rather than `a + (b - a) * t`: the latter is
        // not bit-exact at `t == 1`, so a "finished" transition would sit a
        // rounding error away from its target forever.
        let l = |a: f32, b: f32| a * (1.0 - t) + b * t;
        Self {
            cloud_coverage: l(self.cloud_coverage, other.cloud_coverage),
            cloud_density: l(self.cloud_density, other.cloud_density),
            cloud_altitude: l(self.cloud_altitude, other.cloud_altitude),
            cloud_thickness: l(self.cloud_thickness, other.cloud_thickness),
            rain: l(self.rain, other.rain),
            snow: l(self.snow, other.snow),
            fog: l(self.fog, other.fog),
            thunder: l(self.thunder, other.thunder),
            wind_speed: l(self.wind_speed, other.wind_speed),
            wind_direction: lerp_angle(self.wind_direction, other.wind_direction, t),
            turbulence: l(self.turbulence, other.turbulence),
            humidity: l(self.humidity, other.humidity),
            temperature: l(self.temperature, other.temperature),
        }
    }

    /// Clamps every normalised field into range and keeps the rest sane.
    ///
    /// Called automatically on anything you feed into [`Weather`], so hand-built
    /// or externally-networked conditions cannot put the renderer into a bad
    /// state.
    pub fn sanitized(mut self) -> Self {
        let unit = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        self.cloud_coverage = unit(self.cloud_coverage);
        self.cloud_density = unit(self.cloud_density);
        self.rain = unit(self.rain);
        self.snow = unit(self.snow);
        self.fog = unit(self.fog);
        self.turbulence = unit(self.turbulence);
        self.humidity = unit(self.humidity);

        let positive = |v: f32, fallback: f32| {
            if v.is_finite() && v > 0.0 {
                v
            } else {
                fallback
            }
        };
        self.cloud_altitude = positive(self.cloud_altitude, 1_500.0);
        self.cloud_thickness = positive(self.cloud_thickness, 600.0);
        self.thunder = if self.thunder.is_finite() {
            self.thunder.max(0.0)
        } else {
            0.0
        };
        self.wind_speed = if self.wind_speed.is_finite() {
            self.wind_speed.clamp(0.0, 200.0)
        } else {
            0.0
        };
        self.wind_direction = if self.wind_direction.is_finite() {
            self.wind_direction.rem_euclid(core::f32::consts::TAU)
        } else {
            0.0
        };
        self.temperature = if self.temperature.is_finite() {
            self.temperature.clamp(-90.0, 60.0)
        } else {
            15.0
        };
        self
    }

    /// Total precipitation intensity, rain and snow together.
    #[inline]
    pub fn precipitation(self) -> f32 {
        (self.rain + self.snow).min(1.0)
    }
}

/// Interpolates between two angles the short way around.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    use core::f32::consts::TAU;
    let delta = (b - a).rem_euclid(TAU);
    let delta = if delta > TAU / 2.0 {
        delta - TAU
    } else {
        delta
    };
    (a + delta * t).rem_euclid(TAU)
}

/// The live weather state.
///
/// Set [`target`](Self::target) and the plugin eases [`current`](Self::current)
/// toward it; everything that renders reads `current`. Use the helpers rather
/// than writing the fields directly and you get the clamping for free.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct Weather {
    /// What is actually being rendered right now.
    pub current: WeatherConditions,
    /// What the weather is easing toward.
    pub target: WeatherConditions,
    /// Seconds for `current` to cover half the remaining distance to `target`.
    ///
    /// This is a half-life rather than a duration, which makes the blend
    /// frame-rate independent and lets you retarget mid-transition without any
    /// discontinuity. Zero snaps.
    pub transition_half_life: f32,
}

impl Default for Weather {
    fn default() -> Self {
        Self::new(WeatherConditions::default())
    }
}

impl Weather {
    /// Starts at `conditions`, with nothing to blend toward.
    pub fn new(conditions: WeatherConditions) -> Self {
        let conditions = conditions.sanitized();
        Self {
            current: conditions,
            target: conditions,
            transition_half_life: 8.0,
        }
    }

    /// Eases toward `conditions` over the configured half-life.
    pub fn set(&mut self, conditions: impl Into<WeatherConditions>) {
        self.target = conditions.into().sanitized();
    }

    /// Jumps straight to `conditions` with no transition.
    pub fn set_immediate(&mut self, conditions: impl Into<WeatherConditions>) {
        let conditions = conditions.into().sanitized();
        self.target = conditions;
        self.current = conditions;
    }

    /// True once `current` has essentially caught up with `target`.
    pub fn is_settled(&self) -> bool {
        let c = self.current;
        let t = self.target;
        let close = |a: f32, b: f32, eps: f32| (a - b).abs() <= eps;
        close(c.cloud_coverage, t.cloud_coverage, 1e-3)
            && close(c.cloud_density, t.cloud_density, 1e-3)
            && close(c.rain, t.rain, 1e-3)
            && close(c.snow, t.snow, 1e-3)
            && close(c.fog, t.fog, 1e-3)
            && close(c.wind_speed, t.wind_speed, 1e-2)
    }
}

impl From<WeatherPreset> for WeatherConditions {
    fn from(preset: WeatherPreset) -> Self {
        preset.conditions()
    }
}

/// Eases [`Weather::current`] toward [`Weather::target`] every frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct WeatherStatePlugin;

impl Plugin for WeatherStatePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Weather>()
            .register_type::<Weather>()
            .add_systems(Update, blend_weather.in_set(WeatherSystems::Simulate));
    }
}

/// Eases [`Weather::current`] toward [`Weather::target`].
///
/// Public so you can order your own systems around it.
pub fn blend_weather(mut weather: ResMut<Weather>, time: Res<Time>) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let half_life = weather.transition_half_life;
    if half_life <= 0.0 {
        weather.current = weather.target;
        return;
    }

    // `damp` on the blend factor rather than on each field keeps the angular
    // wind direction taking the short way round.
    let t = damp(0.0, 1.0, half_life, dt);
    weather.current = weather.current.lerp(weather.target, t);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_endpoints_are_exact() {
        let a = WeatherPreset::Clear.conditions();
        let b = WeatherPreset::Blizzard.conditions();
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
    }

    #[test]
    fn lerp_clamps_out_of_range_t() {
        let a = WeatherPreset::Clear.conditions();
        let b = WeatherPreset::Storm.conditions();
        assert_eq!(a.lerp(b, -3.0), a);
        assert_eq!(a.lerp(b, 7.0), b);
    }

    #[test]
    fn wind_direction_takes_the_short_way_round() {
        use core::f32::consts::TAU;
        let mut a = WeatherConditions::default();
        let mut b = WeatherConditions::default();
        a.wind_direction = 350f32.to_radians();
        b.wind_direction = 10f32.to_radians();

        let mid = a.lerp(b, 0.5).wind_direction;
        // Halfway should be due north, not 180 degrees away.
        let from_north = mid.min(TAU - mid);
        assert!(from_north < 1e-4, "{} deg", mid.to_degrees());
    }

    #[test]
    fn sanitize_clamps_and_repairs() {
        let bad = WeatherConditions {
            cloud_coverage: 5.0,
            rain: -2.0,
            fog: f32::NAN,
            cloud_altitude: -100.0,
            cloud_thickness: 0.0,
            wind_speed: f32::INFINITY,
            temperature: f32::NAN,
            thunder: -1.0,
            ..Default::default()
        }
        .sanitized();

        assert_eq!(bad.cloud_coverage, 1.0);
        assert_eq!(bad.rain, 0.0);
        assert_eq!(bad.fog, 0.0);
        assert!(bad.cloud_altitude > 0.0);
        assert!(bad.cloud_thickness > 0.0);
        assert_eq!(bad.wind_speed, 0.0);
        assert_eq!(bad.temperature, 15.0);
        assert_eq!(bad.thunder, 0.0);
    }

    #[test]
    fn set_sanitizes_the_target() {
        let mut w = Weather::default();
        w.set(WeatherConditions {
            rain: 12.0,
            ..Default::default()
        });
        assert_eq!(w.target.rain, 1.0);
    }

    #[test]
    fn set_immediate_skips_the_blend() {
        let mut w = Weather::new(WeatherPreset::Clear.conditions());
        w.set_immediate(WeatherPreset::Storm);
        assert_eq!(w.current, w.target);
        assert!(w.is_settled());
    }

    #[test]
    fn a_preset_converts_into_conditions() {
        let mut w = Weather::default();
        w.set(WeatherPreset::Thunderstorm);
        assert_eq!(w.target, WeatherPreset::Thunderstorm.conditions());
    }

    #[test]
    fn blending_converges() {
        let mut w = Weather::new(WeatherPreset::Clear.conditions());
        w.transition_half_life = 1.0;
        w.set(WeatherPreset::Overcast);
        // 20 half-lives is far past convergence.
        for _ in 0..20 {
            let t = damp(0.0, 1.0, 1.0, 1.0);
            w.current = w.current.lerp(w.target, t);
        }
        assert!(w.is_settled(), "{:?} vs {:?}", w.current, w.target);
    }
}
