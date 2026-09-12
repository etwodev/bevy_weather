//! Where the sun and moon are, and the lights that follow them.

use bevy::app::{App, Plugin, Update};
use bevy::color::{Color, LinearRgba, Mix};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::component::Component;
use bevy::ecs::query::{With, Without};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::light::{AmbientLight, DirectionalLight, SunDisk, VolumetricLight, light_consts::lux};
use bevy::math::Vec3;
use bevy::prelude::Entity;
use bevy::reflect::Reflect;
use bevy::transform::components::Transform;

use crate::config::{WeatherCamera, WeatherConfig};
use crate::state::Weather;
use crate::time::WeatherTime;
use crate::{WeatherSystems, math::remap01};
use bevy::ecs::reflect::ReflectComponent;
use bevy::ecs::reflect::ReflectResource;
use bevy::reflect::std_traits::ReflectDefault;

/// Mean lunar orbital inclination to the ecliptic, in degrees.
const LUNAR_INCLINATION_DEG: f32 = 5.145;

/// Draconic month: the period of the moon's declination wobble, in days.
const DRACONIC_MONTH_DAYS: f32 = 27.212_22;

/// Angular radius of the sun and moon as seen from Earth, in radians (~0.26°).
pub const DEFAULT_ANGULAR_RADIUS: f32 = 0.004_65;

/// How the sun's disc is drawn.
///
/// The disc itself is rendered by Bevy's atmosphere pass from a [`SunDisk`]
/// component, so this is really just a convenient place to configure it
/// alongside everything else.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct SunConfig {
    /// Draw a visible disc at all. With this off you still get sunlight and
    /// atmospheric scattering, just no disc.
    pub disk: bool,

    /// Angular *radius* of the disc, in radians.
    ///
    /// The real sun is `0.00465` -- the same half a degree as the moon, which
    /// is why a total eclipse works at all. As with the moon, the default is
    /// enlarged, because a physically-sized sun on a monitor is a speck.
    ///
    /// Note that Bevy's [`SunDisk`] is specified as a *diameter*; this is
    /// halved for you.
    pub angular_radius: f32,

    /// Brightness multiplier for the disc. `1.0` is physically correct.
    ///
    /// The sun's true radiance is several orders of magnitude past anything
    /// else on screen, so this mostly controls how far the glare bleeds once
    /// bloom gets hold of it.
    pub disk_intensity: f32,
}

impl Default for SunConfig {
    fn default() -> Self {
        Self {
            disk: true,
            angular_radius: 0.0105,
            disk_intensity: 1.0,
        }
    }
}

/// Marks the directional light the plugin steers as the sun.
///
/// Spawned automatically when [`WeatherConfig::celestial_lights`] is on. Add it
/// to your own light instead if you want to control its other properties.
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component, Default)]
pub struct SunLight;

/// Marks the directional light the plugin steers as the moon.
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component, Default)]
pub struct MoonLight;

/// Sun and moon geometry for the current frame, in world space.
///
/// Read this instead of recomputing: the sky shader, the fog, the particles and
/// the lights all share these values, so anything you derive from them stays in
/// step.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct CelestialBodies {
    /// Unit vector pointing *from the observer toward* the sun.
    pub sun_direction: Vec3,
    /// Unit vector pointing *from the observer toward* the moon.
    pub moon_direction: Vec3,
    /// Sine of the sun's altitude: `1.0` at the zenith, `0.0` on the horizon,
    /// negative below it.
    pub sun_altitude: f32,
    /// Sine of the moon's altitude.
    pub moon_altitude: f32,
    /// Moon phase in `[0, 1)`; `0.0` new, `0.5` full.
    pub moon_phase: f32,
    /// Lit fraction of the moon's disc, `0.0..=1.0`.
    pub moon_illumination: f32,
    /// `0.0` at night through `1.0` in full day, with a smooth twilight ramp.
    /// Handy for cross-fading your own day/night content.
    pub daylight: f32,
    /// Colour the sun is tinted to right now, reddening near the horizon.
    pub sun_color: LinearRgba,
    /// Angular radius of the sun's disc, in radians.
    pub sun_angular_radius: f32,
    /// Angular radius of the moon's disc, in radians.
    pub moon_angular_radius: f32,
}

