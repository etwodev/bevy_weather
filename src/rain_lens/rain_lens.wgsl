// Rain on the camera lens.
//
// A full-screen pass that puts water on the glass rather than in the world.
// Everything is procedural: the drops are a function of the screen position and
// the clock, so nothing is simulated, nothing is stored, and the cost does not
// depend on how many drops are on screen.
//
// This runs before tonemapping, so the scene it refracts is still in HDR and a
// bright highlight seen through a droplet stays bright -- which is most of what
// sells the effect.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct RainLens {
    // x: wetness, 0..1. y: seconds, wrapped. z: refraction strength, in UV.
    // w: aspect ratio.
    params: vec4<f32>,
    // x: running-drop cells across the screen. y: fall speed.
    // z: clinging-bead cells across the screen. w: blur radius, in UV.
    params2: vec4<f32>,
    // x: specular rim strength. y: trail bead density. z, w: unused.
    params3: vec4<f32>,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var screen_sampler: sampler;
@group(0) @binding(2) var<uniform> lens: RainLens;

const PI: f32 = 3.14159265359;

fn hash3(p: vec2<f32>, salt: f32) -> vec3<f32> {
    var q = fract(vec3(p.x, p.y, p.x) * vec3(0.1031, 0.1030, 0.0973) + salt * 0.0137);
    q += dot(q, vec3(q.y, q.x, q.z) + 33.33);
    return fract((vec3(q.x, q.x, q.y) + vec3(q.y, q.x, q.x)) * vec3(q.z, q.y, q.x));
}

/// A droplet under consideration.
///
/// `offset` is the vector from the droplet's centre to this pixel, in UV, which
/// is what the refraction needs. `cover` is how much of the pixel the droplet
/// covers and `dome` is the height of its surface there -- one at the middle,
/// zero at the rim.
struct Droplet {
    offset: vec2<f32>,
    cover: f32,
    dome: f32,
}

/// Considers one droplet, keeping it if it covers this pixel more than whatever
/// was found before.
///
/// Keeping the strongest rather than accumulating them all is deliberate: two
/// droplets one behind the other do not refract twice, and adding their offsets
/// would tear the image where they overlap.
fn consider(
    best: ptr<function, Droplet>,
    point: vec2<f32>,
    centre: vec2<f32>,
    radius: f32,
    stretch: f32,
    cell_size: f32,
) {
    if radius <= 1e-4 {
        return;
    }
    let to = point - centre;
    // Water running down glass is drawn out behind itself.
    let shaped = vec2(to.x, to.y / stretch);
    let d2 = dot(shaped, shaped);
    let r2 = radius * radius;
    if d2 >= r2 {
        return;
    }
    let dome = sqrt(r2 - d2) / radius;
    // A droplet's edge is sharp -- it is a meniscus, not a smudge -- so only
    // the last fraction of the rim is softened, and only enough to stop it
    // aliasing.
    let cover = smoothstep(0.0, 0.22, dome);
    if cover <= (*best).cover {
        return;
    }
    *best = Droplet(to * cell_size, cover, dome);
}

/// Drops that run down the glass, and the beads they leave behind them.
fn running_drops(uv: vec2<f32>, cells: f32, salt: f32, wetness: f32, time: f32) -> Droplet {
    let aspect = lens.params.w;
    let speed = lens.params2.y;
    let trail_density = lens.params3.y;

    // Square cells on screen, so the drops come out round rather than oval.
    let point = vec2(uv.x * aspect, uv.y) * cells;
    let cell_size = 1.0 / cells;
    let id = floor(point);

    var best = Droplet(vec2(0.0), 0.0, 0.0);

    // Three by three, so a drop near a cell edge still reaches these pixels.
    for (var dy = -1; dy <= 1; dy += 1) {
        for (var dx = -1; dx <= 1; dx += 1) {
            let cell = id + vec2(f32(dx), f32(dy));
            let rnd = hash3(cell, salt);
            // Only some cells carry a drop, and more of them the wetter it is.
            if rnd.x > wetness {
                continue;
            }

            // Each drop runs at its own pace and sets off at its own moment.
            let pace = 0.45 + rnd.y * 0.95;
            let phase = fract(time * speed * pace + rnd.z * 11.7);
            // Drops cling, then let go: slow at the top, quick at the bottom.
            let fall = phase * phase;
            // Fade in and out across the run, so the wrap back to the top of
            // the cell is not a jump.
            let life = smoothstep(0.0, 0.10, phase) * (1.0 - smoothstep(0.72, 1.0, phase));
            // Placed anywhere in the cell, not near its middle: a drop sitting
            // at the centre of every cell is a grid, and the eye finds a grid
            // instantly however well the rest of it is done.
            let jitter_x = hash3(cell + 3.17, salt + 1.9).x;
            let head = vec2(cell.x + 0.08 + jitter_x * 0.84, cell.y + fall);
            // Sizes spread over a wide range, weighted small. Rain on glass is
            // mostly specks with the occasional fat drop through them; an even
            // spread of middling ovals is the giveaway that it is procedural.
            // Weighted small, but never so small it is a sub-pixel speck --
            // those only ever read as noise.
            let grade = rnd.z * rnd.z;
            let radius = (0.10 + grade * 0.22) * life;
            // Faster drops are drawn out a little behind themselves.
            consider(&best, point, head, radius, 1.0 + pace * 0.22, cell_size);

            if trail_density <= 0.0 {
                continue;
            }
            // The trail: the water the drop failed to take with it, left in a
            // broken line rather than a stripe, because that is what it looks
            // like on a windscreen.
            let above = head.y - point.y;
            if above <= 0.0 || above >= fall {
                continue;
            }
            let spacing = 0.045;
            let index = floor(above / spacing);
            let jitter = hash3(vec2(cell.x + salt, index), rnd.z);
            if jitter.x > trail_density {
                continue;
            }
            let bead = vec2(head.x + (jitter.y - 0.5) * 0.07, head.y - index * spacing);
            // Trail beads shrink with distance behind the head and dry out.
            let fade = 1.0 - above / max(fall, 1e-4);
            consider(
                &best,
                point,
                bead,
                radius * (0.18 + jitter.z * 0.22) * fade,
                1.0,
                cell_size,
            );
        }
    }

    return best;
}

