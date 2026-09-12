// Sky dome: stars, galaxy, moon and volumetric clouds.
//
// One shader, two passes, because deep space and the cloud deck sit on
// opposite sides of Bevy's atmosphere pass.
//
// `SPACE_PASS` runs in the *opaque* pass, at the far plane, with depth writes
// off. Bevy's atmosphere then composites over it as
// `dst * transmittance + inscattering`, so the stars, the galaxy and the moon
// are extinguished by exactly the right amount of air, for free.
//
// `CLOUD_PASS` runs in the *transparent* pass, which is after the atmosphere.
// It has to be: the atmosphere adds the inscattering of the whole column up to
// space, as though the cloud were not there, so a deck drawn underneath it can
// never be darker than the clear sky. An overcast day would come out brighter
// than a sunny one. Drawing the clouds afterwards, with their own aerial
// perspective, is the only way to get grey weather.

#import bevy_pbr::mesh_view_bindings::view

struct SkyUniform {
    // xyz: unit vector toward the sun. w: angular radius, radians.
    sun_direction: vec4<f32>,
    // xyz: unit vector toward the moon. w: angular radius, radians.
    moon_direction: vec4<f32>,
    // rgb: sun colour after atmospheric reddening. a: daylight, 0..1.
    sun_color: vec4<f32>,
    // rgb: moonlight colour. a: lit fraction of the disc, 0..1.
    moon_color: vec4<f32>,

    // x: cells per cube face. y: brightness. z: twinkle rate. w: point size.
    star_params: vec4<f32>,
    // x: colour spread. y: density cutoff. z: twinkle depth.
    // w: magnitude falloff exponent.
    star_params2: vec4<f32>,

    // x: brightness. y: noise scale. z: dust strength. w: band tightness.
    galaxy_params: vec4<f32>,
    galaxy_core_color: vec4<f32>,
    galaxy_edge_color: vec4<f32>,
    // xyz: galactic pole in the fixed star frame. w: core concentration.
    galaxy_axis: vec4<f32>,
    // xyz: galactic centre in the fixed star frame. w: unused.
    galaxy_center: vec4<f32>,

    // x: brightness. y: earthshine. z: surface contrast. w: phase 0..1.
    moon_params: vec4<f32>,
    moon_tint: vec4<f32>,

    // x: coverage. y: density. z: base altitude, m. w: thickness, m.
    cloud_params0: vec4<f32>,
    // x: shape scale, m. y: detail scale, m. z: detail strength. w: extinction.
    cloud_params1: vec4<f32>,
    // x: forward g. y: back g. z: powder. w: ambient.
    cloud_params2: vec4<f32>,
    // x: exposure. y: horizon fade. z: march steps. w: light steps.
    cloud_params3: vec4<f32>,
    cloud_albedo: vec4<f32>,
    // xyz: wind displacement, m. w: shape evolution, m.
    cloud_offset: vec4<f32>,

    // rgb: flash colour and strength. a: how much it lights the cloud interior.
    lightning: vec4<f32>,

    // rgb: ground-fog colour, already in post-exposure units. a: unused.
    fog_color: vec4<f32>,
    // x: extinction per world unit. y: fog layer height, world units.
    // z: forward-scattering glow strength. w: glow exponent.
    fog_params: vec4<f32>,

    // x: seconds, wrapped. y: planet radius, m. z: world units per metre.
    // w: bitfield -- 1 stars, 2 galaxy, 4 moon, 8 clouds.
    misc: vec4<f32>,

    // Rotates world directions into the fixed frame the stars live in.
    star_from_world: mat3x3<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> sky: SkyUniform;

const FLAG_STARS: u32 = 1u;
const FLAG_GALAXY: u32 = 2u;
const FLAG_MOON: u32 = 4u;
const FLAG_CLOUDS: u32 = 8u;

const PI: f32 = 3.14159265359;
const MAX_MARCH_DISTANCE: f32 = 160000.0;

// Bevy's atmosphere works in physical radiance and applies `view.exposure` to
// its own inscattering, so anything composited underneath it has to be in the
// same units or it will be invisible. These are the two anchors:
//
// Direct sunlight at the ground is roughly 130 klx; a perfectly white diffuse
// surface under it has a radiance of E/pi.
const SUNLIT_RADIANCE: f32 = 41000.0;
// A clear daytime sky is around 15 klx, so its radiance is far lower.
const SKY_RADIANCE: f32 = 5000.0;

struct Vertex {
    @location(0) position: vec3<f32>,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    // The mesh is a single oversized triangle already in normalised device
    // coordinates, so it needs no model or view transform at all. That is what
    // lets one sky entity serve every camera in the scene at once, instead of
    // needing a dome parented to each.
    out.ndc = vertex.position.xy;
    // Reverse-Z: zero is the far plane. Pinning Z there makes this behave
    // exactly like a skybox -- it can never be clipped by the far plane, and it
    // always loses the depth test against real geometry.
    out.position = vec4(vertex.position.xy, 0.0, 1.0);
    return out;
}

/// Unprojects a point on the near plane to get the world-space view ray.
fn ray_from_ndc(ndc: vec2<f32>) -> vec3<f32> {
    // Reverse-Z again: the near plane is at z = 1.
    let world = view.world_from_clip * vec4(ndc, 1.0, 1.0);
    return normalize(world.xyz / world.w - view.world_position);
}

// ---------------------------------------------------------------------------
// Noise
// ---------------------------------------------------------------------------

fn hash13(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.1031);
    q += dot(q, q.zyx + 31.32);
    return fract((q.x + q.y) * q.z);
}