impl Default for CelestialBodies {
    fn default() -> Self {
        Self {
            sun_direction: Vec3::Y,
            moon_direction: Vec3::NEG_Y,
            sun_altitude: 1.0,
            moon_altitude: -1.0,
            moon_phase: 0.5,
            moon_illumination: 1.0,
            daylight: 1.0,
            sun_color: LinearRgba::WHITE,
            sun_angular_radius: DEFAULT_ANGULAR_RADIUS,
            moon_angular_radius: DEFAULT_ANGULAR_RADIUS,
        }
    }
}

/// Converts an equatorial position to a world-space direction.
///
/// `hour_angle` is zero when the body crosses the meridian and advances with
/// the day; `declination` is its angle north of the celestial equator. The
/// result uses Bevy's convention of `+Y` up, `+X` east and `-Z` north.
pub fn equatorial_to_world(hour_angle: f32, declination: f32, latitude_rad: f32) -> Vec3 {
    let (sin_h, cos_h) = hour_angle.sin_cos();
    let (sin_d, cos_d) = declination.sin_cos();
    let (sin_lat, cos_lat) = latitude_rad.sin_cos();

    let east = -cos_d * sin_h;
    let up = sin_lat * sin_d + cos_lat * cos_d * cos_h;
    let north = cos_lat * sin_d - sin_lat * cos_d * cos_h;

    // `-Z` is north in Bevy's right-handed, Y-up world.
    Vec3::new(east, up, -north).normalize_or(Vec3::Y)
}

/// Computes [`CelestialBodies`] for a given clock. Pure, so it is easy to test
/// and to reuse outside the schedule.
pub fn compute_celestial(time: &WeatherTime) -> CelestialBodies {
    let latitude = time.latitude.to_radians();
    let sun_declination = time.solar_declination();
    // Hour angle is zero at solar noon (`time_of_day == 0.5`).
    let sun_hour_angle = (time.time_of_day - 0.5) * core::f32::consts::TAU;

    let sun_direction = equatorial_to_world(sun_hour_angle, sun_declination, latitude);

    let phase = time.moon_phase();
    // The moon trails the sun by a full turn over one synodic month, which is
    // exactly what makes a new moon rise with the sun and a full moon oppose it.
    let moon_hour_angle = sun_hour_angle - phase * core::f32::consts::TAU;
    // A full moon sits opposite the sun in declination too, which is why full
    // moons ride high in winter and low in summer.
    let mut moon_declination = sun_declination * (phase * core::f32::consts::TAU).cos();
    // The moon's orbit is tilted off the ecliptic; this wobble is what makes
    // eclipses rare.
    moon_declination += LUNAR_INCLINATION_DEG.to_radians()
        * (core::f32::consts::TAU * (time.elapsed_days() / DRACONIC_MONTH_DAYS as f64) as f32)
            .sin();

    let moon_direction = equatorial_to_world(moon_hour_angle, moon_declination, latitude);

    let sun_altitude = sun_direction.y;
    // Civil twilight is about -6 degrees; ramp across that band so the
    // transition is not a hard switch at the geometric horizon.
    let daylight = remap01(sun_altitude, -0.105, 0.10);

    CelestialBodies {
        sun_direction,
        moon_direction,
        sun_altitude,
        moon_altitude: moon_direction.y,
        moon_phase: phase,
        moon_illumination: time.moon_illumination(),
        daylight,
        sun_color: sun_tint(sun_altitude),
        sun_angular_radius: DEFAULT_ANGULAR_RADIUS,
        moon_angular_radius: DEFAULT_ANGULAR_RADIUS,
    }
}

/// Approximates the colour of direct sunlight at a given altitude.
///
/// This is a cheap stand-in for integrating transmittance along the view ray;
/// the atmosphere pass does the real thing for the sky, but the clouds and the
/// particles need a sun colour before that pass runs.
fn sun_tint(sun_altitude: f32) -> LinearRgba {
    // Air mass grows sharply as the sun approaches the horizon.
    let h = sun_altitude.max(0.0);
    let air_mass = 1.0 / (h + 0.05);
    // Rayleigh optical depth is roughly proportional to lambda^-4, so blue is
    // extinguished several times faster than red.
    let tau = Vec3::new(0.021, 0.052, 0.128) * air_mass;
    LinearRgba::rgb((-tau.x).exp(), (-tau.y).exp(), (-tau.z).exp())
}

