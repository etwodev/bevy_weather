//! The sky dome: stars, the galaxy, the moon and volumetric clouds.
//!
//! # How this fits together with Bevy's atmosphere
//!
//! Bevy 0.19 renders its physically-based atmosphere as a full-screen pass
//! between the opaque and transparent passes, compositing as
//! `destination * transmittance + inscattering`.
//!
//! This module draws deep space *into the opaque pass*, at the far plane, with
//! depth writes disabled. The atmosphere pass then runs over the top. The
//! result is that the sky is extinguished by exactly as much air as physics
//! says it should be, and drowned out by exactly as much daylight scattering:
//! stars appear at dusk and disappear at dawn with no fade parameter anywhere.
//!
//! The whole sky is one full-screen triangle with no model transform, so a
//! single entity serves every camera in the scene.

use bevy::app::{App, Plugin, Startup, Update};
use bevy::asset::RenderAssetUsages;
use bevy::asset::{Asset, Assets, embedded_asset};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::color::{Color, ColorToComponents};
use bevy::ecs::component::Component;
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::material::AlphaMode;
use bevy::math::{Mat3, Vec3, Vec4};
use bevy::mesh::{Indices, Mesh, Mesh3d, PrimitiveTopology};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin, MeshMaterial3d};
use bevy::reflect::{Reflect, TypePath};
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, CompareFunction, RenderPipelineDescriptor, ShaderType,
    SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use bevy::time::Time;
use bevy::transform::components::Transform;

use crate::WeatherSystems;
use crate::celestial::CelestialBodies;
use crate::clouds::CloudConfig;
use crate::config::WeatherConfig;
use crate::fog::FogConfig;
use crate::state::Weather;
use crate::thunder::LightningState;
use crate::time::{DAYS_PER_YEAR, WeatherTime};
use crate::wind::Wind;
use bevy::ecs::reflect::ReflectComponent;
use bevy::ecs::reflect::ReflectResource;
use bevy::reflect::std_traits::ReflectDefault;

const SHADER_PATH: &str = "embedded://bevy_weather/sky/sky.wgsl";

/// Mean planetary radius in metres, used for the curvature of the cloud layer.
pub const EARTH_RADIUS_M: f32 = 6_371_000.0;

/// Night-sky values are scaled by this before exposure so that the defaults
/// look right at the `ev100 = 13` daylight exposure the atmosphere wants.
///
/// Point stars have no meaningful physical radiance at raster resolution -- the
/// value depends entirely on the solid angle of a pixel -- so some exaggeration
/// is unavoidable no matter how the rest is authored.
const NIGHT_SCALE: f32 = 3_000.0;

const FLAG_STARS: u32 = 1;
const FLAG_GALAXY: u32 = 2;
const FLAG_MOON: u32 = 4;
const FLAG_CLOUDS: u32 = 8;

/// Procedural star field settings.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct StarConfig {
    /// Draw stars at all.
    pub enabled: bool,
    /// Grid cells per cube face. Higher means more, smaller stars.
    pub density: f32,
    /// Overall brightness multiplier.
    pub brightness: f32,
    /// Fraction of cells that actually contain a star, `0.0..=1.0`.
    pub occupancy: f32,
    /// Apparent size of a star, as a fraction of a grid cell.
    pub size: f32,
    /// How strongly stars are tinted by temperature, `0.0..=1.0`.
    pub color_variation: f32,
    /// Twinkle rate in radians per second.
    pub twinkle_speed: f32,
    /// Twinkle depth, `0.0..=1.0`. Applied most strongly near the horizon,
    /// where you are looking through the most atmosphere.
    pub twinkle_amount: f32,
}

impl Default for StarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            density: 180.0,
            brightness: 2.2,
            occupancy: 0.35,
            size: 0.055,
            color_variation: 0.6,
            twinkle_speed: 2.4,
            twinkle_amount: 0.45,
        }
    }
}

