//! Internal visual-verification harness: renders a series of fixed
//! time-of-day / weather combinations, screenshots each, then exits.
//!
//! Not part of the public API surface; it exists so the shaders can be checked
//! on a real GPU without a human at the keyboard.
//!
//! ```sh
//! cargo run --example capture --release -- <output-directory>
//! ```
//!
//! Two environment variables help when something looks wrong:
//!
//! * `BIG_MOON=1` renders the moon at about ten times its normal size, which is
//!   the only practical way to check the phase, the terminator and the surface
//!   detail -- at its real size it is twenty pixels across.
//! * `NO_STARS=1` turns off the star field and galaxy, to tell whether a stray
//!   bright pixel is a star or something else.
//! * `NO_ATMO=1` disables the atmosphere pass, which separates what this
//!   plugin's own shaders produce from what the atmosphere then does to it.
//! * `QUALITY=potato|low|medium|high|ultra` picks a quality tier, for
//!   comparing what each one actually looks like.
//! * `WB=<n>` overrides `MoonConfig::white_balance`, for checking how far the
//!   moon's colour moves between no correction and full.
//! * `FLICKER=1` replaces the shot list with a burst of consecutive frames of
//!   one cloudy sky, for measuring temporal stability. Diffing neighbouring
//!   frames is the only way to catch clouds that flicker: every frame on its
//!   own looks perfectly reasonable.
//! * `NO_FAST=1` disables `CloudConfig::adaptive_marching`, so the fast path
//!   can be checked against the slow one for both looks and stability.
//! * `SWEEP=clear|cloudy|overcast` replaces the shot list with the sun walking
//!   down through sunset and on into the night, framed where it went down.
//!   Anything still glowing there after astronomical twilight is light that
//!   should not be arriving.
//! * `SHADOWS=1` shoots the ground from height, which is the only way to see a
//!   cloud shadow: they are kilometres across. `SHADOWTILE=<m>` shrinks the
//!   pattern and `NO_CLOUD_SHADOWS=1` turns it off for an A/B.
//! * `GODRAYS=1` stands in the fog looking into a low sun, which is the framing
//!   where a shadow-cascade boundary shows up. `BEVY_CASCADES=1` puts Bevy's
//!   stock 10/150 layout back, for comparison.
//! * `METEORS=<per minute>` turns the meteor rate up. At the default rate a run
//!   will never catch one.
//! * `TINT=<0..1>` overrides `SunConfig::light_tint`, `HAZE=<n>` overrides
//!   `AtmosphereConfig::aerosol_density`, and `EV=<n>` / `TONEMAP=tony|aces|agx|
//!   blender|reinhard` override the exposure and tonemapper -- the four knobs
//!   that decide what a sunset looks like.

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::time::TimeUpdateStrategy;
use bevy::window::WindowResolution;

use bevy_weather::prelude::*;

/// Where to point the camera for a shot.
#[derive(Clone, Copy, PartialEq)]
enum Aim {
    /// Fixed yaw (radians) and pitch (degrees).
    Fixed { yaw: f32, pitch: f32 },
    /// Straight at the sun, wherever the clock has put it.
    Sun,
    /// Straight at the moon.
    Moon,
    /// At the sun's compass bearing, but held at a fixed pitch, so the shot
    /// still frames the horizon once the sun has set.
    SunBearing { pitch: f32 },
    /// From high up, looking down, to see what is happening on the ground.
    Aerial { yaw: f32, pitch: f32, height: f32 },
    /// Standing on the far side of the scene from the sun, at eye height,
    /// looking into it -- so the pillars are between the camera and the light
    /// and throw shafts through whatever is in the air.
    IntoSun { pitch: f32, back: f32 },
}

/// One capture.
struct Shot {
    name: &'static str,
    hour: f32,
    preset: WeatherPreset,
    aim: Aim,
    /// Overrides `WeatherTime::moon_phase_offset` when set.
    phase: Option<f32>,
}