/// Keeps [`CelestialBodies`] and the sun/moon lights in sync with the clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct CelestialPlugin;

impl Plugin for CelestialPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CelestialBodies>()
            .init_resource::<SunConfig>()
            .register_type::<CelestialBodies>()
            .register_type::<SunConfig>()
            .register_type::<SunLight>()
            .register_type::<MoonLight>()
            .add_systems(
                Update,
                (update_celestial_bodies, spawn_lights, drive_lights)
                    .chain()
                    .in_set(WeatherSystems::Apply),
            );
    }
}

fn update_celestial_bodies(mut bodies: ResMut<CelestialBodies>, time: Res<WeatherTime>) {
    *bodies = compute_celestial(&time);
}

fn spawn_lights(
    mut commands: Commands,
    config: Res<WeatherConfig>,
    sun_config: Res<SunConfig>,
    suns: Query<(), With<SunLight>>,
    moons: Query<(), With<MoonLight>>,
) {
    if !config.celestial_lights {
        return;
    }
    if suns.is_empty() {
        commands.spawn((
            SunLight,
            DirectionalLight {
                // `RAW_SUNLIGHT` is sunlight *before* the atmosphere filters it,
                // which is what Bevy's atmosphere expects as input.
                illuminance: lux::RAW_SUNLIGHT,
                shadow_maps_enabled: true,
                ..Default::default()
            },
            VolumetricLight,
            sun_disk(&sun_config),
            Transform::default(),
        ));
    }
    if moons.is_empty() {
        commands.spawn((
            MoonLight,
            DirectionalLight {
                illuminance: 0.0,
                shadow_maps_enabled: false,
                color: Color::srgb(0.72, 0.80, 1.0),
                ..Default::default()
            },
            VolumetricLight,
            // Not optional. Bevy's atmosphere draws a sun disc for every
            // directional light, and a light with no `SunDisk` component falls
            // back to `SunDisk::EARTH` -- so the moon light gets a second,
            // blazing white disc painted at the moon's exact position, sitting
            // on top of the real moon and hiding its phase completely. The
            // moon's own disc is drawn by the sky shader.
            SunDisk::OFF,
            Transform::default(),
        ));
    }
}

/// The [`SunDisk`] a [`SunConfig`] asks for.
fn sun_disk(config: &SunConfig) -> SunDisk {
    if !config.disk || config.disk_intensity <= 0.0 {
        return SunDisk::OFF;
    }
    SunDisk {
        // Bevy measures the disc as a diameter.
        angular_size: config.angular_radius.max(0.0) * 2.0,
        intensity: config.disk_intensity,
    }
}

/// Illuminance of a full moon, exaggerated well past the true ~0.3 lux so that
/// nights are legible at a daylight exposure.
const MOONLIGHT_ILLUMINANCE: f32 = 2_000.0;

/// Diffuse illuminance, in lux, added by a fully overcast sky in daylight.
///
/// Bevy's atmosphere drives image-based ambient light from the *clear* sky, so
/// it has no idea our cloud deck exists. Under heavy cloud almost all the light
/// reaching the ground is this diffuse bounce, and without it an overcast scene
/// collapses to black once the direct sun is attenuated.
const OVERCAST_AMBIENT_LUX: f32 = 13_000.0;

/// Diffuse illuminance, in lux, on a clear moonlit night.
///
/// Far above the real value. A true moonlit night is a fraction of a lux, which
/// at the daylight exposure this plugin targets is indistinguishable from
/// black; this is the conventional "day for night" lift.
const NIGHT_AMBIENT_LUX: f32 = 1_800.0;

/// The sun light, excluding the moon so the two can be queried together.
type SunQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Transform,
        &'static mut DirectionalLight,
        Option<&'static SunDisk>,
    ),
    (With<SunLight>, Without<MoonLight>),
>;

/// The moon light, excluding the sun.
type MoonQuery<'w, 's> = Query<
    'w,
    's,
    (&'static mut Transform, &'static mut DirectionalLight),
    (With<MoonLight>, Without<SunLight>),
