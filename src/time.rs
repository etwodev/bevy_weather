//! The clock everything else is a function of.

use bevy::app::{App, Plugin, Update};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Res, ResMut};
use bevy::reflect::Reflect;
use bevy::time::Time;

use crate::WeatherSystems;
use bevy::ecs::reflect::ReflectResource;
use bevy::reflect::std_traits::ReflectDefault;

/// Length of the synodic (new-moon to new-moon) month, in days.
pub const SYNODIC_MONTH_DAYS: f32 = 29.530_588;

/// Days in a tropical year.
pub const DAYS_PER_YEAR: f32 = 365.242_2;

/// The in-game clock: time of day, calendar day, and how fast they advance.
///
/// The whole plugin is a pure function of this resource plus [`Weather`]. That
/// means you can scrub it, pause it, network it, or drive it from your own
/// clock and everything stays consistent.
///
/// [`Weather`]: crate::state::Weather
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct WeatherTime {
    /// Time of day in `[0, 1)`. `0.0` is local midnight, `0.25` sunrise-ish,
    /// `0.5` solar noon, `0.75` sunset-ish.
    ///
    /// This is the "time progression ratio": set it directly (with
    /// [`paused`](Self::paused) on) to drive the entire sky and weather system
    /// from your own timeline.
    pub time_of_day: f32,

    /// Whole days elapsed since the epoch. Drives seasons and moon phase.
    pub day: u32,

    /// Real seconds per in-game day. `120.0` gives a brisk two-minute cycle;
    /// `86_400.0` runs in real time.
    pub day_length_secs: f32,

    /// When true, [`time_of_day`](Self::time_of_day) and [`day`](Self::day) are
    /// left entirely to you.
    pub paused: bool,

    /// Observer latitude in degrees, `-90..=90`. Controls how steeply the sun
    /// arcs and how extreme the seasons are.
    pub latitude: f32,

    /// Planet axial tilt in degrees. Earth is `23.44`; `0.0` removes seasons.
    pub axial_tilt: f32,

    /// Day of the year (`0.0..DAYS_PER_YEAR`) that [`day`] `0` corresponds to.
    /// Use it to start the game in a particular season.
    ///
    /// [`day`]: Self::day
    pub year_offset: f32,

    /// Moon phase at [`day`] `0`, in `[0, 1)`. `0.0` is a new moon, `0.5` full.
    ///
    /// [`day`]: Self::day
    pub moon_phase_offset: f32,
}

impl Default for WeatherTime {
    fn default() -> Self {
        Self {
            time_of_day: 0.30,
            day: 0,
            day_length_secs: 300.0,
            paused: false,
            latitude: 45.0,
            axial_tilt: 23.44,
            year_offset: 172.0, // northern summer solstice
            moon_phase_offset: 0.5,
        }
    }
}

impl WeatherTime {
    /// Total elapsed days as a continuous value, `day + time_of_day`.
    ///
    /// `f64` because at long day counts an `f32` can no longer resolve a single
    /// minute, which would make the procedural weather stutter.
    #[inline]
    pub fn elapsed_days(&self) -> f64 {
        self.day as f64 + self.time_of_day as f64
    }

    /// Advances the clock by `days`, rolling [`time_of_day`] into [`day`].
    ///
    /// Negative values run the clock backwards.
    ///
    /// [`time_of_day`]: Self::time_of_day
    /// [`day`]: Self::day
    pub fn advance(&mut self, days: f32) {
        let t = self.time_of_day + days;
        let whole = t.floor();
        self.time_of_day = t - whole;
        // Saturating so running the clock backwards past the epoch parks at
        // day 0 rather than wrapping to ~4 billion.
        if whole >= 0.0 {
            self.day = self.day.saturating_add(whole as u32);
        } else {
            self.day = self.day.saturating_sub((-whole) as u32);
        }
    }

    /// Sets the clock from an hour in `[0, 24)`.
    pub fn set_hour(&mut self, hour: f32) {
        self.time_of_day = (hour / 24.0).rem_euclid(1.0);
    }

    /// Time of day expressed as an hour in `[0, 24)`.
    #[inline]
    pub fn hour(&self) -> f32 {
        self.time_of_day * 24.0
    }

    /// Position in the year, `[0, 1)`. `0.0` is the vernal equinox reference
    /// point used by [`solar_declination`](Self::solar_declination).
    #[inline]
    pub fn year_fraction(&self) -> f32 {
        ((self.day as f32 + self.year_offset) / DAYS_PER_YEAR).rem_euclid(1.0)
    }