const fn shot(name: &'static str, hour: f32, preset: WeatherPreset, aim: Aim) -> Shot {
    Shot {
        name,
        hour,
        preset,
        aim,
        phase: None,
    }
}

const fn at_phase(mut s: Shot, phase: f32) -> Shot {
    s.phase = Some(phase);
    s
}

fn shots() -> Vec<Shot> {
    let fixed = |yaw: f32, pitch: f32| Aim::Fixed { yaw, pitch };

    if std::env::var("FLICKER").is_ok() {
        // The same sky over and over. Nothing changes between these shots
        // except the clock ticking on, so any difference between neighbouring
        // frames is the renderer being unstable rather than the weather moving.
        return (0..16)
            .map(|i| {
                shot(
                    Box::leak(format!("flicker-{i:02}").into_boxed_str()),
                    12.0,
                    WeatherPreset::PartlyCloudy,
                    // Looking well up, where clouds fill the frame. Framed at
                    // the horizon they occupy a thin band and an unstable
                    // renderer can hide in it.
                    fixed(0.35, 30.0),
                )
            })
            .collect();
    }

    if std::env::var("GODRAYS").is_ok() {
        // Low sun through fog, at eye level, looking almost into it: the one
        // framing where a shadow-cascade boundary shows up as a line ruled
        // across the light shafts.
        return vec![
            Shot {
                name: "godrays-misty",
                hour: 17.6,
                preset: WeatherPreset::MistyMorning,
                aim: Aim::IntoSun {
                    pitch: 2.0,
                    back: 38.0,
                },
                phase: None,
            },
            Shot {
                name: "godrays-fog",
                hour: 17.6,
                preset: WeatherPreset::Fog,
                aim: Aim::IntoSun {
                    pitch: 2.0,
                    back: 38.0,
                },
                phase: None,
            },
        ];
    }

    if std::env::var("SHADOWS").is_ok() {
        // Looking down from height, because a cloud shadow is kilometres across
        // and there is no seeing one from the ground in a small scene.
        return vec![
            Shot {
                name: "shadow-noon",
                hour: 12.0,
                preset: WeatherPreset::PartlyCloudy,
                aim: Aim::Aerial {
                    yaw: 0.35,
                    pitch: -38.0,
                    height: 700.0,
                },
                phase: None,
            },
            Shot {
                name: "shadow-afternoon",
                hour: 16.0,
                preset: WeatherPreset::FewClouds,
                aim: Aim::Aerial {
                    yaw: 0.35,
                    pitch: -30.0,
                    height: 700.0,
                },
                phase: None,
            },
            Shot {
                name: "shadow-clear",
                hour: 12.0,
                preset: WeatherPreset::Clear,
                aim: Aim::Aerial {
                    yaw: 0.35,
                    pitch: -38.0,
                    height: 700.0,
                },
                phase: None,
            },
        ];
    }

    if let Ok(preset) = std::env::var("SWEEP") {
        // Walk the sun down through sunset and on into the night, framing the
        // horizon where it went down. Anything still glowing there after
        // astronomical twilight is light that should not be arriving.
        let preset = match preset.as_str() {
            "cloudy" => WeatherPreset::PartlyCloudy,
            "overcast" => WeatherPreset::Overcast,
            _ => WeatherPreset::Clear,
        };
        return [
            17.0, 18.0, 18.8, 19.4, 19.8, 20.2, 20.6, 21.0, 21.5, 22.0, 23.0, 0.5,
        ]
        .iter()
        .map(|&hour| Shot {
            name: Box::leak(
                format!("sweep-{:05.2}", hour)
                    .replace('.', "h")
                    .into_boxed_str(),
            ),
            hour,
            preset,
            aim: Aim::SunBearing { pitch: 12.0 },
            phase: None,
        })
        .collect();
    }

    vec![
        // The clock is set to the northern summer solstice at 45 degrees, so
        // the sun is up by half past four; 6.2 was mid-morning, not dawn.
        Shot {
            name: "01-dawn-clear",
            hour: 4.6,
            preset: WeatherPreset::Clear,
            aim: Aim::SunBearing { pitch: 10.0 },
            phase: None,
        },
        shot(
            "02-noon-cumulus",
            12.0,
            WeatherPreset::PartlyCloudy,
            fixed(0.35, 22.0),
        ),
        shot(
            "03-afternoon-overcast",
            15.0,
            WeatherPreset::Overcast,
            fixed(0.35, 14.0),
        ),
        shot(
            "04-sunset-rain",
            19.3,
            WeatherPreset::Rain,
            fixed(0.35, 8.0),
        ),
        shot(
            "05-night-stars",
            23.5,
            WeatherPreset::Clear,
            fixed(0.35, 34.0),
        ),
        shot(
            "06-night-galaxy-up",
            1.0,
            WeatherPreset::Clear,
            fixed(0.35, 65.0),
        ),
        shot(
            "07-thunderstorm",
            16.0,
            WeatherPreset::Thunderstorm,
            fixed(0.35, 18.0),
        ),
        shot(
            "08-blizzard",
            10.0,
            WeatherPreset::Blizzard,
            fixed(0.35, 6.0),
        ),
        // Paired shots. In each pair the camera is on the same body; the first
        // should show it, the second must not, because weather is in the way.
        shot("09-sun-glare", 8.0, WeatherPreset::FewClouds, Aim::Sun),
        shot(
            "10-sun-behind-storm",
            8.0,
            WeatherPreset::Thunderstorm,
            Aim::Sun,
        ),
        shot("11-moon-gibbous", 23.0, WeatherPreset::Clear, Aim::Moon),
        shot(
            "12-moon-behind-storm",
            23.0,
            WeatherPreset::Thunderstorm,
            Aim::Moon,
        ),
        // Phases, to check the terminator falls where the geometry says it
        // should rather than being painted on.
        at_phase(
            shot("13-moon-crescent", 20.0, WeatherPreset::Clear, Aim::Moon),
            0.10,
        ),
        at_phase(
            shot("14-moon-quarter", 18.0, WeatherPreset::Clear, Aim::Moon),
            0.25,
        ),
        at_phase(
            shot("15-moon-full", 1.0, WeatherPreset::Clear, Aim::Moon),
            0.50,
        ),
        // Fog, at ground level where you would actually stand in it.
        shot("16-fog-midday", 12.0, WeatherPreset::Fog, fixed(0.35, 2.0)),
        shot("17-fog-dawn", 6.5, WeatherPreset::Fog, fixed(0.35, 2.0)),
        shot(
            "18-misty-morning",
            7.5,
            WeatherPreset::MistyMorning,
            fixed(0.35, 4.0),
        ),
        // How far into the night the stars take to arrive.
        shot(
            "19-stars-dusk",
            21.0,
            WeatherPreset::Clear,
            fixed(0.35, 40.0),
        ),
        shot(
            "20-stars-late",
            2.0,
            WeatherPreset::Clear,
            fixed(0.35, 40.0),
        ),
        shot(
            "21-stars-moonlit",
            23.0,
            WeatherPreset::Clear,
            fixed(2.6, 40.0),
        ),
    ]
}

