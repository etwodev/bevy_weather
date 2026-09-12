//! Wires up Bevy's physically-based atmosphere for weather cameras.
//!
//! Bevy 0.19 ships a Bruneton-style precomputed atmosphere, so this module
//! does not reimplement scattering — it just spawns the planet, attaches the
//! right components to the cameras you marked, and scales quality.

use bevy::app::{App, Plugin, Startup, Update};
use bevy::asset::Assets;
use bevy::camera::Exposure;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::ecs::query::{With, Without};
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::ecs::world::Ref;
use bevy::light::atmosphere::ScatteringMedium;
use bevy::light::{Atmosphere, AtmosphereEnvironmentMapLight};
use bevy::pbr::{AtmosphereMode, AtmosphereSettings};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::Entity;
use bevy::reflect::Reflect;

use crate::WeatherSystems;
use crate::config::{Quality, WeatherCamera, WeatherConfig};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::reflect::ReflectComponent;
use bevy::ecs::reflect::ReflectResource;
use bevy::reflect::std_traits::ReflectDefault;

/// Tuning for the atmosphere pass.
// No `Debug`: Bevy's `Bloom` does not implement it.
#[derive(Resource, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct AtmosphereConfig {
    /// Pin this subsystem to its own quality tier, overriding
    /// [`WeatherConfig::quality`](crate::config::WeatherConfig::quality).
    ///
    /// For a settings menu that exposes sky and atmosphere separately from everything
    /// else. `None` follows the global dial. Explicit counts on this struct
    /// still win over both.
    pub quality: Option<Quality>,

    /// Spawn a planet entity with an Earth-like [`Atmosphere`] on startup.
    ///
    /// Turn this off if you want to spawn your own — a different planet radius,
    /// a Martian medium, or several to move between.
    pub spawn_planet: bool,

    /// Multiplies the density of every scattering term.
    ///
    /// Above `1.0` the sky is hazier and sunsets are deeper; below `1.0` it
    /// thins toward the near-vacuum look of a high-altitude or alien sky.
    pub density_multiplier: f32,

    /// Use full raymarching instead of the precomputed sky-view LUT.
    ///
    /// Much more expensive, but correct when the camera moves through the
    /// atmosphere vertically at speed. Off by default.
    pub raymarched: bool,

    /// Drive image-based ambient lighting and reflections from the sky.
    ///
    /// This is what stops objects going flat black in shadow, and what makes
    /// them pick up the orange of a sunset.
    pub environment_light: bool,

    /// Edge length of that environment cubemap, or `None` to follow
    /// [`WeatherConfig::quality`](crate::config::WeatherConfig::quality).
    ///
    /// Bevy regenerates it every frame and defaults to 512, which is far more
    /// than low-frequency ambient light needs.
    pub environment_map_size: Option<u32>,

    /// Set [`Exposure`] on weather cameras to this EV100.
    ///
    /// The atmosphere is calibrated in real physical units, so an unadjusted
    /// camera renders it blown out. `None` leaves exposure alone.
    pub exposure_ev100: Option<f32>,

    /// Set [`Tonemapping`] on weather cameras. `None` leaves it alone.
    pub tonemapping: Option<Tonemapping>,

    /// Add [`Bloom`] to weather cameras. `None` leaves bloom alone.
    ///
    /// This is not decoration. The sun's disc is several orders of magnitude
    /// brighter than anything else in the frame, and without somewhere for that
    /// energy to bleed it tonemaps to a flat white circle with a hard edge --
    /// which is exactly what a real sun does not look like. Bloom is what turns
    /// it back into glare. It also gives the moon and lightning their halos.
    ///
    /// Bloom requires an HDR camera, which the component pulls in for you.
    pub bloom: Option<Bloom>,

    /// Farthest distance, in metres, at which aerial perspective is evaluated.
    pub aerial_view_max_distance: f32,
}

impl Default for AtmosphereConfig {
    fn default() -> Self {
        Self {
            quality: None,
            spawn_planet: true,
            density_multiplier: 1.0,
            raymarched: false,
            environment_light: true,
            environment_map_size: None,
            exposure_ev100: Some(13.0),
            tonemapping: Some(Tonemapping::AcesFitted),
            bloom: Some(Bloom {
                // A little above `NATURAL`. The sun is the one thing in the
                // frame that is genuinely blinding, and it needs enough bleed
                // to read as glare rather than as a white sticker on the sky.
                intensity: 0.24,
                ..Bloom::NATURAL
            }),
            aerial_view_max_distance: 32_000.0,
        }
    }
}

impl AtmosphereConfig {
    /// The quality tier the atmosphere runs at, resolving
    /// [`quality`](Self::quality) against the global dial.
    pub fn quality(&self, global: Quality) -> Quality {
        self.quality.unwrap_or(global)
    }
}

/// Marks the planet entity this plugin spawned, so it can be found again.
#[derive(bevy::ecs::component::Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component, Default)]
pub struct WeatherPlanet;

/// Spawns the planet and keeps weather cameras configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct WeatherAtmospherePlugin;

impl Plugin for WeatherAtmospherePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AtmosphereConfig>()
            .register_type::<AtmosphereConfig>()
            .register_type::<WeatherPlanet>()
            .add_systems(Startup, spawn_planet)
            .add_systems(
                Update,
                (sync_planet, configure_cameras).in_set(WeatherSystems::Apply),
            );
    }
}

