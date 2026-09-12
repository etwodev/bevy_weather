//! An interactive tour of `bevy_weather`.
//!
//! ```sh
//! cargo run --example showcase --release
//! ```
//!
//! Fly around with the mouse and `WASD`. The on-screen panel lists the rest of
//! the keys and shows the live weather state.

use bevy::color::palettes::css;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::text::FontSize;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use bevy_weather::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "bevy_weather showcase".into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(WeatherPlugin {
            time: WeatherTime {
                // A brisk cycle, so you can watch a whole day in a few minutes.
                day_length_secs: 180.0,
                latitude: 48.0,
                ..default()
            },
            procedural: ProceduralWeather {
                climate: Climate::TEMPERATE,
                systems_per_day: 1.2,
                ..default()
            },
            ..default()
        })
        .init_resource::<Tour>()
        .add_systems(Startup, (setup_scene, setup_ui))
        .add_systems(
            Update,
            (
                fly_camera,
                toggle_cursor,
                weather_controls,
                report_lightning,
                update_ui,
            ),
        )
        .run();
}

/// Which preset and climate the manual controls are currently pointing at.
#[derive(Resource, Default)]
struct Tour {
    preset: usize,
    climate: usize,
    quality: usize,
}

const CLIMATES: [(&str, Climate); 7] = [
    ("Temperate", Climate::TEMPERATE),
    ("Oceanic", Climate::OCEANIC),
    ("Arid", Climate::ARID),
    ("Tropical", Climate::TROPICAL),
    ("Mediterranean", Climate::MEDITERRANEAN),
    ("Polar", Climate::POLAR),
    ("Desert Nights", Climate::DESERT_NIGHTS),
];

const QUALITIES: [(&str, Quality); 4] = [
    ("Low", Quality::Low),
    ("Medium", Quality::Medium),
    ("High", Quality::High),
    ("Ultra", Quality::Ultra),
];

#[derive(Component)]
struct FlyCamera {
    yaw: f32,
    pitch: f32,
    speed: f32,
}

#[derive(Component)]
struct StatusText;

fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Camera3d::default(),
        // This marker is what opts a camera into the weather.
        WeatherCamera,
        FlyCamera {
            yaw: 0.0,
            pitch: -0.06,
            speed: 12.0,
        },
        Transform::from_xyz(0.0, 3.0, 0.0),
    ));

    // Ground.
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(600.0)))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.16, 0.19, 0.13),
            perceptual_roughness: 0.95,
            ..default()
        })),
    ));

    // Something to catch the light, cast shadows into the fog, and give the
    // volumetric pass some god rays to work with.
    let pillar = meshes.add(Cuboid::new(2.0, 14.0, 2.0));
    let stone = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.53, 0.5),
        perceptual_roughness: 0.8,
        ..default()
    });
    for i in 0..12 {
        let angle = i as f32 / 12.0 * std::f32::consts::TAU;
        let radius = 26.0;
        commands.spawn((
            Mesh3d(pillar.clone()),
            MeshMaterial3d(stone.clone()),
            Transform::from_xyz(angle.cos() * radius, 7.0, angle.sin() * radius),
        ));
    }

    // A few scattered blocks to give the ground some scale.
    let block = meshes.add(Cuboid::new(3.0, 3.0, 3.0));
    for i in 0..40 {
        let x = ((i * 37) % 101) as f32 - 50.0;
        let z = ((i * 73) % 137) as f32 - 68.0;
        let height = 1.0 + (i % 5) as f32;
        commands.spawn((
            Mesh3d(block.clone()),
            MeshMaterial3d(stone.clone()),
            Transform::from_xyz(x * 2.4, height * 0.5, z * 2.4)
                .with_scale(Vec3::new(1.0, height, 1.0)),
        ));
    }
}

fn setup_ui(mut commands: Commands) {
    commands.spawn((
        StatusText,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(13.0),
            ..default()
        },
        TextColor(css::WHITE.into()),
        Node {
            position_type: PositionType::Absolute,
            top: px(10),
            left: px(10),
            ..default()
        },
    ));
}

fn toggle_cursor(
    keys: Res<ButtonInput<KeyCode>>,
    mut windows: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }
    for mut cursor in &mut windows {
        let grabbed = cursor.grab_mode != CursorGrabMode::None;
        cursor.grab_mode = if grabbed {
            CursorGrabMode::None
        } else {
            CursorGrabMode::Locked
        };
        cursor.visible = grabbed;
    }
}

fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    windows: Query<&CursorOptions, With<PrimaryWindow>>,
    mut cameras: Query<(&mut Transform, &mut FlyCamera)>,
) {
    let locked = windows
        .iter()
        .any(|cursor| cursor.grab_mode != CursorGrabMode::None);
    let looking = locked || buttons.pressed(MouseButton::Right);

    for (mut transform, mut camera) in &mut cameras {
        if looking {
            camera.yaw -= motion.delta.x * 0.0022;
            camera.pitch = (camera.pitch - motion.delta.y * 0.0022).clamp(-1.54, 1.54);
        }
        transform.rotation = Quat::from_euler(EulerRot::YXZ, camera.yaw, camera.pitch, 0.0);

        let mut direction = Vec3::ZERO;
        if keys.pressed(KeyCode::KeyW) {
            direction += *transform.forward();
        }
        if keys.pressed(KeyCode::KeyS) {
            direction += *transform.back();
        }
        if keys.pressed(KeyCode::KeyA) {
            direction += *transform.left();
        }
        if keys.pressed(KeyCode::KeyD) {
            direction += *transform.right();
        }
        if keys.pressed(KeyCode::KeyE) {
            direction += Vec3::Y;
        }
        if keys.pressed(KeyCode::KeyQ) {
            direction -= Vec3::Y;
        }

        let boost = if keys.pressed(KeyCode::ShiftLeft) {
            8.0
        } else {
            1.0
        };
        if direction != Vec3::ZERO {
            transform.translation +=
                direction.normalize() * camera.speed * boost * time.delta_secs();
        }
        // Do not fall through the floor.
        transform.translation.y = transform.translation.y.max(0.4);
    }
}

#[expect(clippy::too_many_arguments, reason = "an example's control panel")]
fn weather_controls(
    keys: Res<ButtonInput<KeyCode>>,
    mut tour: ResMut<Tour>,
    mut weather_time: ResMut<WeatherTime>,
    mut weather: ResMut<Weather>,
    mut procedural: ResMut<ProceduralWeather>,
    mut config: ResMut<WeatherConfig>,
    mut atmosphere: ResMut<AtmosphereConfig>,
    time: Res<Time>,
) {
    // ---- Clock -------------------------------------------------------------
    if keys.just_pressed(KeyCode::Space) {
        weather_time.paused = !weather_time.paused;
    }
    if keys.just_pressed(KeyCode::BracketRight) {
        weather_time.day_length_secs = (weather_time.day_length_secs * 0.5).max(5.0);
    }
    if keys.just_pressed(KeyCode::BracketLeft) {
        weather_time.day_length_secs = (weather_time.day_length_secs * 2.0).min(86_400.0);
    }
    // Scrubbing the clock directly is the "time progression ratio" in action.
    if keys.pressed(KeyCode::ArrowRight) {
        let step = time.delta_secs() * 0.15;
        weather_time.advance(step);
    }
    if keys.pressed(KeyCode::ArrowLeft) {
        let step = time.delta_secs() * 0.15;
        weather_time.advance(-step);
    }
    if keys.just_pressed(KeyCode::ArrowUp) {
        weather_time.advance(1.0);
    }
    if keys.just_pressed(KeyCode::ArrowDown) {
        weather_time.advance(-1.0);
    }

    // ---- Weather -----------------------------------------------------------
    if keys.just_pressed(KeyCode::KeyP) {
        procedural.enabled = !procedural.enabled;
    }
    if keys.just_pressed(KeyCode::KeyN) || keys.just_pressed(KeyCode::KeyM) {
        let count = WeatherPreset::ALL.len();
        tour.preset = if keys.just_pressed(KeyCode::KeyN) {
            (tour.preset + 1) % count
        } else {
            (tour.preset + count - 1) % count
        };
        // Choosing a preset means taking manual control.
        procedural.enabled = false;
        weather.transition_half_life = 3.0;
        weather.set(WeatherPreset::ALL[tour.preset]);
    }
    if keys.just_pressed(KeyCode::KeyC) {
        tour.climate = (tour.climate + 1) % CLIMATES.len();
        procedural.climate = CLIMATES[tour.climate].1;
        procedural.enabled = true;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        procedural.seed = procedural
            .seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        procedural.enabled = true;
    }

    // ---- Rendering ---------------------------------------------------------
    if keys.just_pressed(KeyCode::KeyG) {
        tour.quality = (tour.quality + 1) % QUALITIES.len();
        config.quality = QUALITIES[tour.quality].1;
    }
    if keys.just_pressed(KeyCode::Digit1) {
        config.clouds = !config.clouds;
    }
    if keys.just_pressed(KeyCode::Digit2) {
        config.volumetric_fog = !config.volumetric_fog;
    }
    if keys.just_pressed(KeyCode::Digit3) {
        config.precipitation = !config.precipitation;
    }
    if keys.just_pressed(KeyCode::Digit4) {
        config.sky = !config.sky;
    }
    if keys.just_pressed(KeyCode::Digit5) {
        config.thunder = !config.thunder;
    }
    if keys.just_pressed(KeyCode::Digit6) {
        atmosphere.raymarched = !atmosphere.raymarched;
    }
}

