//! Master switches and quality settings.

use bevy::ecs::component::Component;
use bevy::ecs::reflect::ReflectComponent;
use bevy::ecs::reflect::ReflectResource;
use bevy::ecs::resource::Resource;
use bevy::reflect::Reflect;
use bevy::reflect::std_traits::ReflectDefault;

/// Marks the camera(s) that weather should be rendered for.
///
/// The plugin inserts the components each enabled subsystem needs
/// (`AtmosphereSettings`, `VolumetricFog`, `DistanceFog`, …) onto every entity
/// with this marker, and keeps them in sync with [`WeatherConfig`]. Cameras
/// without it are left completely untouched, which is what you want for UI,
/// minimap or render-to-texture cameras.
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component, Default)]
pub struct WeatherCamera;

/// A coarse quality dial that picks sample counts for the expensive effects.
///
/// Every individual count is still overridable on the specific config struct;
/// this only sets the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Reflect)]
pub enum Quality {
    /// Cheap enough for integrated GPUs. Few cloud samples, no light march.
    Low,
    /// Sensible default.
    #[default]
    Medium,
    /// Doubles cloud raymarch steps and particle counts.
    High,
    /// For screenshots and cutscenes.
    Ultra,
}

impl Quality {
    /// Primary raymarch steps through the cloud layer.
    pub fn cloud_steps(self) -> u32 {
        match self {
            Quality::Low => 24,
            Quality::Medium => 48,
            Quality::High => 96,
            Quality::Ultra => 160,
        }
    }

    /// Steps taken toward the sun per primary sample, to estimate self-shadowing.
    /// Zero means "use the cheap analytic approximation instead".
    pub fn cloud_light_steps(self) -> u32 {
        match self {
            Quality::Low => 0,
            Quality::Medium => 4,
            Quality::High => 6,
            Quality::Ultra => 8,
        }
    }

    /// Number of precipitation particles at full intensity.
    pub fn particle_count(self) -> u32 {
        match self {
            Quality::Low => 4_000,
            Quality::Medium => 12_000,
            Quality::High => 30_000,
            Quality::Ultra => 60_000,
        }
    }

    /// Raymarch steps for Bevy's volumetric fog.
    pub fn fog_steps(self) -> u32 {
        match self {
            Quality::Low => 24,
            Quality::Medium => 48,
            Quality::High => 80,
            Quality::Ultra => 128,
        }
    }
}

/// Top-level configuration: which subsystems run, and how hard they work.
///
/// This is a [`Resource`]; mutate it at runtime and everything reacts on the
/// next frame.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct WeatherConfig {
    /// Global quality dial. Changing this does *not* retroactively overwrite
    /// counts you set by hand on the individual config resources.
    pub quality: Quality,

    /// Render the physically-based atmosphere (sky colour, aerial perspective).
    pub atmosphere: bool,
    /// Render the star field, galaxy, moon and clouds sky dome.
    pub sky: bool,
    /// Render volumetric clouds in the sky dome.
    pub clouds: bool,
    /// Drive Bevy's volumetric fog / god rays.
    pub volumetric_fog: bool,
    /// Drive the cheap per-pixel `DistanceFog` as well.
    ///
    /// On by default, and doing most of the visible work: `DistanceFog` blends
    /// toward a colour, which is what makes heavy fog a white-out rather than
    /// a black-out. The volumetric pass supplies the god rays on top.
    pub distance_fog: bool,
    /// Simulate and draw rain and snow.
    pub precipitation: bool,
    /// Lightning flashes and thunder.
    pub thunder: bool,
    /// Spawn and steer the sun and moon directional lights.
    pub celestial_lights: bool,

    /// Multiplies every light this plugin creates. Handy when your scene is
    /// authored against a different exposure.
    pub light_intensity_scale: f32,

    /// World units per metre. Bevy's atmosphere is calibrated in metres; if
    /// your game is authored at, say, 1 unit = 1 cm, set this to `100.0`.
    pub units_per_meter: f32,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            quality: Quality::Medium,
            atmosphere: true,
            sky: true,
            clouds: true,
            volumetric_fog: true,
            distance_fog: true,
            precipitation: true,
            thunder: true,
            celestial_lights: true,
            light_intensity_scale: 1.0,
            units_per_meter: 1.0,
        }
    }
}