/// Procedural Milky Way settings.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct GalaxyConfig {
    /// Draw the galaxy at all.
    pub enabled: bool,
    /// Overall brightness multiplier.
    pub brightness: f32,
    /// Frequency of the unresolved-starlight noise.
    pub noise_scale: f32,
    /// How dark the dust lanes cut, `0.0..=1.0`.
    pub dust: f32,
    /// How tightly the band hugs the galactic plane. Higher is narrower.
    pub band_tightness: f32,
    /// How concentrated the central bulge is. Higher is tighter.
    pub core_concentration: f32,
    /// Colour of the galactic core.
    pub core_color: Color,
    /// Colour of the outer arms.
    pub edge_color: Color,
    /// Galactic pole, in the fixed star frame. The default approximates the
    /// real one, which is why the band sits at a realistic angle to the
    /// celestial equator.
    pub pole: Vec3,
    /// Direction of the galactic centre, in the fixed star frame.
    pub center: Vec3,
}

impl Default for GalaxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            brightness: 0.55,
            noise_scale: 26.0,
            dust: 0.75,
            band_tightness: 9.0,
            core_concentration: 3.0,
            core_color: Color::srgb(1.0, 0.92, 0.76),
            edge_color: Color::srgb(0.62, 0.72, 1.0),
            // North galactic pole, roughly RA 12h51m / dec +27.1 degrees.
            pole: Vec3::new(-0.874, -0.484, 0.460),
            // Sagittarius A*, roughly RA 17h46m / dec -29.0 degrees.
            center: Vec3::new(-0.055, -0.873, -0.484),
        }
    }
}

/// Moon rendering settings. Its position and phase come from
/// [`WeatherTime`], not from here.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct MoonConfig {
    /// Draw the moon at all.
    pub enabled: bool,
    /// Brightness of the lit surface.
    pub brightness: f32,
    /// Apparent radius in radians. The real moon is about `0.00465`.
    pub angular_radius: f32,
    /// Strength of earthshine on the unlit part of the disc, `0.0..=1.0`.
    /// This is what makes "the old moon in the new moon's arms".
    pub earthshine: f32,
    /// Contrast of the procedural maria and craters, `0.0..=1.0`.
    pub surface_detail: f32,
    /// Tint of the moon's surface.
    pub tint: Color,
}

impl Default for MoonConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            brightness: 10.0,
            angular_radius: crate::celestial::DEFAULT_ANGULAR_RADIUS,
            earthshine: 0.12,
            surface_detail: 0.7,
            tint: Color::srgb(0.96, 0.95, 0.90),
        }
    }
}

/// Marks the sky entity this plugin spawns.
#[derive(Component, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Component, Default)]
pub struct SkyDome;

/// Everything the sky shader needs, in one uniform.
///
/// Field order and types must match `SkyUniform` in `sky.wgsl` exactly.
#[derive(ShaderType, Debug, Clone)]
pub struct SkyUniform {
    /// `xyz`: direction to the sun. `w`: its angular radius.
    pub sun_direction: Vec4,
    /// `xyz`: direction to the moon. `w`: its angular radius.
    pub moon_direction: Vec4,
    /// `rgb`: sun colour. `a`: daylight factor.
    pub sun_color: Vec4,
    /// `rgb`: moonlight colour. `a`: lit fraction.
    pub moon_color: Vec4,
    /// `x`: density. `y`: brightness. `z`: twinkle speed. `w`: size.
    pub star_params: Vec4,
    /// `x`: colour spread. `y`: occupancy. `z`: twinkle depth. `w`: unused.
    pub star_params2: Vec4,
    /// `x`: brightness. `y`: noise scale. `z`: dust. `w`: band tightness.
    pub galaxy_params: Vec4,
    /// Linear colour of the galactic core.
    pub galaxy_core_color: Vec4,
    /// Linear colour of the outer arms.
    pub galaxy_edge_color: Vec4,
    /// `xyz`: galactic pole. `w`: core concentration.
    pub galaxy_axis: Vec4,
    /// `xyz`: galactic centre. `w`: unused.
    pub galaxy_center: Vec4,
    /// `x`: brightness. `y`: earthshine. `z`: detail. `w`: phase.
    pub moon_params: Vec4,
    /// Linear tint of the moon's surface.
    pub moon_tint: Vec4,
    /// `x`: coverage. `y`: density. `z`: base altitude. `w`: thickness.
    pub cloud_params0: Vec4,
    /// `x`: shape scale. `y`: detail scale. `z`: detail strength. `w`: extinction.
    pub cloud_params1: Vec4,
    /// `x`: forward g. `y`: back g. `z`: powder. `w`: ambient.
    pub cloud_params2: Vec4,
    /// `x`: exposure. `y`: horizon fade. `z`: steps. `w`: light steps.
    pub cloud_params3: Vec4,
    /// Linear cloud albedo.
    pub cloud_albedo: Vec4,
    /// `xyz`: wind displacement in metres. `w`: shape evolution.
    pub cloud_offset: Vec4,
    /// `rgb`: lightning flash colour and strength. `a`: interior response.
    pub lightning: Vec4,
    /// `rgb`: ground-fog colour, in post-exposure units.
    pub fog_color: Vec4,
    /// `x`: fog extinction per world unit. `y`: fog layer height.
    pub fog_params: Vec4,
    /// `x`: time. `y`: planet radius. `z`: units per metre. `w`: feature flags.
    pub misc: Vec4,
    /// Rotates a world direction into the fixed frame the stars live in.
    pub star_from_world: Mat3,
}

