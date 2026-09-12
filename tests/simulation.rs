//! End-to-end tests for the simulation half of the plugin.
//!
//! The unit tests cover the maths as pure functions; these run the actual
//! systems in an actual `App`, which is what catches missing resources, bad
//! system ordering and plugins that panic on startup. The rendering plugins
//! need a GPU and so are not included here -- they are covered by the
//! `capture` example instead.

use bevy::MinimalPlugins;
use bevy::app::App;
use bevy::time::{Time, TimePlugin, TimeUpdateStrategy};
use std::time::Duration;

use bevy::light::{DirectionalLight, SunDisk};
use bevy_weather::prelude::*;
use bevy_weather::{
    CorePlugin, celestial::CelestialBodies, celestial::CelestialPlugin,
    procedural::ProceduralWeatherPlugin, state::WeatherStatePlugin, time::WeatherTimePlugin,
    wind::WindPlugin,
};

/// An app with everything that does not need a renderer.
fn simulation_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins((
            CorePlugin,
            WeatherTimePlugin,
            WeatherStatePlugin,
            ProceduralWeatherPlugin,
            WindPlugin,
        ))
        // Fixed steps, so the tests are not at the mercy of how fast the
        // machine running them happens to be.
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            16,
        )));
    app
}

#[test]
fn the_plugin_stack_builds_and_runs() {
    let mut app = simulation_app();
    for _ in 0..120 {
        app.update();
    }
    // Having got here without a panic, the resources should all be present.
    assert!(app.world().get_resource::<WeatherTime>().is_some());
    assert!(app.world().get_resource::<Weather>().is_some());
    assert!(app.world().get_resource::<Wind>().is_some());
    assert!(app.world().get_resource::<ProceduralWeather>().is_some());
}

#[test]
fn the_clock_advances() {
    let mut app = simulation_app();
    app.world_mut()
        .resource_mut::<WeatherTime>()
        .day_length_secs = 10.0;
    let start = app.world().resource::<WeatherTime>().elapsed_days();
    for _ in 0..60 {
        app.update();
    }
    let end = app.world().resource::<WeatherTime>().elapsed_days();
    assert!(end > start, "{start} -> {end}");
}

#[test]
fn pausing_stops_the_clock() {
    let mut app = simulation_app();
    {
        let mut time = app.world_mut().resource_mut::<WeatherTime>();
        time.paused = true;
        time.time_of_day = 0.42;
    }
    for _ in 0..60 {
        app.update();
    }
    let time = app.world().resource::<WeatherTime>();
    assert_eq!(time.time_of_day, 0.42);
    assert_eq!(time.day, 0);
}

#[test]
fn the_procedural_driver_keeps_the_weather_valid() {
    let mut app = simulation_app();
    app.world_mut()
        .resource_mut::<WeatherTime>()
        .day_length_secs = 2.0;
    for _ in 0..600 {
        app.update();
        let weather = app.world().resource::<Weather>();
        assert_eq!(weather.current, weather.current.sanitized());
        assert_eq!(weather.target, weather.target.sanitized());
    }
}

#[test]
fn disabling_the_driver_hands_control_back() {
    let mut app = simulation_app();
    app.world_mut().resource_mut::<ProceduralWeather>().enabled = false;
    {
        let mut weather = app.world_mut().resource_mut::<Weather>();
        weather.set(WeatherPreset::Blizzard);
    }
    for _ in 0..30 {
        app.update();
    }
    assert_eq!(
        app.world().resource::<Weather>().target,
        WeatherPreset::Blizzard.conditions(),
        "the driver overwrote a manually set target"
    );
}

#[test]
fn weather_eases_toward_its_target_rather_than_snapping() {
    let mut app = simulation_app();
    {
        let mut procedural = app.world_mut().resource_mut::<ProceduralWeather>();
        procedural.enabled = false;
    }
    {
        let mut weather = app.world_mut().resource_mut::<Weather>();
        *weather = Weather::new(WeatherPreset::Clear.conditions());
        weather.transition_half_life = 1.0;
        weather.set(WeatherPreset::Storm);
    }

    // A few frames in, the transition should have started but be nowhere near
    // finished. (Bevy's very first frame reports a zero delta, so this cannot
    // be a single update.)
    for _ in 0..5 {
        app.update();
    }
    let target = WeatherPreset::Storm.conditions().rain;
    let early = app.world().resource::<Weather>().current.rain;
    assert!(
        early > 0.0 && early < target * 0.25,
        "should have barely started, got {early} of {target}"
    );

    // Ten half-lives is comfortably converged.
    for _ in 0..700 {
        app.update();
    }
    assert!(
        app.world().resource::<Weather>().is_settled(),
        "never converged"
    );
}

#[test]
fn wind_follows_the_weather() {
    let mut app = simulation_app();
    app.world_mut().resource_mut::<ProceduralWeather>().enabled = false;
    {
        let mut weather = app.world_mut().resource_mut::<Weather>();
        weather.set_immediate(WeatherPreset::Blizzard);
    }
    for _ in 0..30 {
        app.update();
    }
    let wind = app.world().resource::<Wind>();
    assert!(
        wind.speed() > 5.0,
        "a blizzard should be windy: {}",
        wind.speed()
    );
    assert!(wind.offset.length() > 0.0, "wind should have displaced");
}