/// Frames to let each configuration settle before capturing. The atmosphere
/// LUTs and the environment map both need a few frames.
const SETTLE_FRAMES: u32 = 30;

/// Extra frames before the very first shot, while the window and the pipeline
/// caches come up.
const WARMUP_FRAMES: u32 = 90;

/// Frames to keep running after the last shot, so its readback can complete.
const DRAIN_FRAMES: u32 = 120;

/// Width and height of the offscreen target, in pixels.
const CAPTURE_SIZE: (u32, u32) = (1280, 720);

#[derive(Resource)]
struct Capture {
    /// The shot list, built once at startup.
    shots: Vec<Shot>,
    directory: String,
    index: usize,
    frame: u32,
    requested: bool,
    /// Renders go to an offscreen image rather than the window. Screenshotting
    /// the window is at the mercy of the compositor -- on macOS a backgrounded
    /// or occluded window hands back solid black -- and an unattended capture
    /// run has no way to notice.
    target: Handle<Image>,
}

fn main() {
    let directory = std::env::args().nth(1).unwrap_or_else(|| ".".into());

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "bevy_weather capture".into(),
                // The window is only here to give us a GPU context; the
                // captures come off an offscreen target.
                resolution: WindowResolution::new(480, 270),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(WeatherPlugin {
            time: WeatherTime {
                paused: true,
                latitude: 45.0,
                // Northern summer, so the sun actually climbs.
                year_offset: 172.0,
                moon_phase_offset: 0.34,
                ..default()
            },
            procedural: ProceduralWeather {
                enabled: std::env::var("FLICKER").is_ok(),
                systems_per_day: 1.2,
                ..default()
            },
            config: WeatherConfig {
                quality: Quality::High,
                atmosphere: std::env::var("NO_ATMO").is_err(),
                ..default()
            },
            ..default()
        })
        // A fixed simulated time step. Without it every frame advances the
        // world by however long the *previous* frame happened to take, so two
        // runs of the same scene see different amounts of weather between
        // captures -- and any frame-to-frame comparison is measuring the
        // machine's mood as much as the renderer's stability.
        .insert_resource(TimeUpdateStrategy::ManualDuration(
            core::time::Duration::from_secs_f64(1.0 / 60.0),
        ))
        .insert_resource(CaptureDirectory(directory))
        .add_systems(Startup, setup)
        .add_systems(Update, run_capture)
        .run();
}

