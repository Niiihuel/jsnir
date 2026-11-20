// One instanced quad per drawing item. Positions inside the quad are in physical pixels.
// Modes: 0 fill (texture × vertical gradient, rounded), 1 inner shadow without blur, 2 blurred outer shadow.
// Every mode is multiplied by the mask's alpha (group 1; white when the quad has no mask) and by the
// rounded clip, which is the rectangle `round_clip` with corner radii `clip_radii` (all 0: no rounding).
// The rectangular clip is the scissor, so it is not repeated here.
struct Vertex {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) color: vec4f,
    @location(2) point: vec2f,
    @location(3) @interpolate(flat) shape: vec4f,
    @location(4) @interpolate(flat) params: vec4f,
    @location(5) @interpolate(flat) params2: vec4f,
    @location(6) @interpolate(flat) round_clip: vec4f,
    @location(7) @interpolate(flat) mask_rect: vec4f,
    @location(8) @interpolate(flat) clip_radii: vec4f,
    @location(9) @interpolate(flat) radii: vec4f,
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(1) @binding(0) var mask_image: texture_2d<f32>;
@group(1) @binding(1) var mask_sampler: sampler;

// Rectangles are in device pixels: `round_clip` in the target, `mask_rect` relative to the quad.
@vertex fn vertex(
    @builtin(vertex_index) index: u32,
    @location(0) edges: vec4f,
    @location(1) color: vec4f,
    @location(2) end_color: vec4f,
    @location(3) texcoords: vec4f,
    @location(4) shape: vec4f,
    @location(5) params: vec4f,
    @location(6) params2: vec4f,
    @location(7) round_clip: vec4f,
    @location(8) mask_rect: vec4f,
    @location(9) clip_radii: vec4f,
    @location(10) radii: vec4f,
) -> Vertex {
    let uv = vec2f(f32(index & 1u), f32(index >> 1u));
    return Vertex(
        vec4f(mix(edges.xy, edges.zw, uv), 0.0, 1.0),
        mix(texcoords.xy, texcoords.zw, uv), mix(color, end_color, uv.y), uv, shape, params, params2,
        round_clip, mask_rect, clip_radii, radii,
    );
}

// Signed distance from `p` to a rounded rectangle at `origin` with `size` and corner `radii` (top-left,
// top-right, bottom-right, bottom-left, as CSS). The corner is picked by the quadrant of `p`; a radius is
// clamped to half the shorter side, where CSS would scale all radii of the rectangle instead.
fn rounded(p: vec2f, origin: vec2f, size: vec2f, radii: vec4f) -> f32 {
    let half_size = size * 0.5;
    let local = p - origin - half_size;
    let corner = select(
        select(radii.x, radii.w, local.y > 0.0),
        select(radii.y, radii.z, local.y > 0.0),
        local.x > 0.0,
    );
    let r = clamp(corner, 0.0, min(half_size.x, half_size.y));
    let offset = abs(local) - (half_size - r);
    return length(max(offset, vec2f(0.0))) + min(max(offset.x, offset.y), 0.0) - r;
}

fn coverage(distance: f32) -> f32 {
    return clamp(0.5 - distance, 0.0, 1.0);
}

// Abramowitz–Stegun approximation of erf, accurate to 5e-4: enough for an 8-bit shadow.
fn erf(x: f32) -> f32 {
    let s = sign(x);
    let a = abs(x);
    let t = 1.0 / (1.0 + 0.47047 * a);
    let y = 1.0 - (0.3480242 * t - 0.0958798 * t * t + 0.7478556 * t * t * t) * exp(-a * a);
    return s * y;
}

fn sample_quad(input: Vertex) -> vec4f {
    let size = input.shape.xy;
    let radii = input.radii;
    let mode = input.shape.w;
    let p = input.point * size;
    if mode < 0.5 {
        let edge = select(1.0, coverage(rounded(p, vec2f(0.0), size, radii)), any(radii > vec4f(0.0)));
        // Level 0 explicitly: there are no mipmaps, and the sample sits in non-uniform control flow.
        return textureSampleLevel(image, image_sampler, input.uv, 0.0) * input.color * edge;
    }
    if mode < 1.5 {
        // Inside the shape, outside the shape moved by `offset` and shrunk by `spread`.
        let spread = input.params.x;
        let offset = input.params.yz;
        let outer = coverage(rounded(p, vec2f(0.0), size, radii));
        let inner = coverage(rounded(p - offset, vec2f(spread), size - 2.0 * spread, max(radii - spread, vec4f(0.0))));
        return input.color * outer * (1.0 - inner);
    }
    // Gaussian falloff of the element grown by `spread` and moved down by `y`, minus the element itself.
    let element = input.params;
    let sigma = max(input.params2.x, 0.001);
    let spread = input.params2.y;
    let y = input.params2.z;
    let element_radius = input.params2.w;
    let grown = rounded(
        p - vec2f(0.0, y), element.xy - vec2f(spread), element.zw + 2.0 * spread, vec4f(max(element_radius + spread, 0.0)),
    );
    let shadow = 0.5 - 0.5 * erf(grown / (sigma * 1.41421356));
    let under = coverage(rounded(p, element.xy, element.zw, vec4f(element_radius)));
    return input.color * shadow * (1.0 - under);
}

@fragment fn fragment(input: Vertex) -> @location(0) vec4f {
    // Sampled outside any branch, as a level-0 sample. Outside `mask_rect` the mask is transparent.
    let mask_uv = (input.point * input.shape.xy - input.mask_rect.xy) / input.mask_rect.zw;
    let inside = all(mask_uv >= vec2f(0.0)) && all(mask_uv <= vec2f(1.0));
    let mask_alpha = select(0.0, textureSampleLevel(mask_image, mask_sampler, mask_uv, 0.0).a, inside);
    let clip = select(
        1.0, coverage(rounded(input.position.xy, input.round_clip.xy, input.round_clip.zw, input.clip_radii)),
        any(input.clip_radii > vec4f(0.0)),
    );
    return sample_quad(input) * mask_alpha * clip;
}
