//! Rain on the camera lens.
//!
//! A screen-space effect: water on the glass in front of the viewer rather than
//! in the world. Drops cling, run down, leave broken trails behind them and
//! refract what is behind them. It is the thing that makes a downpour feel like
//! something happening *to you* rather than something you are watching.
//!
//! Everything is procedural and stateless. A droplet is a function of the pixel
//! and the clock, so nothing is simulated and nothing is stored; the cost is
//! the same whether the lens is dry or streaming.
//!
//! # Where it sits
//!
//! Before tonemapping, so the scene it refracts is still in high dynamic range
//! and a headlight or a lightning flash seen through a droplet stays bright.
//! That is most of what sells the effect: a droplet that only ever shows a
//! muted version of an already-compressed image reads as dirt on the lens.
//!
//! # Accumulation
//!
//! How wet the lens is lags the weather, through [`LensWetness`]. Rain does not
//! put water on the glass instantly and the glass does not dry the moment it
//! stops, so the drops build up over a few seconds and linger for rather longer
//! -- which is also what stops a passing shower flickering the effect on and
//! off.

use bevy::app::{App, Plugin, Update};
use bevy::asset::{AssetServer, Handle, embedded_asset, load_embedded_asset};
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::core_pipeline::tonemapping::tonemapping;
use bevy::ecs::component::Component;
use bevy::ecs::query::With;
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::math::{Vec4, Vec4Swizzles};
use bevy::prelude::Entity;
use bevy::reflect::Reflect;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, DynamicUniformBuffer, FilterMode, FragmentState, Operations,
    PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor,
    Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages, ShaderType,
    SpecializedRenderPipeline, SpecializedRenderPipelines, TextureSampleType,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;
use bevy::time::Time;

use crate::WeatherSystems;
use crate::config::{WeatherCamera, WeatherConfig};
use crate::state::Weather;
use bevy::ecs::reflect::ReflectResource;
use bevy::reflect::std_traits::ReflectDefault;

/// Settings for rain on the lens.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource, Default)]
pub struct RainLensConfig {
    /// Run the effect at all.
    ///
    /// Worth exposing in a settings menu whatever its cost: a lens the player
    /// is not supposed to be looking through -- a first-person view with no
    /// helmet, anything set indoors -- is a place where water on the glass is
    /// simply wrong, and some people find it distracting regardless.
    pub enabled: bool,

    /// How wet the lens gets in the heaviest rain, `0.0..=1.0`.
    ///
    /// This is the density of droplets, not their size. At `1.0` nearly every
    /// cell of both layers carries one and the view is genuinely hard to see
    /// through, which is realistic and rarely what a game wants.
    pub max_wetness: f32,

    /// Seconds for the lens to reach its full wetness once rain starts.
    pub wet_seconds: f32,

    /// Seconds for the lens to dry once the rain stops.
    ///
    /// Longer than [`wet_seconds`](Self::wet_seconds), because it is: water
    /// arrives as fast as it falls and leaves at the speed of evaporation.
    pub dry_seconds: f32,

    /// How far a droplet displaces what is behind it, in screen widths.
    pub refraction: f32,

    /// Radius of the softening inside a droplet, in screen widths.
    ///
    /// A droplet focuses somewhere other than the sensor, so what it shows is
    /// out of focus as well as displaced.
    pub blur: f32,

    /// Running drops across the width of the screen.
    ///
    /// Fewer means larger, slower drops; more means a fine spray.
    pub drop_cells: f32,

    /// Clinging beads across the width of the screen.
    pub bead_cells: f32,

    /// How fast drops run down the glass.
    pub fall_speed: f32,

    /// How much of the trail behind a running drop survives, `0.0..=1.0`.
    pub trail_density: f32,

    /// Brightness of the light caught on a droplet's rim.
    pub specular: f32,
}

impl Default for RainLensConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_wetness: 0.6,
            wet_seconds: 3.5,
            dry_seconds: 14.0,
            refraction: 3.2,
            blur: 0.0035,
            drop_cells: 9.0,
            bead_cells: 26.0,
            fall_speed: 0.32,
            trail_density: 0.45,
            specular: 0.8,
        }
    }
}