#[derive(Resource)]
struct CaptureDirectory(String);

#[derive(Component)]
struct CaptureCamera;

#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy startup system building the harness's whole scene"
)]
fn setup(
    mut commands: Commands,
    mut moon: ResMut<MoonConfig>,
    mut sun: ResMut<bevy_weather::celestial::SunConfig>,
    mut atmosphere: ResMut<bevy_weather::atmosphere::AtmosphereConfig>,
    mut stars: ResMut<StarConfig>,
    mut meteors: ResMut<MeteorConfig>,
    mut cloud_shadows: ResMut<CloudShadowConfig>,
    mut galaxy: ResMut<GalaxyConfig>,
    mut clouds: ResMut<bevy_weather::clouds::CloudConfig>,
    mut config: ResMut<WeatherConfig>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    directory: Res<CaptureDirectory>,
) {
    // Blow the moon up so its phase, terminator and maria can be inspected.
    if std::env::var("BIG_MOON").is_ok() {
        moon.angular_radius = 0.12;
    }
    if std::env::var("BEVY_CASCADES").is_ok() {
        // Bevy's stock cascade layout, for comparison.
        sun.shadow_near_distance = 10.0;
        sun.shadow_distance = 150.0;
        sun.shadow_cascade_overlap = 0.2;
    }
    if let Ok(v) = std::env::var("SHADOWTILE") {
        cloud_shadows.tile_size = v.parse().unwrap_or(4_000.0);
    }
    if std::env::var("NO_CLOUD_SHADOWS").is_ok() {
        cloud_shadows.enabled = false;
    }
    if let Ok(v) = std::env::var("METEORS") {
        // Meteors are rare by design, so a normal run will never catch one.
        // This turns the sky into a storm so the streaks can be looked at.
        meteors.rate = v.parse().unwrap_or(600.0);
    }
    if let Ok(v) = std::env::var("HAZE") {
        atmosphere.aerosol_density = v.parse().unwrap_or(2.0);
    }
    if let Ok(v) = std::env::var("TONEMAP") {
        atmosphere.tonemapping = Some(match v.as_str() {
            "aces" => bevy::core_pipeline::tonemapping::Tonemapping::AcesFitted,
            "agx" => bevy::core_pipeline::tonemapping::Tonemapping::AgX,
            "blender" => bevy::core_pipeline::tonemapping::Tonemapping::BlenderFilmic,
            "reinhard" => bevy::core_pipeline::tonemapping::Tonemapping::ReinhardLuminance,
            _ => bevy::core_pipeline::tonemapping::Tonemapping::TonyMcMapface,
        });
    }
    if let Ok(v) = std::env::var("EV") {
        atmosphere.exposure_ev100 = Some(v.parse().unwrap_or(13.0));
    }
    if let Ok(v) = std::env::var("TINT") {
        sun.light_tint = v.parse().unwrap_or(0.35);
    }
    if let Ok(v) = std::env::var("WB") {
        moon.white_balance = v.parse().unwrap_or(1.0);
    }
    if let Ok(q) = std::env::var("QUALITY") {
        config.quality = match q.as_str() {
            "potato" => Quality::Potato,
            "low" => Quality::Low,
            "high" => Quality::High,
            "ultra" => Quality::Ultra,
            _ => Quality::Medium,
        };
    }
    if std::env::var("NO_FAST").is_ok() {
        clouds.adaptive_marching = false;
    }
    if std::env::var("FREEZE").is_ok() {
        // Nothing left that can legitimately change between frames: no wind to
        // advect the field, no shape evolution, and the clock stopped. Any
        // difference at all between consecutive frames is then the renderer
        // itself being non-deterministic.
        clouds.evolution_rate = 0.0;
        clouds.wind_multiplier = 0.0;
    }
    if std::env::var("NO_STARS").is_ok() {
        stars.enabled = false;
        galaxy.enabled = false;
    }

    let mut target = Image::new_target_texture(
        CAPTURE_SIZE.0,
        CAPTURE_SIZE.1,
        TextureFormat::Rgba8Unorm,
        Some(TextureFormat::Rgba8UnormSrgb),
    );
    target.asset_usage = RenderAssetUsages::RENDER_WORLD;
    let target = images.add(target);

    commands.insert_resource(Capture {
        shots: shots(),
        directory: directory.0.clone(),
        index: 0,
        frame: 0,
        requested: false,
        target: target.clone(),
    });

    commands.spawn((
        Camera3d::default(),
        RenderTarget::Image(target.into()),
        WeatherCamera,
        CaptureCamera,
        Transform::from_xyz(0.0, 4.0, 40.0),
    ));

    commands.spawn((
        Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(3_000.0)))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.15, 0.18, 0.12),
            perceptual_roughness: 0.95,
            ..default()
        })),
    ));

    let pillar = meshes.add(Cuboid::new(2.0, 16.0, 2.0));
    let stone = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.53, 0.5),
        perceptual_roughness: 0.8,
        ..default()
    });
    for i in 0..10 {
        let angle = i as f32 / 10.0 * std::f32::consts::TAU;
        commands.spawn((
            Mesh3d(pillar.clone()),
            MeshMaterial3d(stone.clone()),
            Transform::from_xyz(angle.cos() * 22.0, 8.0, angle.sin() * 22.0),
        ));
    }
}