fn report_lightning(mut strikes: MessageReader<LightningStrike>) {
    for strike in strikes.read() {
        info!(
            "lightning {:.0} m away, thunder in {:.1} s",
            strike.distance, strike.thunder_delay
        );
    }
}

#[expect(clippy::too_many_arguments, reason = "an example's status panel")]
fn update_ui(
    mut text: Query<&mut Text, With<StatusText>>,
    tour: Res<Tour>,
    weather_time: Res<WeatherTime>,
    weather: Res<Weather>,
    procedural: Res<ProceduralWeather>,
    config: Res<WeatherConfig>,
    bodies: Res<CelestialBodies>,
    wind: Res<Wind>,
) {
    let Ok(mut text) = text.single_mut() else {
        return;
    };
    let now = weather.current;
    let hour = weather_time.hour();
    let phase_name = moon_phase_name(bodies.moon_phase);

    text.0 = format!(
        "\
bevy_weather showcase

  Day {day}, {h:02}:{m:02}   ({day_length:.0}s/day{paused})
  Sun altitude {sun:+.0} deg   daylight {daylight:.2}
  Moon {phase} ({illum:.0}% lit), altitude {moon:+.0} deg

  Mode        {mode}
  Climate     {climate}
  Quality     {quality}

  Cloud       cover {cover:.2}  density {density:.2}  base {base:.0} m  depth {depth:.0} m
  Rain {rain:.2}   Snow {snow:.2}   Fog {fog:.2}   Lightning {thunder:.1}/min
  Wind        {wind_speed:.1} m/s from {bearing:.0} deg (gust x{gust:.2})
  Air         {temp:+.1} C, {humidity:.0}% humidity

  Toggles     [1] clouds {clouds}  [2] fog {vfog}  [3] precip {precip}
              [4] sky {sky}  [5] thunder {thunder_on}  [6] raymarched sky

  Move        WASD / Q E   (Shift to sprint)   Esc grabs the mouse
  Time        Space pause   [ ] slower/faster   <- -> scrub   Up/Down +/- a day
  Weather     N / M preset   P procedural   C climate   R reseed   G quality",
        day = weather_time.day,
        h = hour as u32,
        m = ((hour - hour.floor()) * 60.0) as u32,
        day_length = weather_time.day_length_secs,
        paused = if weather_time.paused { ", paused" } else { "" },
        sun = bodies.sun_altitude.asin().to_degrees(),
        daylight = bodies.daylight,
        phase = phase_name,
        illum = bodies.moon_illumination * 100.0,
        moon = bodies.moon_altitude.asin().to_degrees(),
        mode = if procedural.enabled {
            "procedural"
        } else {
            WeatherPreset::ALL[tour.preset].name()
        },
        climate = CLIMATES[tour.climate].0,
        quality = QUALITIES[tour.quality].0,
        cover = now.cloud_coverage,
        density = now.cloud_density,
        base = now.cloud_altitude,
        depth = now.cloud_thickness,
        rain = now.rain,
        snow = now.snow,
        fog = now.fog,
        thunder = now.thunder,
        wind_speed = wind.speed(),
        bearing = now.wind_direction.to_degrees(),
        gust = wind.gust,
        temp = now.temperature,
        humidity = now.humidity * 100.0,
        clouds = on_off(config.clouds),
        vfog = on_off(config.volumetric_fog),
        precip = on_off(config.precipitation),
        sky = on_off(config.sky),
        thunder_on = on_off(config.thunder),
    );
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn moon_phase_name(phase: f32) -> &'static str {
    match (phase * 8.0).round() as u32 % 8 {
        0 => "new",
        1 => "waxing crescent",
        2 => "first quarter",
        3 => "waxing gibbous",
        4 => "full",
        5 => "waning gibbous",
        6 => "last quarter",
        _ => "waning crescent",
    }
}