/// How wet the lens currently is, `0.0..=1.0`.
///
/// Read it if you want to drive something else from the same state -- a wiper,
/// a sound, a shader of your own. Written every frame by the plugin.
#[derive(Resource, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Resource, Default)]
pub struct LensWetness(pub f32);

/// Put this on a camera to give it a wet lens.
///
/// [`RainLensPlugin`] adds and removes it on cameras marked
/// [`WeatherCamera`](crate::config::WeatherCamera) automatically; add it
/// yourself if you want one camera wet and another dry.
#[derive(Component, Debug, Clone, Copy, ExtractComponent)]
pub struct RainLens {
    /// How wet this lens is, `0.0..=1.0`.
    pub wetness: f32,
    /// See [`RainLensConfig::refraction`].
    pub refraction: f32,
    /// See [`RainLensConfig::blur`].
    pub blur: f32,
    /// See [`RainLensConfig::drop_cells`].
    pub drop_cells: f32,
    /// See [`RainLensConfig::bead_cells`].
    pub bead_cells: f32,
    /// See [`RainLensConfig::fall_speed`].
    pub fall_speed: f32,
    /// See [`RainLensConfig::trail_density`].
    pub trail_density: f32,
    /// See [`RainLensConfig::specular`].
    pub specular: f32,
}

impl Default for RainLens {
    fn default() -> Self {
        let config = RainLensConfig::default();
        Self {
            wetness: 0.0,
            refraction: config.refraction,
            blur: config.blur,
            drop_cells: config.drop_cells,
            bead_cells: config.bead_cells,
            fall_speed: config.fall_speed,
            trail_density: config.trail_density,
            specular: config.specular,
        }
    }
}

/// What the shader reads. Field order must match `RainLens` in
/// `rain_lens.wgsl`.
#[derive(ShaderType, Debug, Clone, Default)]
pub struct RainLensUniform {
    /// `x`: wetness. `y`: seconds. `z`: refraction. `w`: aspect ratio.
    pub params: Vec4,
    /// `x`: drop cells. `y`: fall speed. `z`: bead cells. `w`: blur radius.
    pub params2: Vec4,
    /// `x`: specular. `y`: trail density. `z`, `w`: unused.
    pub params3: Vec4,
}

/// Adds rain on the lens.
#[derive(Debug, Clone, Copy, Default)]
pub struct RainLensPlugin;

impl Plugin for RainLensPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "rain_lens.wgsl");

        app.init_resource::<RainLensConfig>()
            .init_resource::<LensWetness>()
            .register_type::<RainLensConfig>()
            .register_type::<LensWetness>()
            .add_plugins(ExtractComponentPlugin::<RainLens>::default())
            .add_systems(Update, drive_rain_lens.in_set(WeatherSystems::Apply));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<RainLensPipeline>>()
            .init_gpu_resource::<RainLensUniforms>()
            .add_systems(RenderStartup, init_rain_lens_pipeline)
            .add_systems(
                Render,
                (prepare_rain_lens_pipelines, prepare_rain_lens_uniforms)
                    .in_set(RenderSystems::Prepare),
            )
            // In the post-process stage, so the scene has actually been drawn
            // by the time this reads it -- the pass swaps the view target's
            // ping-pong pair, so running it early would leave every later pass
            // writing into the buffer nobody reads.
            //
            // Before tonemapping, so the scene it refracts is still in high
            // dynamic range.
            .add_systems(
                Core3d,
                rain_lens_pass
                    .in_set(Core3dSystems::PostProcess)
                    .before(tonemapping),
            );
    }
}

