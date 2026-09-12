//! Volumetric cloud tuning.
//!
//! The clouds themselves are raymarched in the sky dome shader
//! ([`crate::sky`]); this is the knob box. Shape comes from
//! [`WeatherConditions`](crate::state::WeatherConditions) — coverage, density,
//! altitude and thickness are weather, not configuration — while everything
//! here is art direction and performance.

use bevy::color::{Color, LinearRgba};
use bevy::ecs::reflect::ReflectResource;
use bevy::ecs::resource::Resource;
use bevy::reflect::Reflect;
use bevy::reflect::std_traits::ReflectDefault;

/// Look and cost of the volumetric cloud layer.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct CloudConfig {
    /// Raymarch steps through the cloud slab. `None` follows
    /// [`WeatherConfig::quality`](crate::config::WeatherConfig::quality).
    pub steps: Option<u32>,

    /// Steps taken toward the sun per sample to estimate self-shadowing.
    /// `0` swaps in a cheap analytic approximation. `None` follows quality.
    pub light_steps: Option<u32>,

    /// Size of the base cloud shape, in metres, on every axis.
    ///
    /// Roughly the width of one cumulus. Much above a few kilometres and the
    /// whole overhead sky falls inside a single noise cell, which reads as
    /// "either totally clear or totally covered" rather than as weather.
    pub shape_scale: f32,

    /// Size of the erosion detail carved out of the cloud edges, in metres.
    pub detail_scale: f32,

    /// How hard the detail noise bites into the cloud edges, `0.0..=1.0`.
    pub detail_strength: f32,

    /// Extinction per metre at full density. Higher is darker and more opaque.
    pub extinction: f32,

    /// Forward-scattering asymmetry of the Henyey–Greenstein phase function,
    /// `-1.0..=1.0`. Positive values give the bright silver lining you see
    /// looking toward the sun.
    pub forward_scattering: f32,

    /// Back-scattering lobe, blended with the forward one. Keeps clouds from
    /// going flat and dark when the sun is behind you.
    pub back_scattering: f32,

    /// Strength of the "powder" effect that darkens cloud interiors, giving
    /// the crisp look of sunlit cumulus. `0.0..=1.0`.
    pub powder: f32,

    /// Ambient light the clouds pick up from the sky, multiplied into the base
    /// colour.
    pub ambient: f32,

    /// Overall brightness multiplier for the clouds.
    pub exposure: f32,

    /// Tint of the cloud body.
    pub albedo: Color,

    /// How fast clouds evolve relative to the wind, in "shape changes per
    /// second". Zero makes them rigid shapes that only translate.
    pub evolution_rate: f32,

    /// Multiplies wind speed for cloud advection. Clouds at altitude move
    /// faster than surface wind, so this defaults above `1.0`.
    pub wind_multiplier: f32,

    /// Angular radius of the horizon fade, as a fraction of the sky. Stops the
    /// slab intersection producing a hard line at the horizon.
    pub horizon_fade: f32,

    /// How much a lightning flash lights the cloud from inside.
    pub lightning_response: f32,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            steps: None,
            light_steps: None,
            shape_scale: 1_500.0,
            detail_scale: 320.0,
            detail_strength: 0.35,
            extinction: 0.045,
            forward_scattering: 0.8,
            back_scattering: -0.25,
            powder: 0.6,
            ambient: 0.35,
            exposure: 1.0,
            albedo: Color::srgb(1.0, 1.0, 1.0),
            evolution_rate: 0.006,
            wind_multiplier: 2.5,
            horizon_fade: 0.06,
            lightning_response: 1.0,
        }
    }
}

impl CloudConfig {
    /// Cloud albedo in linear space, ready for the shader.
    pub fn albedo_linear(&self) -> LinearRgba {
        self.albedo.to_linear()
    }
}
