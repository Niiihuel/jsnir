// One instanced quad per drawing item. Positions inside the quad are in physical pixels.
// Modes: 0 fill (texture × vertical gradient, rounded), 1 inner shadow without blur, 2 blurred outer shadow.
struct Vertex {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) color: vec4f,
    @location(2) point: vec2f,
    @location(3) @interpolate(flat) shape: vec4f,
    @location(4) @interpolate(flat) params: vec4f,
    @location(5) @interpolate(flat) params2: vec4f,
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

@vertex fn vertex(
    @builtin(vertex_index) index: u32,
    @location(0) edges: vec4f,
    @location(1) color: vec4f,
    @location(2) end_color: vec4f,
    @location(3) texcoords: vec4f,
    @location(4) shape: vec4f,
    @location(5) params: vec4f,
    @location(6) params2: vec4f,
) -> Vertex {
    let uv = vec2f(f32(index & 1u), f32(index >> 1u));
    return Vertex(
        vec4f(mix(edges.xy, edges.zw, uv), 0.0, 1.0),
        mix(texcoords.xy, texcoords.zw, uv), mix(color, end_color, uv.y), uv, shape, params, params2,
    );
}

// Signed distance from `p` to a rounded rectangle at `origin` with `size` and corner `radius`.
fn rounded(p: vec2f, origin: vec2f, size: vec2f, radius: f32) -> f32 {
    let half_size = size * 0.5;
    let r = clamp(radius, 0.0, min(half_size.x, half_size.y));
    let offset = abs(p - origin - half_size) - (half_size - r);
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
    let radius = input.shape.z;
    let mode = input.shape.w;
    let p = input.point * size;
    if mode < 0.5 {
        let edge = select(1.0, coverage(rounded(p, vec2f(0.0), size, radius)), radius > 0.0);
        // Level 0 explicitly: there are no mipmaps, and the sample sits in non-uniform control flow.
        return textureSampleLevel(image, image_sampler, input.uv, 0.0) * input.color * edge;
    }
    if mode < 1.5 {
        // Inside the shape, outside the shape moved by `offset` and shrunk by `spread`.
        let spread = input.params.x;
        let offset = input.params.yz;
        let outer = coverage(rounded(p, vec2f(0.0), size, radius));
        let inner = coverage(rounded(p - offset, vec2f(spread), size - 2.0 * spread, max(radius - spread, 0.0)));
        return input.color * outer * (1.0 - inner);
    }
    // Gaussian falloff of the element grown by `spread` and moved down by `y`, minus the element itself.
    let element = input.params;
    let sigma = max(input.params2.x, 0.001);
    let spread = input.params2.y;
    let y = input.params2.z;
    let element_radius = input.params2.w;
    let grown = rounded(
        p - vec2f(0.0, y), element.xy - vec2f(spread), element.zw + 2.0 * spread, max(element_radius + spread, 0.0),
    );
    let shadow = 0.5 - 0.5 * erf(grown / (sigma * 1.41421356));
    let under = coverage(rounded(p, element.xy, element.zw, element_radius));
    return input.color * shadow * (1.0 - under);
}

@fragment fn fragment(input: Vertex) -> @location(0) vec4f {
    return sample_quad(input);
}
