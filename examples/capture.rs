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
//! * `WB=<n>` overrides `MoonConfig::white_balance`, for checking how far the
//!   moon's colour moves between no correction and full.

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
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
    vec![
        shot("01-dawn-clear", 6.2, WeatherPreset::Clear, fixed(0.35, 6.0)),
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
                enabled: false,
                ..default()
            },
            config: WeatherConfig {
                quality: Quality::High,
                atmosphere: std::env::var("NO_ATMO").is_err(),
                ..default()
            },
            ..default()
        })
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
    mut stars: ResMut<StarConfig>,
    mut galaxy: ResMut<GalaxyConfig>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    directory: Res<CaptureDirectory>,
) {
    // Blow the moon up so its phase, terminator and maria can be inspected.
    if std::env::var("BIG_MOON").is_ok() {
        moon.angular_radius = 0.12;
    }
    if let Ok(v) = std::env::var("WB") {
        moon.white_balance = v.parse().unwrap_or(1.0);
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
        Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(800.0)))),
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
    mut exit: MessageWriter<AppExit>,
) {
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

    if capture.frame == 0 {
        weather_time.set_hour(hour);
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
            };
        }
        info!(
            "capturing {name} at {hour:.1}h ({}), moon altitude {:.2}",
            preset.name(),
            sky.moon_altitude,
        );
    }

    capture.frame += 1;

    let settle = SETTLE_FRAMES + if capture.index == 0 { WARMUP_FRAMES } else { 0 };
    if capture.frame >= settle && !capture.requested {
        capture.requested = true;
        let path = format!("{}/{}.png", capture.directory, name);
        commands
            .spawn(Screenshot::image(capture.target.clone()))
            .observe(save_to_disk(path));
    }

    // Give the screenshot a few frames to make it to disk before moving on.
    if capture.frame >= settle + 10 {
        capture.index += 1;
        capture.frame = 0;
        capture.requested = false;
    }
}
