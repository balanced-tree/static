// Signal displacement (PLAN §5.2): VHS-style tracking tears, macro-blocking
// on strong clicks and a vertical roll on the strongest. Works on the screen
// position before jitter, so the whole torn picture still shakes as one.

// Largest horizontal band offset at tear = 1, as a fraction of frame width.
const TEAR_MAX: f32 = 0.06;
// Random tracking bands: candidates per frame pair, chance per unit of tear,
// and their strength relative to a click's band.
const LOOSE_BANDS: u32 = 3u;
const LOOSE_CHANCE: f32 = 0.5;
const LOOSE_STRENGTH: f32 = 0.4;
// Clicks at least this strong also break up into macroblocks.
const BLOCK_STRENGTH: f32 = 0.5;
// Macroblock edge in snow cells, and the chance a block breaks at full force.
const BLOCK_CELLS: f32 = 16.0;
const BLOCK_CHANCE: f32 = 0.6;
// Height of the blanking bar that rolls through the picture.
const ROLL_BAR: f32 = 0.05;

struct Displaced {
    uv: vec2<f32>,
    // Brightness multiplier (the blanking bar during a roll).
    dim: f32,
};

// Offset profile of one tear band: snaps hardest at the top edge, where the
// tracking slips, then eases out downwards with a ragged per-line edge.
fn band(y: f32, top: f32, height: f32, salt: u32) -> f32 {
    let k = (y - top) / height;
    if (k < 0.0 || k > 1.0) {
        return 0.0;
    }
    let ragged = hash3(u32(y * globals.snow_rows), salt, globals.frame).x - 0.5;
    return (1.0 - k) * (1.0 - k) + 0.25 * ragged;
}

fn displace(screen: vec2<f32>) -> Displaced {
    var out: Displaced;
    out.dim = 1.0;
    var uv = screen;

    // Vertical roll: the picture slides down and wraps, with a dark bar where
    // its bottom edge meets its top.
    if (globals.roll > 0.0) {
        let y = fract(uv.y - globals.roll * globals.tear);
        out.dim = 1.0 - smoothstep(1.0 - ROLL_BAR, 1.0 - ROLL_BAR * 0.6, y) * 0.95;
        uv.y = y;
    }

    let t = globals.tear;
    var dx = 0.0;
    var block_uv = vec2<f32>(-1.0);

    // Loose tracking bands, re-rolled every second frame.
    let hold = globals.frame / 2u;
    for (var i = 0u; i < LOOSE_BANDS; i++) {
        let h = hash3(hold, i, 31u);
        if (h.x < t * LOOSE_CHANCE) {
            let height = mix(0.01, 0.06, h.y);
            let dir = select(-1.0, 1.0, hash3(hold, i, 32u).x > 0.5);
            dx += dir * TEAR_MAX * LOOSE_STRENGTH * t * band(uv.y, h.z * (1.0 - height), height, 100u + i);
        }
    }

    // One band per click, placed by its seed so each click tears differently.
    for (var i = 0u; i < globals.event_count; i++) {
        let e = globals.events[i];
        let seed = u32(e.x);
        let h = hash3(seed, 0u, 21u);
        let height = mix(0.04, 0.18, h.y);
        let top = h.x * (1.0 - height);
        let dir = select(-1.0, 1.0, h.z > 0.5);
        dx += dir * TEAR_MAX * t * e.y * band(uv.y, top, height, seed);

        // Strong clicks smash the area around their band into macroblocks:
        // a 4×4 mosaic copied from a few blocks away.
        if (e.z >= BLOCK_STRENGTH && abs(uv.y - (top + height * 0.5)) < height) {
            let cols = round(globals.snow_rows * globals.resolution.x / globals.resolution.y);
            let size = vec2<f32>(BLOCK_CELLS / cols, BLOCK_CELLS / globals.snow_rows);
            let b = floor(uv / size);
            let bh = hash3(u32(b.x), u32(b.y), seed * 7919u + hold);
            if (bh.x < BLOCK_CHANCE * t * e.y) {
                let mosaic = (floor(fract(uv / size) * 4.0) + 0.5) / 4.0;
                let jump = round((bh.yz - 0.5) * vec2<f32>(6.0, 2.0));
                block_uv = (b + jump + mosaic) * size;
            }
        }
    }

    out.uv = select(uv + vec2<f32>(dx, 0.0), block_uv, block_uv.x >= 0.0 || block_uv.y >= 0.0);
    return out;
}