fn hash33(p: vec3<f32>) -> vec3<f32> {
    var q = fract(p * vec3(0.1031, 0.1030, 0.0973));
    q += dot(q, q.yxz + 33.33);
    return fract((q.xxy + q.yxx) * q.zyx);
}

fn hash21(p: vec2<f32>) -> f32 {
    var q = fract(vec3(p.xyx) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
}

fn hash23(p: vec2<f32>, salt: f32) -> vec3<f32> {
    return hash33(vec3(p, salt));
}

fn value_noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);

    let n000 = hash13(i + vec3(0.0, 0.0, 0.0));
    let n100 = hash13(i + vec3(1.0, 0.0, 0.0));
    let n010 = hash13(i + vec3(0.0, 1.0, 0.0));
    let n110 = hash13(i + vec3(1.0, 1.0, 0.0));
    let n001 = hash13(i + vec3(0.0, 0.0, 1.0));
    let n101 = hash13(i + vec3(1.0, 0.0, 1.0));
    let n011 = hash13(i + vec3(0.0, 1.0, 1.0));
    let n111 = hash13(i + vec3(1.0, 1.0, 1.0));

    let x00 = mix(n000, n100, u.x);
    let x10 = mix(n010, n110, u.x);
    let x01 = mix(n001, n101, u.x);
    let x11 = mix(n011, n111, u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

fn fbm3(p: vec3<f32>, octaves: i32) -> f32 {
    var sum = 0.0;
    var amplitude = 0.5;
    var total = 0.0;
    var q = p;
    for (var i = 0; i < octaves; i += 1) {
        sum += value_noise3(q) * amplitude;
        total += amplitude;
        amplitude *= 0.5;
        // Rotating between octaves hides the axis-aligned grid of the
        // underlying value noise.
        q = q * 2.03 + vec3(17.3, 5.1, 41.7);
    }
    return sum / total;
}

/// Ridged, billowy noise. Cloud tops are built from bulges, not from the
/// smooth blobs plain fbm gives you.
fn billow3(p: vec3<f32>, octaves: i32) -> f32 {
    var sum = 0.0;
    var amplitude = 0.5;
    var total = 0.0;
    var q = p;
    for (var i = 0; i < octaves; i += 1) {
        sum += abs(value_noise3(q) * 2.0 - 1.0) * amplitude;
        total += amplitude;
        amplitude *= 0.5;
        q = q * 2.11 + vec3(3.7, 29.1, 11.3);
    }
    return sum / total;
}

fn remap(x: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    return c + (saturate((x - a) / max(b - a, 1e-6))) * (d - c);
}

// ---------------------------------------------------------------------------
// Stars
// ---------------------------------------------------------------------------

/// Projects a direction onto the face of a cube.
///
/// Returns `(u, v, face)` with `u`/`v` in `[-1, 1]`. Hashing in this space
/// gives a star distribution that is even across the sky and, unlike hashing
/// the raw direction, has no cell-boundary clipping.
fn cube_face(dir: vec3<f32>) -> vec3<f32> {
    let a = abs(dir);
    if a.x >= a.y && a.x >= a.z {
        let face = select(1.0, 0.0, dir.x > 0.0);
        return vec3(dir.z / a.x, dir.y / a.x, face);
    } else if a.y >= a.z {
        let face = select(3.0, 2.0, dir.y > 0.0);
        return vec3(dir.x / a.y, dir.z / a.y, face);
    }
    let face = select(5.0, 4.0, dir.z > 0.0);
    return vec3(dir.x / a.z, dir.y / a.z, face);
}

fn star_field(star_dir: vec3<f32>, horizon_factor: f32) -> vec3<f32> {
    let density = max(sky.star_params.x, 1.0);
    let brightness = sky.star_params.y;
    let twinkle_rate = sky.star_params.z;
    let size = max(sky.star_params.w, 1e-4);
    let color_spread = sky.star_params2.x;
    let cutoff = sky.star_params2.y;
    let twinkle_depth = sky.star_params2.z;
    let magnitude_falloff = max(sky.star_params2.w, 0.1);

    let face = cube_face(star_dir);
    let grid = face.xy * density;
    let cell = floor(grid);
    let local = grid - cell;

    var accumulated = vec3(0.0);

    // Three by three so a star sitting near a cell edge still contributes to
    // the neighbouring cell's pixels, instead of being cut in half.
    for (var dy = -1; dy <= 1; dy += 1) {
        for (var dx = -1; dx <= 1; dx += 1) {
            let neighbour = cell + vec2(f32(dx), f32(dy));
            let rnd = hash23(neighbour, face.z);

            // Only some cells hold a star, otherwise the sky looks like graph
            // paper.
            if rnd.x > cutoff {
                continue;
            }

            let jitter = hash23(neighbour + 7.77, face.z + 3.3);
            let offset = vec2(f32(dx), f32(dy)) + vec2(0.15, 0.15) + jitter.xy * 0.7;
            let delta = offset - local;
            let distance_squared = dot(delta, delta);

            // A power curve turns a uniform variate into something like the
            // real magnitude distribution: a handful of bright stars and a
            // great many faint ones. Too steep and every star lands in the
            // bottom few percent of the range, which renders as uniform grey
            // speckle -- recognisably noise rather than a sky.
            let magnitude = pow(rnd.y, magnitude_falloff);

            // Scintillation is an atmospheric effect, so it is strongest near
            // the horizon where you look through the most air.
            let phase = rnd.z * 100.0;
            let twinkle = 1.0
                + twinkle_depth
                    * horizon_factor
                    * sin(sky.misc.x * twinkle_rate + phase)
                    * sin(sky.misc.x * twinkle_rate * 0.37 + phase * 1.7);

            let radius = size * (0.6 + magnitude * 1.6);
            let falloff = exp(-distance_squared / (radius * radius));

            // Hotter stars are blue, cooler ones orange; the brightest tend to
            // read as blue-white.
            let temperature = mix(jitter.z, magnitude, 0.35);
            let cool = vec3(1.0, 0.72, 0.45);
            let hot = vec3(0.72, 0.80, 1.0);
            let tint = mix(vec3(1.0), mix(cool, hot, temperature), color_spread);

            accumulated += tint * magnitude * falloff * max(twinkle, 0.0);
        }
    }

    return accumulated * brightness;
}

// ---------------------------------------------------------------------------
// Galaxy
// ---------------------------------------------------------------------------

fn galaxy(star_dir: vec3<f32>) -> vec3<f32> {
    let intensity = sky.galaxy_params.x;
    let scale = max(sky.galaxy_params.y, 1e-4);
    let dust = sky.galaxy_params.z;
    let tightness = max(sky.galaxy_params.w, 0.1);
    let pole = normalize(sky.galaxy_axis.xyz);
    let concentration = sky.galaxy_axis.w;

    // Distance from the galactic plane, as an angle.
    let off_plane = abs(dot(star_dir, pole));
    let band = pow(saturate(1.0 - off_plane), tightness);
    if band < 1e-4 {
        return vec3(0.0);
    }

    // Unresolved starlight: fine-grained, bright.
    let stars = fbm3(star_dir * scale, 4);
    // Dust lanes: coarser, and they subtract.
    let lanes = fbm3(star_dir * scale * 0.35 + 19.7, 3);

    // The bulge, brightest toward the galactic centre.
    let toward_center = saturate(dot(star_dir, normalize(sky.galaxy_center.xyz)));
    let bulge = pow(toward_center, max(concentration, 0.01));

    let brightness = band * (0.35 + 0.9 * stars) * (0.35 + 1.3 * bulge);
    let obscuration = 1.0 - dust * smoothstep(0.35, 0.75, lanes) * band;

    let color = mix(sky.galaxy_edge_color.rgb, sky.galaxy_core_color.rgb, bulge);
    return color * brightness * max(obscuration, 0.0) * intensity;
}

// ---------------------------------------------------------------------------
// Moon
// ---------------------------------------------------------------------------

fn moon(ray: vec3<f32>) -> vec3<f32> {
    let moon_dir = sky.moon_direction.xyz;
    let angular_radius = max(sky.moon_direction.w, 1e-5);
    let cos_angle = dot(ray, moon_dir);
    let cos_radius = cos(angular_radius);

    // A soft edge one pixel-ish wide stops the disc from aliasing.
    let edge = angular_radius * 0.06;
    let disc = smoothstep(cos(angular_radius + edge), cos_radius, cos_angle);

    // A little glow outside the disc, from forward scattering in the air.
    // Kept tight and faint: a wide halo baked into the sky cannot be occluded
    // by anything, so it shows through cloud that should have hidden it. Real
    // glare belongs in the bloom pass, where it is applied to the composited
    // image and is hidden along with whatever produced it.
    let angle = acos(clamp(cos_angle, -1.0, 1.0));
    let halo = exp(-angle / (angular_radius * 5.0)) * 0.03;

    var result = vec3(0.0);

    if disc > 0.0 {
        // Reconstruct the surface normal of the sphere we are looking at.
        // `offset` is the position on the disc in units of its radius; the
        // remaining component points back at us.
        let tangent = normalize(cross(moon_dir, vec3(0.0, 1.0, 0.0)) + vec3(1e-5, 0.0, 0.0));
        let bitangent = cross(moon_dir, tangent);
        let offset = vec2(dot(ray, tangent), dot(ray, bitangent)) / sin(angular_radius);
        let r2 = saturate(dot(offset, offset));
        let normal = normalize(tangent * offset.x + bitangent * offset.y - moon_dir * sqrt(1.0 - r2));

        // The sun is effectively at infinity for both of us, so its direction
        // at the moon is the same as ours. That makes the terminator, and
        // therefore the phase, fall straight out of the geometry -- no separate
        // crescent mask needed.
        let lambert = saturate(dot(normal, sky.sun_direction.xyz));

        // Maria and craters, from noise on the surface point.
        let surface = fbm3(normal * 6.0, 4);
        let craters = billow3(normal * 22.0, 3);
        let albedo = mix(0.62, 1.0, surface) * mix(0.82, 1.0, craters);

        // The moon is a famously back-scattering body: it stays nearly as
        // bright at the limb as at the centre, which is why a full moon reads
        // as a flat disc rather than a shaded ball.
        let retro = 0.55 + 0.45 * pow(lambert, 0.42);

        // Earthshine: sunlight bounced off the planet onto the dark limb. It is
        // what makes "the old moon in the new moon's arms".
        let earthshine = sky.moon_params.y * (1.0 - sky.moon_color.a) * 0.5;

        let contrast = mix(1.0, albedo, saturate(sky.moon_params.z));
        let lit = lambert * retro * contrast + earthshine * contrast;
        result += sky.moon_tint.rgb * lit * sky.moon_params.x * disc;
    }

    // Only a lit moon glows.
    result += sky.moon_tint.rgb * halo * sky.moon_params.x * sky.moon_color.a;
    return result;
}

// ---------------------------------------------------------------------------
// Volumetric clouds
// ---------------------------------------------------------------------------

/// Distance to the far intersection with a sphere of radius `planet + shell`,
/// from a point at altitude `altitude` above a planet of radius `planet`.
///
/// The naive `dot(o, o) - r * r` loses all its precision at planetary scale, so
/// the constant term is factored as a difference of altitudes instead.
fn shell_exit(planet: f32, altitude: f32, shell: f32, rd_y: f32) -> f32 {
    let b = (planet + altitude) * rd_y;
    let c = (altitude - shell) * (2.0 * planet + altitude + shell);
    let discriminant = b * b - c;
    if discriminant < 0.0 {
        return -1.0;
    }
    return -b + sqrt(discriminant);
}

/// Distance to the near intersection with the planet, or -1 if the ray misses.
fn ground_hit(planet: f32, altitude: f32, rd_y: f32) -> f32 {
    let b = (planet + altitude) * rd_y;
    if b >= 0.0 {
        return -1.0;
    }
    let c = altitude * (2.0 * planet + altitude);
    let discriminant = b * b - c;
    if discriminant < 0.0 {
        return -1.0;
    }
    let t = -b - sqrt(discriminant);
    return select(-1.0, t, t > 0.0);
}

struct CloudSample {
    density: f32,
    height: f32,
}

fn cloud_density(position: vec3<f32>, planet: f32, detailed: bool) -> CloudSample {
    let base_altitude = sky.cloud_params0.z;
    let thickness = max(sky.cloud_params0.w, 1.0);
    let coverage = sky.cloud_params0.x;
    let density_scale = sky.cloud_params0.y;
    let shape_scale = max(sky.cloud_params1.x, 1.0);
    let detail_scale = max(sky.cloud_params1.y, 1.0);
    let detail_strength = sky.cloud_params1.z;

    var out: CloudSample;
    out.density = 0.0;
    out.height = 0.0;

    // Height within the slab, 0 at the base and 1 at the top.
    let altitude = length(position) - planet;
    let height = (altitude - base_altitude) / thickness;
    if height < 0.0 || height > 1.0 {
        return out;
    }
    out.height = height;

    if coverage <= 0.001 {
        return out;
    }

    // Sample the noise in slab-local space, at the *same* scale on all three
    // axes. Feeding it the planet-centred position instead would be a disaster
    // twice over: the vertical coordinate would be ~6.4e6, so `fract`-based
    // hashing loses all its precision, and dividing a few kilometres of visible
    // sky by a multi-kilometre feature size would put the whole overhead sky
    // inside a single noise cell.
    //
    // The isotropy matters just as much. Scaling the vertical axis by the slab
    // thickness instead makes every feature as wide as the cloud layer and only
    // as tall as it is deep, and the result reads as flat streaks of cirrus no
    // matter what the weather is supposed to be.
    //
    // The evolution term slides the sampling point through the field, so clouds
    // slowly change shape rather than just translating rigidly.
    let evolution = sky.cloud_offset.w;
    let sample_point = vec3(
        (position.x + sky.cloud_offset.x) / shape_scale + evolution,
        altitude / shape_scale + evolution * 0.7,
        (position.z + sky.cloud_offset.z) / shape_scale + evolution * 1.3,
    );

    // Two scales of base shape: broad systems, and the cells inside them.
    let broad = fbm3(sample_point, 3);
    let cells = billow3(sample_point * 3.7, 3);
    var shape = mix(broad, cells, 0.35);

    // Coverage is a threshold on the shape field, so raising it makes existing
    // clouds grow outward rather than making everything uniformly foggier.
    //
    // The upper edge tracks the threshold rather than sitting at 1.0. Fractal
    // noise clusters around its mean and almost never reaches its extremes, so
    // remapping to a fixed 1.0 would leave even a "solid" cloud at a fraction
    // of full density -- opaque cumulus would come out as haze.
    shape = remap(shape, 1.0 - coverage, 1.0 - coverage * 0.4, 0.0, 1.0);
    if shape <= 0.0 {
        return out;
    }

    // Vertical profile: rounded at the bottom, anvil-flattened at the top. A
    // thick slab keeps its shoulders (cumulonimbus); a thin one is all base
    // and top (stratus).
    let bottom = saturate(remap(height, 0.0, 0.12, 0.0, 1.0));
    let top = saturate(remap(height, 0.55, 1.0, 1.0, 0.0));
    // Higher coverage means flatter, more stratiform cloud.
    let profile = bottom * top * mix(1.0, 1.0 - height * 0.35, coverage);
    shape *= profile;
    if shape <= 0.0 {
        return out;
    }

    if detailed && detail_strength > 0.0 {
        let detail_point = vec3(
            (position.x + sky.cloud_offset.x * 1.4) / detail_scale,
            altitude / detail_scale,
            (position.z + sky.cloud_offset.z * 1.4) / detail_scale,
        );
        // Wispy at the edges, tighter lower down: erosion is strongest where
        // the cloud is already thin.
        let erosion = mix(billow3(detail_point, 3), 1.0 - fbm3(detail_point * 2.3, 2), height);
        shape = remap(shape, erosion * detail_strength, 1.0, 0.0, 1.0);
    }

    out.density = saturate(shape) * density_scale;
    return out;
}

/// Henyey-Greenstein phase function.
fn henyey_greenstein(cos_angle: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denominator = 1.0 + g2 - 2.0 * g * cos_angle;
    return (1.0 - g2) / (4.0 * PI * max(pow(denominator, 1.5), 1e-4));
}

/// Optical depth from `position` toward the sun, by short-stepping outward.
fn light_march(position: vec3<f32>, planet: f32, steps: i32) -> f32 {
    if steps <= 0 {
        return 0.0;
    }
    let thickness = max(sky.cloud_params0.w, 1.0);
    let sun = sky.sun_direction.xyz;
    // Cone-ish stepping: short steps near the sample capture local
    // self-shadowing, long ones catch the bulk of the cloud above.
    var optical_depth = 0.0;
    var travelled = 0.0;
    let base_step = thickness / f32(steps);
    for (var i = 0; i < steps; i += 1) {
        let step_size = base_step * (0.5 + f32(i));
        travelled += step_size;
        let sample = cloud_density(position + sun * travelled, planet, false);
        optical_depth += sample.density * step_size;
    }
    return optical_depth;
}

struct CloudResult {
    scattering: vec3<f32>,
    transmittance: f32,
    /// Opacity-weighted mean distance to the cloud, in metres. This is where
    /// the cloud "is" as far as aerial perspective is concerned.
    mean_distance: f32,
}

fn march_clouds(
    origin: vec3<f32>,
    ray: vec3<f32>,
    planet: f32,
    altitude: f32,
    dither: vec2<f32>,
) -> CloudResult {
    var result: CloudResult;
    result.scattering = vec3(0.0);
    result.transmittance = 1.0;
    result.mean_distance = 0.0;
    var distance_weight = 0.0;

    let base_altitude = sky.cloud_params0.z;
    let thickness = max(sky.cloud_params0.w, 1.0);
    let extinction = max(sky.cloud_params1.w, 1e-6);
    let forward_g = sky.cloud_params2.x;
    let back_g = sky.cloud_params2.y;
    let powder_strength = sky.cloud_params2.z;
    let ambient_strength = sky.cloud_params2.w;
    let exposure = sky.cloud_params3.x;
    let horizon_fade = sky.cloud_params3.y;
    let steps = i32(sky.cloud_params3.z);
    let light_steps = i32(sky.cloud_params3.w);

    if sky.cloud_params0.x <= 0.001 || steps <= 0 {
        return result;
    }

    // Looking at the ground: no sky clouds to march through.
    let ground = ground_hit(planet, altitude, ray.y);

    var near = shell_exit(planet, altitude, base_altitude, ray.y);
    var far = shell_exit(planet, altitude, base_altitude + thickness, ray.y);

    if altitude > base_altitude {
        // Inside or above the layer: the entry point is the top shell coming
        // down, so the two swap roles.
        near = 0.0;
        if altitude > base_altitude + thickness {
            return result;
        }
    }

    if far <= 0.0 || far <= near {
        return result;
    }
    if ground > 0.0 && ground < near {
        return result;
    }

    near = max(near, 0.0);
    far = min(far, MAX_MARCH_DISTANCE);
    if far <= near {
        return result;
    }

    let cos_sun = dot(ray, sky.sun_direction.xyz);
    // Two lobes: a tight forward one for the silver lining, a broad backward
    // one so cloud away from the sun does not go flat and dead.
    let phase = mix(
        henyey_greenstein(cos_sun, forward_g),
        henyey_greenstein(cos_sun, back_g),
        0.35,
    );

    // A cloud deck sits kilometres up, so it stays in sunlight well after the
    // sun has set for an observer on the ground. The depression of the horizon
    // seen from height h is about sqrt(2h/R) -- which is exactly why the
    // undersides of clouds are the last thing to lose the light at dusk.
    let cloud_top = base_altitude + thickness * 0.5;
    let cloud_horizon = -sqrt(2.0 * cloud_top / planet);
    let sun_up = smoothstep(cloud_horizon - 0.025, cloud_horizon + 0.025, sky.sun_direction.xyz.y);

    let sun_light = sky.sun_color.rgb * SUNLIT_RADIANCE * sun_up;
    // `moon_color.rgb` already carries the night-sky scale factor.
    let moon_light = sky.moon_color.rgb * sky.moon_color.a * 0.35;
    // Skylight bouncing around inside and under the deck. Without this the
    // shadowed side of a cloud reads as a hole in the sky. Albedo is applied
    // once, below, along with the direct term.
    let ambient = vec3(ambient_strength * (SKY_RADIANCE * sun_up + 4.0));

    let span = far - near;
    let step_size = span / f32(steps);
    // Dithering the start breaks the raymarch into noise instead of the
    // concentric banding a fixed start gives.
    let jitter = hash21(dither) * step_size;
    var travelled = near + jitter;

    for (var i = 0; i < steps; i += 1) {
        if result.transmittance < 0.01 {
            break;
        }
        let position = origin + ray * travelled;
        let sample = cloud_density(position, planet, true);
        travelled += step_size;

        if sample.density <= 0.001 {
            continue;
        }

        let sigma = sample.density * extinction;

        var sun_transmittance = 1.0;
        if light_steps > 0 {
            sun_transmittance = exp(-light_march(position, planet, light_steps) * extinction);
        } else {
            // Cheap stand-in: assume the cloud above is as dense as here.
            let above = (1.0 - sample.height) * thickness;
            sun_transmittance = exp(-sample.density * extinction * above * 0.5);
        }

        // Powder: multiple scattering makes deep cloud darker than a
        // single-scattering model predicts, which is what gives sunlit cumulus
        // their crisp, sculpted look.
        let powder = mix(1.0, 1.0 - exp(-sample.density * 8.0), powder_strength);

        // `phase * 4 * PI` is the phase function normalised to average one, so
        // a fully lit, unshadowed cloud comes out at `SUNLIT_RADIANCE` rather
        // than at some arbitrary multiple of it.
        let direct =
            (sun_light * sun_transmittance + moon_light) * (phase * 4.0 * PI) * powder;
        let source = (direct + ambient) * sky.cloud_albedo.rgb;

        // Lightning lights the cloud from within, brightest deep inside it.
        let inner_glow = sky.lightning.rgb * sky.lightning.a * sample.density;

        // Analytic integration of in-scattering over the step, rather than a
        // Riemann sum. For conservative scattering the exact integral of
        // `T(s) * sigma_s * L` across a step of optical depth `tau` collapses
        // to `L * (1 - exp(-tau))`: the density and the extinction coefficient
        // cancel out entirely. Leaving them in over-brightens the cloud by a
        // factor of `1 / extinction`, which is enough to make an overcast sky
        // come out whiter than a sunlit one.
        let step_transmittance = exp(-sigma * step_size);
        let integrated = (source + inner_glow) * (1.0 - step_transmittance);

        result.scattering += result.transmittance * integrated;
        // Weight the distance by how much of the final image this sample is
        // responsible for, so a thin veil in front of clear sky is treated as
        // near and a solid deck as the distance to its face.
        let contribution = result.transmittance * (1.0 - step_transmittance);
        result.mean_distance += travelled * contribution;
        distance_weight += contribution;
        result.transmittance *= step_transmittance;
    }

    // Fade the layer out as it approaches the horizon, where the slab
    // intersection stretches to hundreds of kilometres and the march can no
    // longer resolve it.
    let fade = smoothstep(0.0, max(horizon_fade, 1e-4), ray.y);
    result.scattering *= fade * exposure;
    result.transmittance = mix(1.0, result.transmittance, fade);
    result.mean_distance = select(near, result.mean_distance / distance_weight, distance_weight > 1e-5);
    return result;
}

// ---------------------------------------------------------------------------

/// Atmospheric optical depth along a ray, for the cloud pass' own aerial
/// perspective.
///
/// The air thins exponentially with height, so the column density from
/// altitude `h0` out to distance `d` along a ray whose vertical component is
/// `mu` has the closed form `H/mu * (exp(-h0/H) - exp(-(h0 + d*mu)/H))`.
fn air_column(h0: f32, mu: f32, distance: f32, scale_height: f32) -> f32 {
    if abs(mu) < 1e-3 {
        // Grazing the horizontal: the exponential is effectively constant over
        // the path, and the closed form above degenerates.
        return distance * exp(-h0 / scale_height);
    }
    let h1 = h0 + distance * mu;
    return scale_height / mu * (exp(-h0 / scale_height) - exp(-h1 / scale_height));
}

/// Transmittance of the air between the eye and a cloud `distance` away.
fn aerial_transmittance(h0: f32, mu: f32, distance: f32) -> vec3<f32> {
    // Rayleigh scattering coefficients at sea level, per metre, plus a grey
    // Mie term for haze.
    let rayleigh = vec3(5.802e-6, 13.558e-6, 33.100e-6);
    let mie = vec3(4.44e-6);
    let rayleigh_column = air_column(h0, mu, distance, 8000.0);
    let mie_column = air_column(h0, mu, distance, 1200.0);
    return exp(-(rayleigh * rayleigh_column + mie * mie_column));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let ray = ray_from_ndc(in.ndc);
    let flags = u32(sky.misc.w);
    let planet = max(sky.misc.y, 1.0);
    let units_per_meter = max(sky.misc.z, 1e-6);
    let eye = view.world_position / units_per_meter;
    let altitude = max(eye.y, 0.0);

#ifdef CLOUD_PASS
    var cloud_color = vec3(0.0);
    var cloud_alpha = 0.0;

    if (flags & FLAG_CLOUDS) != 0u {
        // Cloud maths runs in metres regardless of the scene's unit scale, and
        // is anchored to the planet centre so the layer curves away and meets
        // the horizon on its own.
        let origin = vec3(eye.x, planet + altitude, eye.z);
        let clouds = march_clouds(origin, ray, planet, altitude, in.position.xy);

        let opacity = 1.0 - clouds.transmittance;
        if opacity > 0.002 {
            // How much the cloud hides is decided by the cloud alone. Folding
            // the aerial-perspective transmittance into the alpha as well --
            // which looks like the same thing, and is tempting because it makes
            // the inscattering fall out of the blend for free -- leaves a solid
            // overcast deck at about 87% opacity. That is invisible against
            // blue sky and glaring against the sun, whose disc is some four
            // orders of magnitude brighter than anything else in the frame: it
            // burns straight through the storm.
            cloud_alpha = saturate(opacity);

            // So aerial perspective is applied to the colour instead, both
            // halves explicitly: the cloud's own light is extinguished by the
            // air in front of it, and the air in front of it adds its own
            // inscattered light in turn. That second term is what makes a
            // distant cloud fade into the haze rather than just going dark.
            let air = aerial_transmittance(altitude, ray.y, clouds.mean_distance);
            let sun_up = saturate(sky.sun_color.a);
            let inscattering = sky.sun_color.rgb * SKY_RADIANCE * sun_up;
            let radiance = clouds.scattering * air + inscattering * (vec3(1.0) - air);

            // Un-premultiply: the blend multiplies by alpha again on the way
            // out.
            cloud_color = radiance / max(opacity, 1e-4) * view.exposure;
        }
    }

    // Ground fog hides the sky too, and it has to be done here rather than by
    // Bevy's `DistanceFog`, which only runs inside the PBR shader and so never
    // touches the sky, the clouds or the stars. Without this the horizon stays
    // crisp and blue in the middle of a white-out.
    //
    // Ground fog is a shallow layer, so looking up you leave it quickly: the
    // path length through it is the layer height over the ray's vertical
    // component, which goes to infinity at the horizon and to the layer height
    // at the zenith.
    let fog_extinction = sky.fog_params.x;
    var fog_factor = 0.0;
    var fog_rgb = sky.fog_color.rgb;
    if fog_extinction > 0.0 {
        let path = sky.fog_params.y / max(ray.y, 0.004);
        fog_factor = 1.0 - exp(-fog_extinction * path);

        // Fog scatters forward, so it is markedly brighter in the direction of
        // the sun and dimmer away from it. A single flat colour in every
        // direction is the thing that makes fog read as a grey card taped over
        // the lens rather than as air you are standing in.
        let toward_sun = saturate(dot(ray, sky.sun_direction.xyz));
        let glow = sky.fog_params.z * pow(toward_sun, sky.fog_params.w) * saturate(sky.sun_color.a);
        fog_rgb = fog_rgb * (1.0 + glow) + sky.sun_color.rgb * glow * 0.35;
    }

    // Composite `fog over (cloud over sky)` into the single source colour and
    // alpha that one blend can express.
    let alpha = 1.0 - (1.0 - fog_factor) * (1.0 - cloud_alpha);
    if alpha <= 0.002 {
        discard;
    }
    let premultiplied = fog_rgb * fog_factor + cloud_color * cloud_alpha * (1.0 - fog_factor);

    return vec4(premultiplied / alpha, alpha);
#else
    var color = vec3(0.0);

    // Stars, galaxy and moon all sit outside the atmosphere.
    let star_dir = sky.star_from_world * ray;
    // Scintillation is strongest looking through the most air.
    let horizon_factor = 1.0 - saturate(ray.y);

    // Physically the atmosphere pass alone would hide the stars by day, since
    // daylight inscattering is thousands of times brighter than they are. But
    // a *point* star has no meaningful radiance at raster resolution -- the
    // value depends entirely on how much solid angle a pixel covers -- so the
    // night sky here is deliberately exaggerated to stay legible at a fixed
    // daylight exposure, and has to be faded back out explicitly.
    //
    // The band matches real twilight: first stars at about -6 degrees of sun
    // altitude, a full sky by about -15.
    let night = 1.0 - smoothstep(-0.26, -0.05, sky.sun_direction.y);

    if (flags & FLAG_GALAXY) != 0u {
        color += galaxy(star_dir) * night;
    }
    if (flags & FLAG_STARS) != 0u {
        color += star_field(star_dir, horizon_factor) * night;
    }
    if (flags & FLAG_MOON) != 0u {
        // A pale daytime moon against a blue sky is a real sight, so it keeps
        // a fraction of its brightness rather than vanishing at sunrise.
        color += moon(ray) * mix(0.12, 1.0, night);
    }

    // Nothing above the horizon should be visible below it.
    color *= smoothstep(-0.06, 0.02, ray.y);

    // Bevy's atmosphere applies exposure to its own inscattering only, so this
    // has to match to be composited in the same units.
    return vec4(color * view.exposure, 1.0);
#endif
}
