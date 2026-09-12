//! Internal visual-verification harness: renders a series of fixed
//! time-of-day / weather combinations, screenshots each, then exits.
//!
//! Not part of the public API surface; it exists so the shaders can be checked
//! on a real GPU without a human at the keyboard.
//!
//! ```sh
//! cargo run --example capture --release -- <output-directory>
//! ```

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::WindowResolution;

use bevy_weather::prelude::*;

/// `(name, hour, preset, camera pitch in degrees)`
const SHOTS: [(&str, f32, WeatherPreset, f32); 8] = [
    ("01-dawn-clear", 6.2, WeatherPreset::Clear, 6.0),
    ("02-noon-cumulus", 12.0, WeatherPreset::PartlyCloudy, 22.0),
    ("03-afternoon-overcast", 15.0, WeatherPreset::Overcast, 14.0),
    ("04-sunset-rain", 19.3, WeatherPreset::Rain, 8.0),
    ("05-night-stars", 23.5, WeatherPreset::Clear, 34.0),
    ("06-night-galaxy-up", 1.0, WeatherPreset::Clear, 65.0),
    ("07-thunderstorm", 16.0, WeatherPreset::Thunderstorm, 18.0),
    ("08-blizzard", 10.0, WeatherPreset::Blizzard, 6.0),
];

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

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    directory: Res<CaptureDirectory>,
) {
    let mut target = Image::new_target_texture(
        CAPTURE_SIZE.0,
        CAPTURE_SIZE.1,
        TextureFormat::Rgba8Unorm,
        Some(TextureFormat::Rgba8UnormSrgb),
    );
    target.asset_usage = RenderAssetUsages::RENDER_WORLD;
    let target = images.add(target);

    commands.insert_resource(Capture {
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
    let Some(&(name, hour, preset, pitch)) = SHOTS.get(capture.index) else {
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
        weather.set_immediate(preset);
        for mut transform in &mut cameras {
            // Face north-ish and tilt up by the requested amount.
            *transform = Transform::from_xyz(0.0, 4.0, 40.0).with_rotation(Quat::from_euler(
                EulerRot::YXZ,
                0.35,
                pitch.to_radians(),
                0.0,
            ));
        }
        info!("capturing {name} at {hour:.1}h ({})", preset.name());
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