/// Small beads that cling where they landed, growing and being knocked off.
fn clinging_beads(uv: vec2<f32>, cells: f32, salt: f32, wetness: f32, time: f32) -> Droplet {
    let aspect = lens.params.w;
    let point = vec2(uv.x * aspect, uv.y) * cells;
    let cell_size = 1.0 / cells;
    let id = floor(point);

    var best = Droplet(vec2(0.0), 0.0, 0.0);

    for (var dy = -1; dy <= 1; dy += 1) {
        for (var dx = -1; dx <= 1; dx += 1) {
            let cell = id + vec2(f32(dx), f32(dy));
            let rnd = hash3(cell, salt);
            if rnd.x > wetness * 0.85 {
                continue;
            }
            // Each bead has a life: it gathers, sits, and is eventually taken
            // by a passing drop. Staggered per cell, so the lens is always part
            // way through rather than pulsing as a whole.
            let cycle = fract(time * 0.055 * (0.6 + rnd.x) + rnd.z * 3.1);
            let life = smoothstep(0.0, 0.22, cycle) * (1.0 - smoothstep(0.72, 1.0, cycle));
            let centre = cell + vec2(0.1 + rnd.x * 0.8, 0.1 + rnd.y * 0.8);
            let grade = rnd.y * rnd.y;
            let radius = (0.13 + grade * 0.24) * life;
            consider(&best, point, centre, radius, 1.0, cell_size);
        }
    }

    return best;
}

fn stronger(a: Droplet, b: Droplet) -> Droplet {
    if b.cover > a.cover {
        return b;
    }
    return a;
}

/// A cheap five-tap blur, for the inside of a droplet.
fn blurred(uv: vec2<f32>, radius: f32) -> vec3<f32> {
    var total = textureSampleLevel(screen_texture, screen_sampler, uv, 0.0).rgb;
    if radius <= 0.0 {
        return total;
    }
    let step = vec2(radius, 0.0);
    total += textureSampleLevel(screen_texture, screen_sampler, uv + step.xy, 0.0).rgb;
    total += textureSampleLevel(screen_texture, screen_sampler, uv - step.xy, 0.0).rgb;
    total += textureSampleLevel(screen_texture, screen_sampler, uv + step.yx, 0.0).rgb;
    total += textureSampleLevel(screen_texture, screen_sampler, uv - step.yx, 0.0).rgb;
    return total * 0.2;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let wetness = lens.params.x;
    if wetness <= 0.002 {
        return textureSampleLevel(screen_texture, screen_sampler, uv, 0.0);
    }

    let time = lens.params.y;
    let aspect = lens.params.w;
    let cells = lens.params2.x;

    // Three layers: big drops running down, a finer and faster set behind them,
    // and the beads clinging between.
    var drop = running_drops(uv, cells, 0.0, wetness, time);
    drop = stronger(drop, running_drops(uv, cells * 1.9, 31.0, wetness * 0.8, time * 1.4));
    drop = stronger(drop, clinging_beads(uv, lens.params2.z, 71.0, wetness, time));

    if drop.cover <= 0.0 {
        return textureSampleLevel(screen_texture, screen_sampler, uv, 0.0);
    }

    // A droplet is a short-focus lens held against the glass, so it shows a
    // shrunken, inverted piece of whatever is behind it. Sampling *away* from
    // the surface normal by an amount proportional to the distance from its
    // centre is that inversion: the far side of the droplet shows what is on
    // the near side of the scene.
    let refraction = lens.params.z * (0.35 + 0.65 * wetness);
    let offset = -drop.offset * refraction * vec2(1.0 / aspect, 1.0);
    let refracted = clamp(uv + offset, vec2(0.0), vec2(1.0));

    // And it focuses somewhere other than the sensor, so what it shows is soft.
    let blur = lens.params2.w * drop.cover;
    var color = blurred(refracted, blur);

    // The rim. Two things happen there and both are needed, because a droplet
    // over an empty grey sky refracts nothing and would otherwise be invisible.
    //
    // Just inside the edge the surface turns away steeply enough to reflect
    // rather than transmit, which reads as a dark outline against anything
    // bright. Right at the edge the meniscus catches the light and throws it
    // back at you. Together they are what makes a droplet read as a solid piece
    // of water rather than as a smudge on the image.
    let edge = 1.0 - drop.dome;
    let brightness = dot(color, vec3(0.2126, 0.7152, 0.0722));
    let dark = smoothstep(0.55, 0.95, edge) * 0.55;
    let rim = pow(edge, 6.0);
    color = mix(color, color * (1.0 - dark), drop.cover);
    color += vec3(rim * lens.params3.x * brightness * drop.cover);

    let clear = textureSampleLevel(screen_texture, screen_sampler, uv, 0.0);
    return vec4(mix(clear.rgb, color, drop.cover), clear.a);
}