impl Default for SkyUniform {
    fn default() -> Self {
        Self {
            sun_direction: Vec4::new(0.0, 1.0, 0.0, crate::celestial::DEFAULT_ANGULAR_RADIUS),
            moon_direction: Vec4::new(0.0, -1.0, 0.0, crate::celestial::DEFAULT_ANGULAR_RADIUS),
            sun_color: Vec4::new(1.0, 1.0, 1.0, 1.0),
            moon_color: Vec4::new(0.7, 0.8, 1.0, 0.0),
            star_params: Vec4::ZERO,
            star_params2: Vec4::ZERO,
            galaxy_params: Vec4::ZERO,
            galaxy_core_color: Vec4::ONE,
            galaxy_edge_color: Vec4::ONE,
            galaxy_axis: Vec4::new(0.0, 1.0, 0.0, 3.0),
            galaxy_center: Vec4::new(0.0, 0.0, 1.0, 0.0),
            moon_params: Vec4::ZERO,
            moon_tint: Vec4::ONE,
            cloud_params0: Vec4::ZERO,
            cloud_params1: Vec4::new(14_000.0, 1_400.0, 0.35, 0.045),
            cloud_params2: Vec4::new(0.8, -0.25, 0.6, 0.35),
            cloud_params3: Vec4::new(1.0, 0.06, 48.0, 4.0),
            cloud_albedo: Vec4::ONE,
            cloud_offset: Vec4::ZERO,
            lightning: Vec4::ZERO,
            fog_color: Vec4::ZERO,
            fog_params: Vec4::ZERO,
            misc: Vec4::new(0.0, EARTH_RADIUS_M, 1.0, 0.0),
            star_from_world: Mat3::IDENTITY,
        }
    }
}

/// The material behind the sky dome.
///
/// You normally never touch this: [`SkyPlugin`] rebuilds
/// [`uniform`](Self::uniform) from the weather resources every frame.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone, Default)]
pub struct SkyMaterial {
    /// Everything the shader reads.
    #[uniform(0)]
    pub uniform: SkyUniform,
}

/// Shared pipeline setup for both sky passes.
fn specialize_sky(
    descriptor: &mut RenderPipelineDescriptor,
    layout: &MeshVertexBufferLayoutRef,
) -> Result<(), SpecializedMeshPipelineError> {
    // The triangle is already in clip space and is viewed from "inside".
    descriptor.primitive.cull_mode = None;

    if let Some(depth_stencil) = descriptor.depth_stencil.as_mut() {
        // Never write depth: the atmosphere pass keys off a far-plane depth of
        // zero to decide it is looking at sky rather than at geometry.
        depth_stencil.depth_write_enabled = Some(false);
        // `GreaterEqual` rather than `Greater`, because in reverse-Z the sky
        // sits *at* the far plane and has to pass against an untouched depth
        // buffer -- while still losing to any real geometry in front of it.
        depth_stencil.depth_compare = Some(CompareFunction::GreaterEqual);
    }

    // Position is the only attribute these shaders read.
    descriptor.vertex.buffers = vec![
        layout
            .0
            .get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0)])?,
    ];

    Ok(())
}

impl Material for SkyMaterial {
    fn vertex_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        // Opaque, so it lands in the opaque pass and Bevy's atmosphere
        // composites over it. The depth state in `specialize_sky` is what stops
        // it behaving like ordinary opaque geometry.
        AlphaMode::Opaque
    }

    fn enable_prepass() -> bool {
        // A prepass would write the sky's depth and convince the atmosphere
        // there is solid geometry at the far plane.
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        specialize_sky(descriptor, layout)
    }
}

