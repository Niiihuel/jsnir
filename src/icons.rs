//! Stroke icons from Hugeicons free (MIT), drawn on the CPU like text so every backend shows the same pixels.
//! Paths use the 24 × 24 viewBox, round caps and round joins.
use crate::{text::TextBitmap, tree::Icon};

/// One stroked path: a start point and cubic or line segments.
enum Segment {
    Line([f32; 2]),
    Cubic([f32; 2], [f32; 2], [f32; 2]),
}

fn paths(icon: Icon) -> Vec<([f32; 2], Vec<Segment>)> {
    use Segment::{Cubic, Line};
    match icon {
        Icon::ArrowLeft => vec![(
            [15., 6.],
            vec![
                Cubic([15., 6.], [9.00001, 10.4189], [9., 12.]),
                Cubic([8.99999, 13.5812], [15., 18.], [15., 18.]),
            ],
        )],
        Icon::ArrowRight => vec![(
            [9.00005, 6.],
            vec![
                Cubic([9.00005, 6.], [15., 10.4189], [15., 12.]),
                Cubic([15., 13.5812], [9., 18.], [9., 18.]),
            ],
        )],
        Icon::Tick => vec![([5., 14.], vec![Line([8.5, 17.5]), Line([19., 6.5])])],
        Icon::CheckCircle => {
            // The circle as four quarter arcs, each a cubic with the usual 0.5523 handle.
            let k = 10. * 0.552_284_8;
            vec![
                (
                    [22., 12.],
                    vec![
                        Cubic([22., 12. - k], [12. + k, 2.], [12., 2.]),
                        Cubic([12. - k, 2.], [2., 12. - k], [2., 12.]),
                        Cubic([2., 12. + k], [12. - k, 22.], [12., 22.]),
                        Cubic([12. + k, 22.], [22., 12. + k], [22., 12.]),
                    ],
                ),
                ([8., 12.5], vec![Line([10.5, 15.]), Line([16., 9.])]),
            ]
        }
    }
}

/// Flattens each path into a polyline in viewBox units.
fn polylines(icon: Icon) -> Vec<Vec<[f32; 2]>> {
    paths(icon)
        .into_iter()
        .map(|(start, segments)| {
            let mut points = vec![start];
            for segment in segments {
                let p0 = *points.last().unwrap();
                match segment {
                    Segment::Line(p) => points.push(p),
                    Segment::Cubic(c1, c2, p) => points.extend((1..=16).map(|i| {
                        let t = i as f32 / 16.;
                        let u = 1. - t;
                        let w = [u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t];
                        [0, 1].map(|k| w[0] * p0[k] + w[1] * c1[k] + w[2] * c2[k] + w[3] * p[k])
                    })),
                }
            }
            points
        })
        .collect()
}

/// Keeps the first `fraction` of each polyline's length, as an SVG `stroke-dashoffset` reveal.
fn trim(line: &[[f32; 2]], fraction: f32) -> Vec<[f32; 2]> {
    let length = |a: [f32; 2], b: [f32; 2]| ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let total: f32 = line.windows(2).map(|w| length(w[0], w[1])).sum();
    let mut left = total * fraction.clamp(0., 1.);
    let mut out = vec![line[0]];
    for w in line.windows(2) {
        let step = length(w[0], w[1]);
        if left >= step {
            out.push(w[1]);
            left -= step;
        } else {
            if left > 0. {
                let t = left / step;
                out.push([
                    w[0][0] + (w[1][0] - w[0][0]) * t,
                    w[0][1] + (w[1][1] - w[0][1]) * t,
                ]);
            }
            break;
        }
    }
    out
}

/// Rasterizes an icon `size` logical pixels wide at `scale`, with a stroke of `stroke` viewBox units,
/// `draw` of each path shown and `color` as straight RGBA. Pixels are premultiplied.
pub fn rasterize(
    icon: Icon,
    size: f32,
    stroke: f32,
    draw: f32,
    color: [u8; 4],
    scale: f32,
) -> Option<TextBitmap> {
    let side = (size * scale).ceil() as usize;
    if side == 0 || draw.is_nan() || draw <= 0. {
        return None;
    }
    let units = side as f32 / 24.;
    let radius = stroke * units / 2.;
    let lines: Vec<Vec<[f32; 2]>> = polylines(icon)
        .iter()
        .map(|line| {
            trim(line, draw)
                .into_iter()
                .map(|[x, y]| [x * units, y * units])
                .collect()
        })
        .collect();
    let mut rgba = vec![0; side * side * 4];
    for y in 0..side {
        for x in 0..side {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let distance = lines
                .iter()
                .flat_map(|line| {
                    let single = (line.len() == 1).then(|| [line[0], line[0]]);
                    line.windows(2).map(|w| [w[0], w[1]]).chain(single)
                })
                .map(|[a, b]| segment_distance(p, a, b))
                .fold(f32::MAX, f32::min);
            let coverage = (radius - distance + 0.5).clamp(0., 1.) * f32::from(color[3]) / 255.;
            if coverage > 0. {
                let pixel = &mut rgba[(y * side + x) * 4..][..4];
                for (channel, value) in pixel.iter_mut().zip([color[0], color[1], color[2], 255]) {
                    *channel = (f32::from(value) * coverage).round() as u8;
                }
            }
        }
    }
    Some(TextBitmap {
        size: [side, side],
        rgba,
    })
}

fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let length = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if length > 0. {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / length).clamp(0., 1.)
    } else {
        0.
    };
    let closest = [a[0] + ab[0] * t - p[0], a[1] + ab[1] * t - p[1]];
    (closest[0] * closest[0] + closest[1] * closest[1]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_tick_covers_fewer_pixels_and_none_draws_nothing() {
        let ink = |draw| {
            rasterize(Icon::Tick, 16., 2.4, draw, [255; 4], 1.)
                .map_or(0, |b| b.rgba.chunks(4).filter(|p| p[3] > 0).count())
        };
        assert_eq!(ink(0.), 0);
        assert!(0 < ink(0.3) && ink(0.3) < ink(1.));
        // The circle icon is closed: its first and last points meet.
        let circle = &polylines(Icon::CheckCircle)[0];
        let (a, b) = (circle[0], *circle.last().unwrap());
        assert!((a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3);
    }
}