#[test]
fn the_wind_offset_stays_bounded_over_a_long_session() {
    // The offset feeds shader-side `f32` maths, so it must wrap rather than
    // grow without limit.
    let mut app = simulation_app();
    app.world_mut().resource_mut::<ProceduralWeather>().enabled = false;
    {
        let mut weather = app.world_mut().resource_mut::<Weather>();
        weather.set_immediate(WeatherPreset::Blizzard);
    }
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(60)));
    for _ in 0..2_000 {
        app.update();
        let wind = app.world().resource::<Wind>();
        let wrap = wind.offset_wrap;
        assert!(
            wind.offset.x.abs() <= wrap && wind.offset.y.abs() <= wrap,
            "offset escaped its wrap: {:?}",
            wind.offset
        );
    }
}

#[test]
fn celestial_bodies_default_before_any_render_systems_run() {
    // `CelestialPlugin` is a render-side plugin, but the resource it drives is
    // registered by `CorePlugin` so that headless code can still read it.
    let app = simulation_app();
    let bodies = app.world().resource::<CelestialBodies>();
    assert!((bodies.sun_direction.length() - 1.0).abs() < 1e-5);
}

#[test]
fn scrubbing_the_clock_backwards_and_forwards_is_stable() {
    let mut app = simulation_app();
    app.world_mut().resource_mut::<WeatherTime>().paused = true;

    let sample_at = |app: &mut App, hour: f32| {
        app.world_mut().resource_mut::<WeatherTime>().set_hour(hour);
        app.update();
        app.world().resource::<Weather>().target
    };

    let forwards = sample_at(&mut app, 14.0);
    let _ = sample_at(&mut app, 3.0);
    let backwards = sample_at(&mut app, 14.0);
    assert_eq!(
        forwards, backwards,
        "the same clock reading gave different weather"
    );
}

#[test]
fn time_and_weather_survive_a_zero_length_day() {
    // A `day_length_secs` of zero means "do not advance", not "divide by zero".
    let mut app = simulation_app();
    app.world_mut()
        .resource_mut::<WeatherTime>()
        .day_length_secs = 0.0;
    for _ in 0..30 {
        app.update();
    }
    let time = app.world().resource::<WeatherTime>();
    assert!(time.time_of_day.is_finite());
    assert_eq!(time.day, 0);
}

#[test]
fn a_zero_delta_frame_does_not_break_the_blend() {
    let mut app = App::new();
    app.add_plugins(TimePlugin)
        .add_plugins((CorePlugin, WeatherStatePlugin))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
    app.world_mut()
        .resource_mut::<Weather>()
        .set(WeatherPreset::Storm);
    for _ in 0..10 {
        app.update();
    }
    let weather = app.world().resource::<Weather>();
    assert!(weather.current.rain.is_finite());
    let _ = app.world().resource::<Time>();
}

#[test]
fn a_full_year_of_simulation_never_produces_invalid_state() {
    let mut app = simulation_app();
    app.world_mut()
        .resource_mut::<WeatherTime>()
        .day_length_secs = 1.0;
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        500,
    )));
    // 730 half-second steps at one second per day is a year.
    for _ in 0..730 {
        app.update();
        let weather = app.world().resource::<Weather>();
        assert_eq!(weather.current, weather.current.sanitized());
        let time = app.world().resource::<WeatherTime>();
        assert!((0.0..1.0).contains(&time.time_of_day));
    }
}

#[test]
fn the_moon_light_suppresses_the_atmosphere_sun_disc() {
    // Bevy draws a sun disc for every directional light, defaulting to
    // `SunDisk::EARTH` when the component is absent. Without an explicit
    // `SunDisk::OFF` the moon light paints a blazing sun at the moon's
    // position, which sits on top of the real moon and hides its phase.
    let mut app = simulation_app();
    app.add_plugins(CelestialPlugin);
    app.update();
    app.update();

    let mut moons = app.world_mut().query_filtered::<Option<&SunDisk>, (
        bevy::ecs::query::With<MoonLight>,
        bevy::ecs::query::With<DirectionalLight>,
    )>();

    let discs: Vec<_> = moons.iter(app.world()).collect();
    assert_eq!(discs.len(), 1, "expected exactly one moon light");
    let disc = discs[0].expect("the moon light needs an explicit SunDisk");
    assert_eq!(
        disc.intensity, 0.0,
        "the moon light must not draw an atmospheric sun disc"
    );
}

#[test]
fn the_sun_light_does_draw_a_disc() {
    let mut app = simulation_app();
    app.add_plugins(CelestialPlugin);
    app.update();
    app.update();

    let mut suns = app.world_mut().query_filtered::<Option<&SunDisk>, (
        bevy::ecs::query::With<SunLight>,
        bevy::ecs::query::With<DirectionalLight>,
    )>();
    let discs: Vec<_> = suns.iter(app.world()).collect();
    assert_eq!(discs.len(), 1, "expected exactly one sun light");
    let disc = discs[0].expect("the sun light should carry a SunDisk");
    assert!(disc.intensity > 0.0);
    assert!(disc.angular_size > 0.0);
}
