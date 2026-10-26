//! Independent geometric oracle for one declared affine SMAA-family contour.
//! Integrate by clipping two complete pixel rectangles against a line and using
//! polygon area, without consulting the production area table or its integral.
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}
fn clip_rectangle(low_y: f64, high_y: f64, left: Point, right: Point, above: bool) -> f64 {
    let polygon = [
        Point { x: -0.5, y: low_y },
        Point { x: 0.5, y: low_y },
        Point { x: 0.5, y: high_y },
        Point { x: -0.5, y: high_y },
    ];
    let signed = |p: Point| {
        // Positive is geometrically below the contour (screen Y grows down).
        (right.x - left.x) * (p.y - left.y) - (right.y - left.y) * (p.x - left.x)
    };
    let inside = |value: f64| if above { value <= 0.0 } else { value >= 0.0 };
    let mut result = Vec::with_capacity(6);
    let mut previous = polygon[3];
    let mut previous_value = signed(previous);
    for current in polygon {
        let value = signed(current);
        let previous_inside = inside(previous_value);
        let current_inside = inside(value);
        if previous_inside != current_inside {
            let fraction = previous_value / (previous_value - value);
            result.push(Point {
                x: previous.x + fraction * (current.x - previous.x),
                y: previous.y + fraction * (current.y - previous.y),
            });
        }
        if current_inside {
            result.push(current);
        }
        previous = current;
        previous_value = value;
    }
    if result.len() < 3 {
        return 0.0;
    }
    let mut twice_area = 0.0;
    let mut previous = *result.last().unwrap();
    for current in result {
        twice_area += previous.x * current.y - current.x * previous.y;
        previous = current;
    }
    (twice_area.abs() * 0.5).clamp(0.0, 1.0)
}
/// Return positive/negative contour areas. e=-1/0/+1 places an endpoint at
/// -0.5/0/+0.5 relative to the boundary between two adjacent pixel rows.
/// Search/pattern rejection is a separate policy, outside this geometry oracle.
pub fn area(d0: u32, d1: u32, e0: i8, e1: i8) -> [f64; 2] {
    assert!((-1..=1).contains(&e0) && (-1..=1).contains(&e1));
    let left = Point {
        x: -f64::from(d0) - 0.5,
        y: f64::from(e0) * 0.5,
    };
    let right = Point {
        x: f64::from(d1) + 0.5,
        y: f64::from(e1) * 0.5,
    };
    [
        clip_rectangle(0.0, 1.0, left, right, true),
        clip_rectangle(-1.0, 0.0, left, right, false),
    ]
}
/// Exact bright-pixel coverage for y >= slope*x + offset on the unit square
/// centred at (x,y). Uses polygon clipping, including both slope orientations.
pub fn coverage(slope: f64, offset: f64, x: f64, y: f64) -> f64 {
    assert!([slope, offset, x, y].iter().all(|v| v.is_finite()));
    let centre = slope * x + offset - y;
    assert!(centre.is_finite());
    clip_rectangle(
        -0.5,
        0.5,
        Point {
            x: -0.5,
            y: centre - slope * 0.5,
        },
        Point {
            x: 0.5,
            y: centre + slope * 0.5,
        },
        false,
    )
}
#[cfg(test)]
mod tests {
    use super::{area, coverage};
    #[test]
    fn moving_contour_coverage_has_exact_limits_complement_and_integer_reprojection() {
        assert_eq!(coverage(0., 0., 0., 0.), 0.5);
        assert_eq!(coverage(0., 0.5, 0., 0.), 0.);
        assert_eq!(coverage(0., -0.5, 0., 0.), 1.);
        assert_eq!(coverage(1., 0., 0., 0.), 0.5);
        for slope in [-16., -8., -4., -2., -1., -0.5, 0., 0.5, 1., 2., 4., 8., 16.] {
            for step in -16..=16 {
                let offset = f64::from(step) / 32.;
                let value = coverage(slope, offset, 0., 0.);
                let complement = coverage(-slope, -offset, 0., 0.);
                assert!((value + complement - 1.).abs() <= 2e-14);
                for (dx, dy) in [(1., 0.), (0., -1.), (3., 2.)] {
                    let shifted = coverage(slope, offset + dy - slope * dx, dx, dy);
                    assert!((shifted - value).abs() <= 2e-14);
                }
            }
        }
    }
    #[test]
    fn contour_has_exact_grid_diagonal_complement_and_reflection_areas() {
        assert_eq!(area(0, 0, 0, 0), [0.0, 0.0]);
        assert_eq!(area(0, 0, -1, 1), [0.125, 0.125]);
        assert_eq!(area(0, 0, 1, -1), [0.125, 0.125]);
        assert_eq!(area(7, 16, 1, 1), [0.5, 0.0]);
        for d0 in [0, 1, 7, 16] {
            for d1 in [0, 1, 7, 16] {
                for e0 in -1..=1 {
                    for e1 in -1..=1 {
                        let actual = area(d0, d1, e0, e1);
                        let reflected = area(d1, d0, e1, e0);
                        let inverted = area(d0, d1, -e0, -e1);
                        for channel in 0..2 {
                            assert!(actual[channel] >= 0.0 && actual[channel] <= 0.5);
                            assert!((actual[channel] - reflected[channel]).abs() <= 1e-14);
                            assert!((actual[channel] - inverted[1 - channel]).abs() <= 1e-14);
                        }
                    }
                }
            }
        }
    }
}