>;

#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy system steering both lights and the ambient term"
)]
fn drive_lights(
    mut commands: Commands,
    bodies: Res<CelestialBodies>,
    config: Res<WeatherConfig>,
    sun_config: Res<SunConfig>,
    weather: Res<Weather>,
    cameras: Query<Entity, With<WeatherCamera>>,
    mut sun: SunQuery,
    mut moon: MoonQuery,
) {
    if !config.celestial_lights {
        return;
    }

    let cloudiness = weather.current.cloud_coverage * weather.current.cloud_density;
    // How much light survives the deck depends on how far down through it the
    // light has to come, not just on how much of the sky it covers. A seven
    // hundred metre stratus deck is a grey day; a four kilometre cumulonimbus
    // is dark enough to turn the streetlights on, and without this term the two
    // render identically.
    let depth = remap01(weather.current.cloud_thickness, 600.0, 4_000.0);
    let opacity = cloudiness * (0.55 + 0.45 * depth);
    // Thick cloud turns direct sunlight into diffuse skylight rather than
    // destroying it, so what it takes off the directional light is put back on
    // the ambient term below.
    let overcast = 1.0 - 0.95 * opacity;
    let scale = config.light_intensity_scale.max(0.0);

    let disk = sun_disk(&sun_config);
    for (entity, mut transform, mut light, existing_disk) in &mut sun {
        // A directional light shines along its local `-Z`, so it must look
        // *away* from the body it represents.
        if bodies.sun_direction.y > -0.999 {
            *transform = Transform::default().looking_to(-bodies.sun_direction, Vec3::Y);
        }
        // Fade out below the horizon rather than snapping, so shadows don't pop.
        let visibility = remap01(bodies.sun_altitude, -0.02, 0.06);
        light.illuminance = lux::RAW_SUNLIGHT * visibility * overcast * scale;
        light.color = Color::LinearRgba(bodies.sun_color);

        // Only rewrite when something actually changed, so a user adjusting
        // `SunDisk` by hand on their own light is not fought every frame.
        if existing_disk.is_none() || sun_config.is_changed() {
            commands.entity(entity).insert(disk.clone());
        }
    }

    for (mut transform, mut light) in &mut moon {
        if bodies.moon_direction.y > -0.999 {
            *transform = Transform::default().looking_to(-bodies.moon_direction, Vec3::Y);
        }
        let visibility = remap01(bodies.moon_altitude, -0.02, 0.06);
        // Moonlight only reads once the sun is out of the way.
        let night = 1.0 - bodies.daylight;
        light.illuminance = MOONLIGHT_ILLUMINANCE
            * bodies.moon_illumination
            * visibility
            * night
            * overcast
            * scale;
    }

    // The diffuse term the atmosphere's environment map cannot know about:
    // light bounced around by the cloud deck, plus a night-time floor.
    let night = 1.0 - bodies.daylight;
    // The same depth term: light that has been through four kilometres of
    // cloud arrives diffuse *and* greatly reduced.
    let overcast_ambient =
        OVERCAST_AMBIENT_LUX * cloudiness * bodies.daylight * (1.0 - 0.6 * depth);
    let night_ambient = NIGHT_AMBIENT_LUX * night * (0.25 + 0.75 * bodies.moon_illumination);
    let brightness = (overcast_ambient + night_ambient) * scale;
    // Overcast light is grey; night is blue, because what little there is has
    // been scattered by the atmosphere.
    let color = Color::LinearRgba(
        LinearRgba::rgb(0.85, 0.90, 1.0).mix(&LinearRgba::WHITE, bodies.daylight),
    );

    for entity in &cameras {
        commands.entity(entity).insert(AmbientLight {
            color,
            brightness,
            affects_lightmapped_meshes: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{DAYS_PER_YEAR, SYNODIC_MONTH_DAYS};

    fn at(hour: f32) -> WeatherTime {
        let mut t = WeatherTime {
            latitude: 45.0,
            axial_tilt: 0.0,
            year_offset: 0.0,
            moon_phase_offset: 0.0,
            day: 0,
            ..Default::default()
        };
        t.set_hour(hour);
        t
    }

    #[test]
    fn sun_is_highest_at_noon_and_lowest_at_midnight() {
        assert!(compute_celestial(&at(12.0)).sun_altitude > 0.7);
        assert!(compute_celestial(&at(0.0)).sun_altitude < -0.7);
    }

    #[test]
    fn sun_rises_in_the_east_and_sets_in_the_west() {
        // At zero declination the sun crosses the horizon due east/west.
        let dawn = compute_celestial(&at(6.0));
        assert!(dawn.sun_direction.x > 0.99, "{:?}", dawn.sun_direction);
        assert!(dawn.sun_altitude.abs() < 1e-3);

        let dusk = compute_celestial(&at(18.0));
        assert!(dusk.sun_direction.x < -0.99, "{:?}", dusk.sun_direction);
    }

    #[test]
    fn northern_noon_sun_sits_in_the_south() {
        // `-Z` is north, so a southern sun has positive Z.
        let noon = compute_celestial(&at(12.0));
        assert!(noon.sun_direction.z > 0.0, "{:?}", noon.sun_direction);
    }

    #[test]
    fn southern_hemisphere_noon_sun_sits_in_the_north() {
        let mut t = at(12.0);
        t.latitude = -45.0;
        let noon = compute_celestial(&t);
        assert!(noon.sun_direction.z < 0.0, "{:?}", noon.sun_direction);
    }

    #[test]
    fn directions_are_unit_length() {
        let mut t = WeatherTime::default();
        for _ in 0..500 {
            t.advance(0.0137);
            let b = compute_celestial(&t);
            assert!((b.sun_direction.length() - 1.0).abs() < 1e-4);
            assert!((b.moon_direction.length() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn new_moon_sits_near_the_sun_and_full_moon_opposes_it() {
        let mut t = at(12.0);

        t.moon_phase_offset = 0.0; // new
        let new = compute_celestial(&t);
        assert!(
            new.sun_direction.dot(new.moon_direction) > 0.95,
            "new moon should be close to the sun, got {}",
            new.sun_direction.dot(new.moon_direction)
        );

        t.moon_phase_offset = 0.5; // full
        let full = compute_celestial(&t);
        assert!(
            full.sun_direction.dot(full.moon_direction) < -0.95,
            "full moon should oppose the sun, got {}",
            full.sun_direction.dot(full.moon_direction)
        );
    }

    #[test]
    fn full_moon_is_up_at_midnight() {
        let mut t = at(0.0);
        t.moon_phase_offset = 0.5;
        assert!(compute_celestial(&t).moon_altitude > 0.5);
    }

    #[test]
    fn daylight_ramps_monotonically_through_the_morning() {
        let mut previous = -1.0;
        for i in 0..=48 {
            let hour = 4.0 + i as f32 * (8.0 / 48.0); // 04:00 -> 12:00
            let d = compute_celestial(&at(hour)).daylight;
            assert!(d >= previous - 1e-6, "daylight dipped at {hour}h");
            previous = d;
        }
        assert!(previous > 0.99);
    }

    #[test]
    fn sun_reddens_near_the_horizon() {
        let noon = sun_tint(1.0);
        let horizon = sun_tint(0.0);
        // Blue is knocked down far harder than red at low sun.
        assert!(horizon.blue / horizon.red < noon.blue / noon.red);
        assert!(horizon.blue < 0.2, "{}", horizon.blue);
    }

    #[test]
    fn summer_sun_climbs_higher_than_winter_sun() {
        let mut summer = at(12.0);
        summer.axial_tilt = 23.44;
        summer.year_offset = 172.0;

        let mut winter = at(12.0);
        winter.axial_tilt = 23.44;
        winter.year_offset = 172.0 + DAYS_PER_YEAR / 2.0;

        assert!(compute_celestial(&summer).sun_altitude > compute_celestial(&winter).sun_altitude);
    }

    #[test]
    fn moon_phase_completes_one_cycle_per_synodic_month() {
        let mut t = at(0.0);
        t.moon_phase_offset = 0.0;
        let start = compute_celestial(&t).moon_phase;
        t.advance(SYNODIC_MONTH_DAYS);
        let end = compute_celestial(&t).moon_phase;
        assert!((start - end).abs() < 1e-3, "{start} vs {end}");
    }
}