fn run_capture(
    mut commands: Commands,
    mut capture: ResMut<Capture>,
    mut weather_time: ResMut<WeatherTime>,
    mut weather: ResMut<Weather>,
    mut cameras: Query<&mut Transform, With<CaptureCamera>>,
    wind: Res<Wind>,
    mut exit: MessageWriter<AppExit>,
) {
    let flicker = std::env::var("FLICKER").is_ok();
    let Some(&Shot {
        name,
        hour,
        preset,
        aim,
        phase,
    }) = capture.shots.get(capture.index)
    else {
        // Screenshot readback is asynchronous: the GPU copy lands a few frames
        // after it is requested, and `save_to_disk` only runs then. Exiting the
        // moment the last shot is *requested* closes the channel underneath it
        // and silently loses the final images.
        capture.frame += 1;
        if capture.frame > DRAIN_FRAMES {
            exit.write(AppExit::Success);
        }
        return;
    };

    if std::env::var("FREEZE").is_ok() {
        weather.set_immediate(WeatherConditions {
            wind_speed: 0.0,
            ..WeatherPreset::PartlyCloudy.conditions()
        });
    }

    if flicker {
        // Deliberately not a frozen scene. The showcase runs its clock fast and
        // leaves the procedural driver on, so the cloud layer's altitude,
        // thickness and coverage are all being rewritten every frame. That is
        // the state to test stability in; a paused one hides anything that only
        // goes wrong while the weather is moving.
        if std::env::var("FREEZE").is_err() {
            weather_time.paused = false;
            weather_time.day_length_secs = 180.0;
        }
    }

    if capture.frame == 0 {
        if !flicker || capture.index == 0 {
            weather_time.set_hour(hour);
        }
        // Reset rather than leaving a previous shot's override in place.
        weather_time.moon_phase_offset = phase.unwrap_or(0.34);
        weather.set_immediate(preset);

        // The bodies resource is only refreshed by the weather schedule, so
        // recompute it here rather than aiming at last frame's sun.
        let sky = bevy_weather::celestial::compute_celestial(&weather_time);
        let eye = Vec3::new(0.0, 4.0, 40.0);
        for mut transform in &mut cameras {
            *transform = match aim {
                Aim::Fixed { yaw, pitch } => Transform::from_translation(eye).with_rotation(
                    Quat::from_euler(EulerRot::YXZ, yaw, pitch.to_radians(), 0.0),
                ),
                Aim::Sun => Transform::from_translation(eye).looking_to(sky.sun_direction, Vec3::Y),
                Aim::Moon => {
                    Transform::from_translation(eye).looking_to(sky.moon_direction, Vec3::Y)
                }
                Aim::Aerial { yaw, pitch, height } => {
                    Transform::from_translation(Vec3::new(0.0, height, 40.0)).with_rotation(
                        Quat::from_euler(EulerRot::YXZ, yaw, pitch.to_radians(), 0.0),
                    )
                }
                Aim::IntoSun { pitch, back } => {
                    let bearing = Vec3::new(sky.sun_direction.x, 0.0, sky.sun_direction.z)
                        .normalize_or(Vec3::NEG_Z);
                    let yaw = (-bearing.x).atan2(-bearing.z);
                    Transform::from_translation(-bearing * back + Vec3::new(0.0, 1.7, 0.0))
                        .with_rotation(Quat::from_euler(
                            EulerRot::YXZ,
                            yaw,
                            pitch.to_radians(),
                            0.0,
                        ))
                }
                Aim::SunBearing { pitch } => {
                    let bearing = Vec3::new(sky.sun_direction.x, 0.0, sky.sun_direction.z)
                        .normalize_or(Vec3::NEG_Z);
                    // A camera looks along its local -Z, so the yaw that aims
                    // it at `bearing` solves (-sin y, -cos y) = bearing.xz.
                    let yaw = (-bearing.x).atan2(-bearing.z);
                    Transform::from_translation(eye).with_rotation(Quat::from_euler(
                        EulerRot::YXZ,
                        yaw,
                        pitch.to_radians(),
                        0.0,
                    ))
                }
            };
        }
        info!(
            "{name}: moon altitude {:.2}, wind {:.1} m/s, offset {:.1},{:.1}",
            sky.moon_altitude,
            wind.speed(),
            wind.offset.x,
            wind.offset.y,
        );
    }

    capture.frame += 1;

    // In flicker mode, one shot per app frame: the whole point is to see what
    // changes between frames the user actually sees, and leaving gaps measures
    // the weather moving instead.
    let settle = if flicker {
        if capture.index == 0 { WARMUP_FRAMES } else { 0 }
    } else {
        SETTLE_FRAMES + if capture.index == 0 { WARMUP_FRAMES } else { 0 }
    };
    if capture.frame >= settle && !capture.requested {
        capture.requested = true;
        let path = format!("{}/{}.png", capture.directory, name);
        commands
            .spawn(Screenshot::image(capture.target.clone()))
            .observe(save_to_disk(path));
    }

    // Give the screenshot a few frames to make it to disk before moving on.
    let hold = if flicker { 0 } else { 10 };
    if capture.frame >= settle + hold {
        capture.index += 1;
        capture.frame = 0;
        capture.requested = false;
    }
}