/// The volumetric cloud layer.
///
/// A separate material from [`SkyMaterial`] purely so it can be alpha-blended
/// in the transparent pass, which runs *after* Bevy's atmosphere. See the
/// header comment in `sky.wgsl` for why that ordering is not optional.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone, Default)]
pub struct CloudMaterial {
    /// The same uniform [`SkyMaterial`] uses; both passes are driven together.
    #[uniform(0)]
    pub uniform: SkyUniform,
}

impl Material for CloudMaterial {
    fn vertex_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn depth_bias(&self) -> f32 {
        // Push the clouds to the back of the transparent queue so that rain,
        // snow and the player's own transparent geometry all draw in front of
        // them.
        -1.0e6
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push("CLOUD_PASS".into());
        }
        specialize_sky(descriptor, layout)
    }
}

/// A single oversized triangle covering the viewport in normalised device
/// coordinates.
///
/// One triangle rather than two so there is no diagonal seam, and no
/// quad-shading inefficiency down the middle of the screen.
fn fullscreen_triangle() -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![[-1.0, -1.0, 0.0], [3.0, -1.0, 0.0], [-1.0, 3.0, 0.0]],
    )
    .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

/// Builds the star frame rotation for a given clock.
///
/// Returns the matrix that takes a world-space direction into the frame the
/// stars are fixed in. Because it is built from local sidereal time rather than
/// solar time, the star field drifts about four minutes a day against the sun —
/// so a given constellation returns to the same place a little earlier each
/// night, exactly as it does in reality.
pub fn star_frame_from_world(time: &WeatherTime) -> Mat3 {
    let latitude = time.latitude.to_radians();
    // Solar hour angle plus the sun's own yearly march around the sky.
    let sidereal = core::f32::consts::TAU
        * ((time.time_of_day - 0.5) + (time.day as f32 / DAYS_PER_YEAR).fract());
    let (sin_lst, cos_lst) = sidereal.sin_cos();
    let (sin_lat, cos_lat) = latitude.sin_cos();

    // Rows of the fixed-frame-to-world rotation, matching the horizon basis
    // used by `celestial::equatorial_to_world`: east, up, and south (Bevy's
    // `+Z`, since north is `-Z`).
    let east = Vec3::new(-sin_lst, cos_lst, 0.0);
    let up = Vec3::new(cos_lat * cos_lst, cos_lat * sin_lst, sin_lat);
    let south = Vec3::new(sin_lat * cos_lst, sin_lat * sin_lst, -cos_lat);

    // Those three are the *rows* of world-from-star, so a matrix built from
    // them as columns is its transpose -- which, for a rotation, is its
    // inverse. That is exactly the world-to-star direction we want.
    Mat3::from_cols(east, up, south)
}

/// Renders the sky dome and keeps its uniform up to date.
#[derive(Debug, Clone, Copy, Default)]
pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "sky.wgsl");

        app.init_resource::<StarConfig>()
            .init_resource::<GalaxyConfig>()
            .init_resource::<MoonConfig>()
            .init_resource::<CloudConfig>()
            .register_type::<StarConfig>()
            .register_type::<GalaxyConfig>()
            .register_type::<MoonConfig>()
            .register_type::<CloudConfig>()
            .register_type::<SkyDome>()
            .add_plugins((
                MaterialPlugin::<SkyMaterial>::default(),
                MaterialPlugin::<CloudMaterial>::default(),
            ))
            .add_systems(Startup, spawn_sky)
            .add_systems(Update, update_sky.in_set(WeatherSystems::Apply));
    }
}

fn spawn_sky(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut sky_materials: ResMut<Assets<SkyMaterial>>,
    mut cloud_materials: ResMut<Assets<CloudMaterial>>,
) {
    let mesh = meshes.add(fullscreen_triangle());

    // The triangle's vertices are clip-space coordinates, so its bounding box
    // is meaningless and culling on it would be wrong.
    commands.spawn((
        SkyDome,
        Mesh3d(mesh.clone()),
        MeshMaterial3d(sky_materials.add(SkyMaterial::default())),
        NoFrustumCulling,
        Transform::default(),
    ));
    commands.spawn((
        SkyDome,
        Mesh3d(mesh),
        MeshMaterial3d(cloud_materials.add(CloudMaterial::default())),
        NoFrustumCulling,
        Transform::default(),
    ));
}