/// Keeps [`LensWetness`] tracking the weather and the cameras in step with it.
fn drive_rain_lens(
    mut commands: Commands,
    config: Res<WeatherConfig>,
    lens: Res<RainLensConfig>,
    weather: Res<Weather>,
    time: Res<Time>,
    mut wetness: ResMut<LensWetness>,
    cameras: Query<(Entity, Option<&RainLens>), With<WeatherCamera>>,
) {
    let on = lens.enabled && config.precipitation;
    let target = if on {
        // Sleet and snow leave water on the glass too, though snow takes a
        // moment to melt; both are folded in as rain's equal here rather than
        // modelled separately.
        let wet = weather.current.rain.max(weather.current.snow * 0.55);
        (wet * lens.max_wetness).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Exponential approach, at a different rate each way: water arrives as fast
    // as it falls and leaves at the speed of evaporation.
    let seconds = if target > wetness.0 {
        lens.wet_seconds
    } else {
        lens.dry_seconds
    };
    wetness.0 = approach(wetness.0, target, seconds, time.delta_secs());

    for (entity, existing) in &cameras {
        // A dry lens carries no component at all, so the render world does no
        // work for it -- no pipeline to specialise, no uniform to write.
        if wetness.0 <= 0.001 {
            if existing.is_some() {
                commands.entity(entity).remove::<RainLens>();
            }
            continue;
        }
        commands.entity(entity).insert(RainLens {
            wetness: wetness.0,
            refraction: lens.refraction.max(0.0),
            blur: lens.blur.max(0.0),
            drop_cells: lens.drop_cells.max(1.0),
            bead_cells: lens.bead_cells.max(1.0),
            fall_speed: lens.fall_speed.max(0.0),
            trail_density: lens.trail_density.clamp(0.0, 1.0),
            specular: lens.specular.max(0.0),
        });
    }
}

/// Moves `current` toward `target`, covering most of the gap in `seconds`.
///
/// Framerate independent, which matters here: a per-frame lerp would dry the
/// lens twice as fast at 120 fps as at 60.
fn approach(current: f32, target: f32, seconds: f32, delta: f32) -> f32 {
    if seconds <= 1e-4 || delta <= 0.0 {
        return target;
    }
    // Three time constants is about 95% of the way there, which is what
    // "reaches it in `seconds`" should mean.
    let rate = 3.0 / seconds;
    let blend = 1.0 - (-rate * delta).exp();
    current + (target - current) * blend
}

// ---------------------------------------------------------------------------
// Render world
// ---------------------------------------------------------------------------

/// The pipeline and its bindings, built once.
#[derive(Resource)]
pub struct RainLensPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
    fragment: Handle<Shader>,
}

/// The specialisation key: only the target format varies.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RainLensPipelineKey {
    target_format: bevy::render::render_resource::TextureFormat,
}

/// The pipeline prepared for one view.
#[derive(Component)]
pub struct RainLensPipelineId(CachedRenderPipelineId);

/// Where this view's uniform sits in the shared buffer.
#[derive(Component)]
pub struct RainLensUniformOffset(u32);

/// One uniform per view, in a single dynamic buffer.
#[derive(Resource, Default)]
pub struct RainLensUniforms {
    buffer: DynamicUniformBuffer<RainLensUniform>,
}

fn init_rain_lens_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    fullscreen: Res<FullscreenShader>,
    asset_server: Res<AssetServer>,
) {
    let layout = BindGroupLayoutDescriptor::new(
        "rain lens bind group layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<RainLensUniform>(true),
            ),
        ),
    );

    let sampler = render_device.create_sampler(&SamplerDescriptor {
        label: Some("rain lens sampler"),
        min_filter: FilterMode::Linear,
        mag_filter: FilterMode::Linear,
        ..Default::default()
    });

    commands.insert_resource(RainLensPipeline {
        layout,
        sampler,
        fullscreen: fullscreen.clone(),
        fragment: load_embedded_asset!(asset_server.as_ref(), "rain_lens.wgsl"),
    });
}

