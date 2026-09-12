# bevy_weather

A complete, reusable weather and sky plugin for [Bevy](https://bevy.org) 0.19.

Day/night cycle, procedural stars, a Milky Way, moon phases, volumetric clouds,
volumetric fog, rain, snow, thunder — and a procedural driver that generates all
of it from nothing but the clock.

```rust,no_run
use bevy::prelude::*;
use bevy_weather::prelude::*;

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, WeatherPlugin::default()))
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        // Weather is only rendered for cameras you mark.
        WeatherCamera,
        Transform::from_xyz(0.0, 2.0, 0.0).looking_at(Vec3::new(0.0, 2.0, -10.0), Vec3::Y),
    ));
}
```

That is the whole setup. The sun rises, clouds build, it rains, night falls and
the stars come out.

## What it does

| | |
|---|---|
| **Day/night cycle** | Real solar geometry from latitude, axial tilt and date. Seasons and day length fall out of it. |
| **Atmosphere** | Bevy's built-in Bruneton scattering, wired up and quality-scaled. |
| **Stars** | Procedural, with a realistic magnitude distribution, colour temperature and horizon-weighted scintillation. Fixed to the celestial sphere, so they rotate about the pole and drift by sidereal time. |
| **Galaxy** | Procedural Milky Way with dust lanes and a central bulge, at the correct angle to the celestial equator. |
| **Moon** | Phase-correct, because the terminator is computed from real geometry rather than masked. Procedural maria, craters and earthshine. |
| **Volumetric clouds** | Raymarched through a curved planetary shell, with light marching, Henyey–Greenstein phase, powder and wind advection. |
| **Volumetric fog** | Bevy's fog volumes and god rays, driven by weather and time of day. |
| **Rain and snow** | GPU-resident particle fields. One draw call each, tens of thousands of particles, world-anchored. |
| **Thunder** | Poisson-scheduled strikes with multi-stroke flash envelopes, scene lighting, in-cloud glow and speed-of-sound thunder delay. |
| **Procedural weather** | Climate-driven, and a pure function of the clock. |

## The procedural driver

Weather is generated from the clock, not accumulated frame to frame:

```rust,no_run
# use bevy::prelude::*;
# use bevy_weather::prelude::*;
# let mut app = App::new();
app.add_plugins(WeatherPlugin {
    procedural: ProceduralWeather {
        climate: Climate::OCEANIC,
        seed: 1234,
        // Roughly how many weather systems pass per in-game day.
        systems_per_day: 1.5,
        ..default()
    },
    time: WeatherTime {
        latitude: 57.0,
        day_length_secs: 600.0,
        ..default()
    },
    ..default()
});
```

Because it is a pure function, the same clock and seed always give the same sky:
on every machine, after a save/load, and on every client in a multiplayer game.
It also means you can ask what the weather *will* be:

```rust
# use bevy_weather::prelude::*;
# let driver = ProceduralWeather::default();
# let now = WeatherTime::default();
let this_evening = driver.forecast(&now, 8.0);
if this_evening.rain > 0.5 {
    // tell the player to pack a coat
}
```

Climates ship for temperate, oceanic, arid, tropical, Mediterranean, polar and
desert regions, and `Climate` is a plain struct if none of them fit.

## Driving it yourself

### From your own timeline

Set `paused` and write `time_of_day` — the "time progression ratio" — from
whatever drives your game:

```rust,no_run
# use bevy::prelude::*;
# use bevy_weather::prelude::*;
fn scrub(mut time: ResMut<WeatherTime>, cutscene_progress: f32) {
    time.paused = true;
    time.time_of_day = cutscene_progress; // 0.0 midnight, 0.5 noon
}
```

### Manual weather

Turn the driver off and set conditions directly. Presets ease in over
`Weather::transition_half_life`:

```rust,no_run
# use bevy::prelude::*;
# use bevy_weather::prelude::*;
fn make_it_rain(mut procedural: ResMut<ProceduralWeather>, mut weather: ResMut<Weather>) {
    procedural.enabled = false;
    weather.set(WeatherPreset::Thunderstorm);
}
```

Or build conditions field by field — nothing forces you into a preset:

```rust
# use bevy_weather::prelude::*;
let mut thundersnow = WeatherPreset::Thunderstorm.conditions();
thundersnow.rain = 0.0;
thundersnow.snow = 1.0;
thundersnow.temperature = -8.0;
```

### Thunder

The plugin owns the timing; you supply the sound. Strikes are scheduled as a
Poisson process, and each clap is delayed by the time sound actually takes to
reach the camera, then attenuated for distance:

```rust,no_run
# use bevy::prelude::*;
# use bevy_weather::prelude::*;
fn setup_thunder(mut thunder: ResMut<ThunderConfig>, assets: Res<AssetServer>) {
    thunder.sound = Some(assets.load("sounds/thunder.ogg"));
}
```

Requires the `audio` feature (on by default). Without a sound set, nothing is
played and you can drive audio yourself from the message below.

### Reacting to lightning

```rust,no_run
# use bevy::prelude::*;
# use bevy_weather::prelude::*;
fn on_strike(mut strikes: MessageReader<LightningStrike>) {
    for strike in strikes.read() {
        // `thunder_delay` is already computed from the speed of sound.
        println!("strike {:.0}m away, thunder in {:.1}s", strike.distance, strike.thunder_delay);
    }
}
```

## Configuration

Every subsystem has its own resource, all mutable at runtime:

| Resource | Controls |
|---|---|
| `WeatherConfig` | Master on/off switches, quality, unit scale |
| `WeatherTime` | Clock, latitude, axial tilt, moon phase offset |
| `Weather` | Current and target conditions |
| `ProceduralWeather` | The driver, climate and seed |
| `AtmosphereConfig` | Scattering density, exposure, tonemapping |
| `StarConfig`, `GalaxyConfig`, `MoonConfig` | The night sky |
| `CloudConfig` | Cloud look and raymarch cost |
| `FogConfig` | Fog colour, visibility range and volume size |
| `PrecipitationConfig` | Particle counts, sizes, speeds |
| `ThunderConfig` | Strike rate, distance, flash |

`WeatherConfig::quality` sets sensible sample counts for everything at once;
each subsystem can still override its own.

## Ordering your systems

```rust,no_run
# use bevy::prelude::*;
# use bevy_weather::prelude::*;
# let mut app = App::new();
# fn my_system() {}
app.add_systems(
    Update,
    my_system
        .after(WeatherSystems::Simulate)
        .before(WeatherSystems::Apply),
);
```

`Tick` advances the clock, `Simulate` produces the weather, `Apply` pushes it
into lights, materials, fog and particles.

## How the sky is put together

Worth knowing if you plan to extend it, because the ordering is not arbitrary.

Bevy 0.19 renders its atmosphere as a full-screen pass between the opaque and
transparent passes, compositing as `destination * transmittance + inscattering`.
This plugin draws on both sides of it:

* **Stars, galaxy and moon** are drawn in the **opaque** pass, at the far plane,
  with depth writes off. The atmosphere then composites over them, so they are
  extinguished by exactly the right amount of air with no code on our side.
* **Clouds** are drawn in the **transparent** pass, *after* the atmosphere, with
  their own aerial perspective. They have to be: the atmosphere adds the
  inscattering of the whole column up to space as though the cloud were not
  there, so a deck drawn underneath it can never be darker than the clear sky —
  an overcast day would come out brighter than a sunny one.

Both are one full-screen triangle with no model transform, so a single entity
serves every camera and there is nothing to keep in sync with camera movement.

Everything the shaders emit is in physical radiance and is multiplied by
`view.exposure`, matching the atmosphere.

## Notes and limitations

* **Exposure.** The atmosphere is calibrated in physical units, so
  `AtmosphereConfig` sets `Exposure { ev100: 13.0 }` and ACES tonemapping on
  weather cameras by default. Set `exposure_ev100: None` to manage exposure
  yourself.
* **The night sky is deliberately exaggerated.** A point star has no meaningful
  radiance at raster resolution — the value depends entirely on the solid angle
  of a pixel — and a real moonlit night is a fraction of a lux, which at a fixed
  daylight exposure is indistinguishable from black. So stars, the galaxy and
  the night ambient are all scaled well past life, and faded out explicitly
  across twilight rather than being drowned by daylight on their own.
* **Clouds are a sky-layer effect.** They are raymarched through a curved
  planetary shell above the camera and read correctly from the ground or a hill.
  Flying *through* the layer works but is not the case it is tuned for, and they
  do not cast shadows on the ground.
* **Fog is two effects.** `DistanceFog` and a matching fade in the sky shader do
  the visibility work, because they blend toward a colour — which is what makes
  heavy fog a white-out rather than a black-out. The volumetric pass adds god
  rays on top at a much lower density. Bevy's fog volumes have hard faces, so
  keep `FogConfig::volume_size` comfortably larger than your visibility distance
  or you will see the box.
* **Volumetric fog needs WebGPU** on the web; the `WebGL2` backend cannot run it.
  Turn `volumetric_fog` off there; `distance_fog` alone still looks right.
* **The sky renders for every 3D camera.** If you have a minimap or
  render-to-texture camera that must not see it, put the sky entities on their
  own render layer.

## Feature flags

* `audio` *(default)* — pulls in `bevy_audio` so the plugin can play a thunder
  clap you supply, delayed and attenuated for distance. Without it,
  `LightningStrike` still carries `thunder_delay` so you can do it yourself.

## Examples

```bash
cargo run --example showcase --release
```

An interactive tour: fly around with `WASD` and the mouse, scrub the clock,
cycle presets and climates, and toggle each subsystem. The on-screen panel lists
the keys and shows the live weather state.

## License

MIT OR Apache-2.0, at your option.