/// How often the sky material's uniform is rebuilt: every frame.
#[expect(
    clippy::too_many_arguments,
    reason = "the sky is a function of every weather resource; grouping them \
              into a SystemParam would hide that rather than simplify it"
)]
fn update_sky(
    mut sky_materials: ResMut<Assets<SkyMaterial>>,
    mut cloud_materials: ResMut<Assets<CloudMaterial>>,
    space_domes: Query<&MeshMaterial3d<SkyMaterial>, bevy::ecs::query::With<SkyDome>>,
    cloud_domes: Query<&MeshMaterial3d<CloudMaterial>, bevy::ecs::query::With<SkyDome>>,
    config: Res<WeatherConfig>,
    stars: Res<StarConfig>,
    galaxy: Res<GalaxyConfig>,
    moon: Res<MoonConfig>,
    clouds: Res<CloudConfig>,
    fog: Res<FogConfig>,
    weather: Res<Weather>,
    bodies: Res<CelestialBodies>,
    weather_time: Res<WeatherTime>,
    wind: Res<Wind>,
    lightning: Res<LightningState>,
    time: Res<Time>,
) {
    let uniform = build_uniform(
        &config,
        &stars,
        &galaxy,
        &moon,
        &clouds,
        &fog,
        &weather,
        &bodies,
        &weather_time,
        &wind,
        &lightning,
        time.elapsed_secs_wrapped(),
    );

    for handle in &space_domes {
        if let Some(mut material) = sky_materials.get_mut(&handle.0) {
            material.uniform = uniform.clone();
        }
    }
    for handle in &cloud_domes {
        if let Some(mut material) = cloud_materials.get_mut(&handle.0) {
            material.uniform = uniform.clone();
        }
    }
}

/// Assembles the shader uniform from the weather resources.
///
/// Split out from the system so it can be unit tested without a render world.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the system's parameters; see the note there"
)]
pub fn build_uniform(
    config: &WeatherConfig,
    stars: &StarConfig,
    galaxy: &GalaxyConfig,
    moon: &MoonConfig,
    clouds: &CloudConfig,
    fog: &FogConfig,
    weather: &Weather,
    bodies: &CelestialBodies,
    weather_time: &WeatherTime,
    wind: &Wind,
    lightning: &LightningState,
    elapsed: f32,
) -> SkyUniform {
    let conditions = weather.current;

    let mut flags = 0u32;
    if config.sky {
        if stars.enabled {
            flags |= FLAG_STARS;
        }
        if galaxy.enabled {
            flags |= FLAG_GALAXY;
        }
        if moon.enabled {
            flags |= FLAG_MOON;
        }
        if clouds_enabled(config, &conditions) {
            flags |= FLAG_CLOUDS;
        }
    }

    let steps = clouds.steps.unwrap_or_else(|| config.quality.cloud_steps());
    let light_steps = clouds
        .light_steps
        .unwrap_or_else(|| config.quality.cloud_light_steps());

    // Clouds ride higher and faster than surface wind.
    let cloud_offset = wind.offset3() * clouds.wind_multiplier;

    let sun = bodies.sun_direction;
    let moon_dir = bodies.moon_direction;
    let sun_color = bodies.sun_color.to_vec3();
    let moon_tint = moon.tint.to_linear().to_vec3();

    SkyUniform {
        sun_direction: sun.extend(bodies.sun_angular_radius),
        moon_direction: moon_dir.extend(moon.angular_radius.max(1e-5)),
        sun_color: sun_color.extend(bodies.daylight),
        moon_color: (moon_tint * NIGHT_SCALE).extend(bodies.moon_illumination),

        star_params: Vec4::new(
            stars.density.max(1.0),
            stars.brightness * NIGHT_SCALE,
            stars.twinkle_speed,
            stars.size.max(1e-4),
        ),
        star_params2: Vec4::new(
            stars.color_variation.clamp(0.0, 1.0),
            stars.occupancy.clamp(0.0, 1.0),
            stars.twinkle_amount.clamp(0.0, 1.0),
            0.0,
        ),

        galaxy_params: Vec4::new(
            galaxy.brightness * NIGHT_SCALE,
            galaxy.noise_scale,
            galaxy.dust.clamp(0.0, 1.0),
            galaxy.band_tightness.max(0.1),
        ),
        galaxy_core_color: galaxy.core_color.to_linear().to_vec3().extend(1.0),
        galaxy_edge_color: galaxy.edge_color.to_linear().to_vec3().extend(1.0),
        galaxy_axis: galaxy
            .pole
            .normalize_or(Vec3::Y)
            .extend(galaxy.core_concentration.max(0.01)),
        galaxy_center: galaxy.center.normalize_or(Vec3::Z).extend(0.0),

        moon_params: Vec4::new(
            moon.brightness * NIGHT_SCALE,
            moon.earthshine.max(0.0),
            moon.surface_detail.clamp(0.0, 1.0),
            bodies.moon_phase,
        ),
        moon_tint: moon_tint.extend(1.0),

        cloud_params0: Vec4::new(
            conditions.cloud_coverage,
            conditions.cloud_density,
            conditions.cloud_altitude,
            conditions.cloud_thickness,
        ),
        cloud_params1: Vec4::new(
            clouds.shape_scale.max(1.0),
            clouds.detail_scale.max(1.0),
            clouds.detail_strength.clamp(0.0, 1.0),
            clouds.extinction.max(1e-6),
        ),
        cloud_params2: Vec4::new(
            clouds.forward_scattering.clamp(-0.99, 0.99),
            clouds.back_scattering.clamp(-0.99, 0.99),
            clouds.powder.clamp(0.0, 1.0),
            clouds.ambient.max(0.0),
        ),
        cloud_params3: Vec4::new(
            clouds.exposure.max(0.0),
            clouds.horizon_fade.max(1e-4),
            steps as f32,
            light_steps as f32,
        ),
        cloud_albedo: clouds.albedo_linear().to_vec3().extend(1.0),
        cloud_offset: cloud_offset.extend(elapsed * clouds.evolution_rate),

        lightning: (lightning.color.to_vec3() * lightning.intensity)
            .extend(clouds.lightning_response.max(0.0)),

        fog_color: fog
            .color_at(
                bodies.daylight,
                conditions.cloud_coverage * conditions.cloud_density,
            )
            .to_vec3()
            .extend(1.0),
        fog_params: Vec4::new(
            fog.extinction_at(conditions.fog),
            fog.volume_height.max(0.1),
            0.0,
            0.0,
        ),

        misc: Vec4::new(
            elapsed,
            EARTH_RADIUS_M,
            config.units_per_meter.max(1e-6),
            flags as f32,
        ),

        star_from_world: star_frame_from_world(weather_time),
    }
}