impl SpecializedRenderPipeline for RainLensPipeline {
    type Key = RainLensPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("rain lens".into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: self.fragment.clone(),
                targets: vec![Some(ColorTargetState {
                    format: key.target_format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

fn prepare_rain_lens_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<RainLensPipeline>>,
    pipeline: Res<RainLensPipeline>,
    views: Query<(Entity, &ExtractedView), With<RainLens>>,
) {
    for (entity, view) in views.iter() {
        let id = pipelines.specialize(
            &pipeline_cache,
            &pipeline,
            RainLensPipelineKey {
                target_format: view.target_format,
            },
        );
        commands.entity(entity).insert(RainLensPipelineId(id));
    }
}

fn prepare_rain_lens_uniforms(
    mut commands: Commands,
    mut uniforms: ResMut<RainLensUniforms>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    time: Res<Time>,
    views: Query<(Entity, &RainLens, &ExtractedView)>,
) {
    uniforms.buffer.clear();
    for (entity, lens, view) in views.iter() {
        let size = view.viewport.zw().as_vec2();
        let aspect = if size.y > 0.0 {
            (size.x / size.y).max(0.01)
        } else {
            16.0 / 9.0
        };
        let offset = uniforms.buffer.push(&RainLensUniform {
            params: Vec4::new(
                lens.wetness,
                time.elapsed_secs_wrapped(),
                lens.refraction,
                aspect,
            ),
            params2: Vec4::new(lens.drop_cells, lens.fall_speed, lens.bead_cells, lens.blur),
            params3: Vec4::new(lens.specular, lens.trail_density, 0.0, 0.0),
        });
        commands
            .entity(entity)
            .insert(RainLensUniformOffset(offset));
    }
    uniforms.buffer.write_buffer(&render_device, &render_queue);
}

fn rain_lens_pass(
    view: ViewQuery<(
        &ViewTarget,
        &RainLens,
        &RainLensPipelineId,
        &RainLensUniformOffset,
    )>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<RainLensPipeline>,
    uniforms: Res<RainLensUniforms>,
    mut ctx: RenderContext,
) {
    let (view_target, lens, pipeline_id, offset) = view.into_inner();
    if lens.wetness <= 0.002 {
        return;
    }
    let Some(render_pipeline) = pipeline_cache.get_render_pipeline(pipeline_id.0) else {
        return;
    };
    let Some(binding) = uniforms.buffer.binding() else {
        return;
    };

    // A full-screen pass that reads what is already there, so it needs the
    // ping-pong pair rather than the target on its own.
    let post_process = view_target.post_process_write();

    let bind_group = ctx.render_device().create_bind_group(
        Some("rain lens bind group"),
        &pipeline_cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((post_process.source, &pipeline.sampler, binding)),
    );

    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("rain lens"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: post_process.destination,
            depth_slice: None,
            resolve_target: None,
            ops: Operations::default(),
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(render_pipeline);
    pass.set_bind_group(0, &bind_group, &[offset.0]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wetness_approaches_its_target_at_the_stated_rate() {
        // "Reaches it in `seconds`" has to mean something, or the two rates are
        // not comparable with each other or with anything else.
        let mut wet = 0.0;
        let step = 1.0 / 60.0;
        for _ in 0..(60.0 * 4.0) as u32 {
            wet = approach(wet, 1.0, 4.0, step);
        }
        assert!(wet > 0.94 && wet < 0.97, "reached {wet} after its time");
    }

    #[test]
    fn wetness_is_framerate_independent() {
        // A per-frame lerp dries the lens twice as fast at 120 fps as at 60,
        // which is the kind of thing nobody notices until they change monitor.
        let simulate = |step: f32| {
            let mut wet = 1.0;
            let steps = (6.0 / step) as u32;
            for _ in 0..steps {
                wet = approach(wet, 0.0, 10.0, step);
            }
            wet
        };
        let slow = simulate(1.0 / 30.0);
        let fast = simulate(1.0 / 240.0);
        assert!(
            (slow - fast).abs() < 0.01,
            "{slow} at 30 fps against {fast} at 240"
        );
    }

    #[test]
    fn a_dry_lens_stays_dry() {
        let mut wet = 0.0;
        for _ in 0..600 {
            wet = approach(wet, 0.0, 5.0, 1.0 / 60.0);
        }
        assert_eq!(wet, 0.0);
    }

    #[test]
    fn drying_is_slower_than_wetting() {
        // Water arrives as fast as it falls and leaves at the speed of
        // evaporation; a lens that dried as fast as it wetted would flicker
        // through a squall.
        let config = RainLensConfig::default();
        assert!(config.dry_seconds > config.wet_seconds * 2.0);
    }
}