fn spawn_planet(
    mut commands: Commands,
    config: Res<WeatherConfig>,
    atmosphere: Res<AtmosphereConfig>,
    mut media: ResMut<Assets<ScatteringMedium>>,
) {
    if !config.atmosphere || !atmosphere.spawn_planet {
        return;
    }
    let medium = media.add(
        ScatteringMedium::earth(256, 256)
            .with_density_multiplier(atmosphere.density_multiplier.max(0.0)),
    );
    let mut planet = Atmosphere::earth(medium);
    // `units_per_meter` lets a scene authored in centimetres (or kilometres)
    // still get a correctly-scaled sky.
    let scale = config.units_per_meter.max(1e-6);
    planet.inner_radius *= scale;
    planet.outer_radius *= scale;
    commands.spawn((WeatherPlanet, planet));
}

/// Rebuilds the scattering medium when the density multiplier changes.
fn sync_planet(
    atmosphere: Res<AtmosphereConfig>,
    mut media: ResMut<Assets<ScatteringMedium>>,
    planets: Query<&Atmosphere, With<WeatherPlanet>>,
) {
    if !atmosphere.is_changed() {
        return;
    }
    for planet in &planets {
        let rebuilt = ScatteringMedium::earth(256, 256)
            .with_density_multiplier(atmosphere.density_multiplier.max(0.0));
        if let Some(mut slot) = media.get_mut(&planet.medium) {
            *slot = rebuilt;
        }
    }
}

fn configure_cameras(
    mut commands: Commands,
    config: Res<WeatherConfig>,
    atmosphere: Res<AtmosphereConfig>,
    cameras: Query<(Entity, Option<Ref<AtmosphereSettings>>), With<WeatherCamera>>,
    without_marker: Query<Entity, (With<AtmosphereSettings>, Without<WeatherCamera>)>,
) {
    let _ = &without_marker; // cameras we did not mark are none of our business

    for (entity, existing) in &cameras {
        if !config.atmosphere {
            commands
                .entity(entity)
                .remove::<AtmosphereSettings>()
                .remove::<AtmosphereEnvironmentMapLight>();
            continue;
        }

        // Only rewrite when something we own actually changed, so a user
        // tweaking sample counts by hand isn't fought every frame.
        let needs_write = existing.is_none() || config.is_changed() || atmosphere.is_changed();
        if !needs_write {
            continue;
        }

        let mut settings = AtmosphereSettings {
            rendering_method: if atmosphere.raymarched {
                AtmosphereMode::Raymarched
            } else {
                AtmosphereMode::LookupTexture
            },
            aerial_view_lut_max_distance: atmosphere.aerial_view_max_distance,
            ..Default::default()
        };
        apply_quality(&mut settings, atmosphere.quality(config.quality));

        let mut entity_commands = commands.entity(entity);
        entity_commands.insert(settings);

        if atmosphere.environment_light {
            let size = atmosphere
                .environment_map_size
                .unwrap_or_else(|| atmosphere.quality(config.quality).environment_map_size())
                .clamp(16, 2048)
                .next_power_of_two();
            entity_commands.insert(AtmosphereEnvironmentMapLight {
                size: bevy::math::UVec2::splat(size),
                ..Default::default()
            });
        } else {
            entity_commands.remove::<AtmosphereEnvironmentMapLight>();
        }
        if let Some(ev100) = atmosphere.exposure_ev100 {
            entity_commands.insert(Exposure { ev100 });
        }
        if let Some(tonemapping) = atmosphere.tonemapping {
            entity_commands.insert(tonemapping);
        }
        if let Some(bloom) = atmosphere.bloom.clone() {
            entity_commands.insert(bloom);
        }
    }
}

fn apply_quality(settings: &mut AtmosphereSettings, quality: Quality) {
    use bevy::math::{UVec2, UVec3};
    match quality {
        Quality::Potato => {
            settings.transmittance_lut_size = UVec2::new(64, 32);
            settings.transmittance_lut_samples = 20;
            settings.multiscattering_lut_size = UVec2::new(16, 16);
            settings.multiscattering_lut_dirs = 32;
            settings.multiscattering_lut_samples = 10;
            settings.sky_view_lut_size = UVec2::new(128, 64);
            settings.sky_view_lut_samples = 6;
            settings.aerial_view_lut_size = UVec3::new(16, 16, 8);
            settings.aerial_view_lut_samples = 4;
            settings.sky_max_samples = 6;
        }
        Quality::Low => {
            settings.transmittance_lut_size = UVec2::new(128, 64);
            settings.sky_view_lut_size = UVec2::new(200, 100);
            settings.sky_view_lut_samples = 8;
            settings.aerial_view_lut_size = UVec3::new(16, 16, 16);
            settings.aerial_view_lut_samples = 6;
            settings.sky_max_samples = 8;
        }
        Quality::Medium => {}
        Quality::High => {
            settings.sky_view_lut_size = UVec2::new(512, 256);
            settings.sky_view_lut_samples = 24;
            settings.aerial_view_lut_size = UVec3::new(48, 48, 48);
            settings.aerial_view_lut_samples = 16;
            settings.sky_max_samples = 24;
        }
        Quality::Ultra => {
            settings.transmittance_lut_size = UVec2::new(512, 256);
            settings.transmittance_lut_samples = 64;
            settings.multiscattering_lut_size = UVec2::new(64, 64);
            settings.sky_view_lut_size = UVec2::new(800, 400);
            settings.sky_view_lut_samples = 40;
            settings.aerial_view_lut_size = UVec3::new(64, 64, 64);
            settings.aerial_view_lut_samples = 24;
            settings.sky_max_samples = 48;
        }
    }
}