fn clouds_enabled(config: &WeatherConfig, conditions: &crate::state::WeatherConditions) -> bool {
    // Skip the raymarch entirely on a genuinely cloudless sky; it is by far the
    // most expensive thing this shader does.
    config.clouds && conditions.cloud_coverage > 0.001
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::celestial::compute_celestial;

    #[test]
    fn star_frame_is_a_rotation() {
        let mut time = WeatherTime::default();
        for _ in 0..200 {
            time.advance(0.0173);
            let m = star_frame_from_world(&time);
            let identity = m * m.transpose();
            for row in 0..3 {
                for column in 0..3 {
                    let expected = if row == column { 1.0 } else { 0.0 };
                    assert!(
                        (identity.col(column)[row] - expected).abs() < 1e-4,
                        "not orthonormal: {identity:?}"
                    );
                }
            }
            assert!((m.determinant() - 1.0).abs() < 1e-4, "not a rotation");
        }
    }

    #[test]
    fn stars_are_fixed_while_the_sky_turns() {
        // A fixed star direction must map to different world directions as the
        // night goes on -- that rotation is the whole point.
        let mut early = WeatherTime::default();
        early.set_hour(21.0);
        let mut late = WeatherTime::default();
        late.set_hour(3.0);

        let world_dir = Vec3::new(0.3, 0.6, -0.74).normalize();
        let a = star_frame_from_world(&early) * world_dir;
        let b = star_frame_from_world(&late) * world_dir;
        assert!(
            a.dot(b) < 0.99,
            "the star field did not rotate: {}",
            a.dot(b)
        );
    }

    #[test]
    fn celestial_pole_stays_put_all_night() {
        // Everything rotates about the pole, so the pole itself must not move.
        let latitude = 45f32.to_radians();
        let pole = Vec3::new(0.0, latitude.sin(), -latitude.cos());

        let mut reference: Option<Vec3> = None;
        let mut time = WeatherTime {
            latitude: 45.0,
            ..Default::default()
        };
        for _ in 0..24 {
            time.advance(1.0 / 24.0);
            let star_pole = star_frame_from_world(&time) * pole;
            match reference {
                None => reference = Some(star_pole),
                Some(first) => assert!(
                    first.dot(star_pole) > 0.9999,
                    "the pole drifted: {first:?} vs {star_pole:?}"
                ),
            }
        }
    }

    fn uniform_for(weather: Weather, config: WeatherConfig) -> SkyUniform {
        let time = WeatherTime::default();
        build_uniform(
            &config,
            &StarConfig::default(),
            &GalaxyConfig::default(),
            &MoonConfig::default(),
            &CloudConfig::default(),
            &FogConfig::default(),
            &weather,
            &compute_celestial(&time),
            &time,
            &Wind::default(),
            &LightningState::default(),
            0.0,
        )
    }

    #[test]
    fn feature_flags_follow_the_config() {
        let cloudy = Weather::new(crate::presets::WeatherPreset::Overcast.conditions());
        let all_on = uniform_for(cloudy.clone(), WeatherConfig::default());
        let flags = all_on.misc.w as u32;
        assert_eq!(flags & FLAG_STARS, FLAG_STARS);
        assert_eq!(flags & FLAG_GALAXY, FLAG_GALAXY);
        assert_eq!(flags & FLAG_MOON, FLAG_MOON);
        assert_eq!(flags & FLAG_CLOUDS, FLAG_CLOUDS);

        let sky_off = uniform_for(
            cloudy.clone(),
            WeatherConfig {
                sky: false,
                ..Default::default()
            },
        );
        assert_eq!(sky_off.misc.w as u32, 0);

        let clouds_off = uniform_for(
            cloudy,
            WeatherConfig {
                clouds: false,
                ..Default::default()
            },
        );
        assert_eq!((clouds_off.misc.w as u32) & FLAG_CLOUDS, 0);
    }

    #[test]
    fn a_clear_sky_skips_the_cloud_raymarch() {
        let clear = Weather::new(crate::presets::WeatherPreset::Clear.conditions());
        let uniform = uniform_for(clear, WeatherConfig::default());
        assert_eq!((uniform.misc.w as u32) & FLAG_CLOUDS, 0);
    }

    #[test]
    fn directions_reach_the_shader_normalised() {
        let uniform = uniform_for(Weather::default(), WeatherConfig::default());
        assert!((uniform.sun_direction.truncate().length() - 1.0).abs() < 1e-4);
        assert!((uniform.moon_direction.truncate().length() - 1.0).abs() < 1e-4);
        assert!((uniform.galaxy_axis.truncate().length() - 1.0).abs() < 1e-4);
        assert!((uniform.galaxy_center.truncate().length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn quality_drives_the_cloud_step_count() {
        let cloudy = Weather::new(crate::presets::WeatherPreset::Overcast.conditions());
        for quality in [
            crate::config::Quality::Low,
            crate::config::Quality::Medium,
            crate::config::Quality::High,
            crate::config::Quality::Ultra,
        ] {
            let uniform = uniform_for(
                cloudy.clone(),
                WeatherConfig {
                    quality,
                    ..Default::default()
                },
            );
            assert_eq!(uniform.cloud_params3.z as u32, quality.cloud_steps());
            assert_eq!(uniform.cloud_params3.w as u32, quality.cloud_light_steps());
        }
    }

    #[test]
    fn the_fullscreen_triangle_covers_the_viewport() {
        let mesh = fullscreen_triangle();
        assert_eq!(mesh.count_vertices(), 3);
        // Every corner of NDC space must fall inside the triangle.
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x3(values) => values.clone(),
            other => panic!("unexpected position format: {other:?}"),
        };
        let point_in_triangle = |p: [f32; 2]| {
            let sign = |a: &[f32; 3], b: &[f32; 3], c: [f32; 2]| {
                (c[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (c[1] - b[1])
            };
            let d1 = sign(&positions[0], &positions[1], p);
            let d2 = sign(&positions[1], &positions[2], p);
            let d3 = sign(&positions[2], &positions[0], p);
            let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            !(has_negative && has_positive)
        };
        for corner in [[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]] {
            assert!(point_in_triangle(corner), "corner {corner:?} not covered");
        }
    }
}
