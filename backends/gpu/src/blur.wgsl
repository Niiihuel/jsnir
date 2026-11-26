// Dual Kawase blur, after Marius Bjørge, "Bandwidth-Efficient Rendering" (SIGGRAPH 2015). A down pass
// halves the image with a 5-tap filter (the center and four diagonal taps); an up pass doubles it with an
// 8-tap filter (four axial taps and four diagonal taps of double weight). Several passes approximate a wide
// Gaussian at a fraction of its cost. One triangle covers the destination; the scissor limits it to the
// pixels in use, and `factor` maps a destination pixel to the source: 2 down, 0.5 up.
// Colors are premultiplied: every step is linear, so they stay premultiplied.
struct Params {
    // 1 / size of the source texture.
    inv_size: vec2f,
    // The source's pixels in use, as texture coordinates: taps are clamped to them (edge extension).
    min_uv: vec2f,
    max_uv: vec2f,
    factor: f32,
    // Distance of the diagonal taps, in source pixels.
    offset: f32,
    // Destination origin in the target, which seeds the dither so it stays put on screen.
    origin: vec2f,
    // 1 and 0 except in the last up pass.
    saturation: f32,
    noise: f32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(1) @binding(0) var<uniform> params: Params;

@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

fn tap(uv: vec2f) -> vec4f {
    return textureSampleLevel(source, source_sampler, clamp(uv, params.min_uv, params.max_uv), 0.0);
}

// Integer hash of a pixel, uniform in [0, 1).
fn hash(p: vec2u) -> f32 {
    var h = p.x * 1664525u + p.y * 1013904223u;
    h = (h ^ (h >> 16u)) * 0x7feb352du;
    h = (h ^ (h >> 15u)) * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return f32(h >> 8u) / 16777216.0;
}

@fragment fn fragment(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let uv = position.xy * params.factor * params.inv_size;
    let d = params.offset * params.inv_size;
    let e = vec2f(d.x, -d.y);
    let diagonal = tap(uv + d) + tap(uv - d) + tap(uv + e) + tap(uv - e);
    var color: vec4f;
    if params.factor > 1.0 {
        color = (tap(uv) * 4.0 + diagonal) / 8.0;
    } else {
        let axial = tap(uv + vec2f(2.0 * d.x, 0.0)) + tap(uv - vec2f(2.0 * d.x, 0.0))
            + tap(uv + vec2f(0.0, 2.0 * d.y)) + tap(uv - vec2f(0.0, 2.0 * d.y));
        color = (axial + diagonal * 2.0) / 12.0;
    }
    // Saturation around Rec. 709 luma, then a small dither against banding in the smooth gradients.
    let luma = dot(color.rgb, vec3f(0.2126, 0.7152, 0.0722));
    var rgb = mix(vec3f(luma), color.rgb, params.saturation);
    let pixel = vec2u(max(position.xy + params.origin, vec2f(0.0)));
    rgb += (hash(pixel) - 0.5) * params.noise * color.a;
    return vec4f(clamp(rgb, vec3f(0.0), vec3f(color.a)), color.a);
}