    /// Sun declination in radians: how far north or south of the equator the
    /// sun is directly overhead today.
    pub fn solar_declination(&self) -> f32 {
        let tilt = self.axial_tilt.to_radians();
        // Day 0 of the year is ~10 days after the December solstice.
        let angle = core::f32::consts::TAU * (self.year_fraction() + 10.0 / DAYS_PER_YEAR);
        -tilt * angle.cos()
    }

    /// Moon phase in `[0, 1)`: `0.0` new, `0.25` first quarter, `0.5` full,
    /// `0.75` last quarter.
    pub fn moon_phase(&self) -> f32 {
        let d = self.elapsed_days() + self.moon_phase_offset as f64 * SYNODIC_MONTH_DAYS as f64;
        (d / SYNODIC_MONTH_DAYS as f64).rem_euclid(1.0) as f32
    }

    /// Fraction of the moon's disc that is lit, `0.0` new to `1.0` full.
    pub fn moon_illumination(&self) -> f32 {
        0.5 * (1.0 - (core::f32::consts::TAU * self.moon_phase()).cos())
    }
}

/// Advances [`WeatherTime`] from Bevy's [`Time`].
#[derive(Debug, Clone, Copy, Default)]
pub struct WeatherTimePlugin;

impl Plugin for WeatherTimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeatherTime>()
            .register_type::<WeatherTime>()
            .add_systems(Update, tick_weather_time.in_set(WeatherSystems::Tick));
    }
}

fn tick_weather_time(mut weather_time: ResMut<WeatherTime>, time: Res<Time>) {
    if weather_time.paused || weather_time.day_length_secs <= 0.0 {
        return;
    }
    let days = time.delta_secs() / weather_time.day_length_secs;
    weather_time.advance(days);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_rolls_over_into_days() {
        let mut t = WeatherTime {
            time_of_day: 0.9,
            day: 3,
            ..Default::default()
        };
        t.advance(0.2);
        assert_eq!(t.day, 4);
        assert!((t.time_of_day - 0.1).abs() < 1e-6, "{}", t.time_of_day);
    }

    #[test]
    fn advance_backwards_rolls_days_down() {
        let mut t = WeatherTime {
            time_of_day: 0.1,
            day: 3,
            ..Default::default()
        };
        t.advance(-0.2);
        assert_eq!(t.day, 2);
        assert!((t.time_of_day - 0.9).abs() < 1e-5, "{}", t.time_of_day);
    }

    #[test]
    fn advance_backwards_past_epoch_saturates() {
        let mut t = WeatherTime {
            time_of_day: 0.1,
            day: 0,
            ..Default::default()
        };
        t.advance(-5.0);
        assert_eq!(t.day, 0);
    }

    #[test]
    fn time_of_day_stays_in_unit_range() {
        let mut t = WeatherTime::default();
        for _ in 0..1_000 {
            t.advance(0.037);
            assert!((0.0..1.0).contains(&t.time_of_day));
        }
    }

    #[test]
    fn moon_illumination_tracks_phase() {
        let mut t = WeatherTime {
            day: 0,
            time_of_day: 0.0,
            moon_phase_offset: 0.0,
            ..Default::default()
        };
        assert!(t.moon_illumination() < 1e-4, "new moon should be dark");

        t.day = (SYNODIC_MONTH_DAYS / 2.0).round() as u32;
        assert!(
            t.moon_illumination() > 0.99,
            "half a synodic month later should be full, got {}",
            t.moon_illumination()
        );
    }

    #[test]
    fn declination_peaks_at_the_solstices() {
        // year_offset 172 is the northern summer solstice: declination ~ +tilt.
        let summer = WeatherTime {
            year_offset: 172.0,
            ..Default::default()
        };
        let winter = WeatherTime {
            year_offset: 172.0 + DAYS_PER_YEAR / 2.0,
            ..Default::default()
        };
        let tilt = 23.44f32.to_radians();
        assert!(
            (summer.solar_declination() - tilt).abs() < 0.05,
            "{}",
            summer.solar_declination()
        );
        assert!(
            (winter.solar_declination() + tilt).abs() < 0.05,
            "{}",
            winter.solar_declination()
        );
    }

    #[test]
    fn zero_tilt_removes_seasons() {
        for day in [0u32, 40, 90, 200, 300] {
            let t = WeatherTime {
                axial_tilt: 0.0,
                day,
                ..Default::default()
            };
            assert!(t.solar_declination().abs() < 1e-6);
        }
    }
}
