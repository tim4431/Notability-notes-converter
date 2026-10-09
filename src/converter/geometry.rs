//! Preserve full stroke paths while assigning their visible parts to pages.
use super::model::{StrokeCurve, STROKE_STYLE_HIGHLIGHTER, STROKE_STYLE_PENCIL};
use super::render::{curve_has_variable_width, pen_width, pencil_width, smoothed_cubic_segments};

pub(super) fn stroke_pages(curve: StrokeCurve, page_height: f32) -> Vec<StrokeCurve> {
    if curve.points.is_empty()
        || !page_height.is_finite()
        || page_height <= 0.0
        || !curve.width.is_finite()
        || !curve
            .points
            .iter()
            .all(|(x, y)| x.is_finite() && y.is_finite())
    {
        return Vec::new();
    }
    let (mut min_y, mut max_y) = vertical_bounds(&curve);
    let radius = stroke_radius(&curve);
    if !radius.is_finite() {
        return Vec::new();
    }
    min_y -= radius;
    max_y += radius;
    if !min_y.is_finite() || !max_y.is_finite() {
        return Vec::new();
    }
    if max_y < 0.0 {
        return Vec::new();
    }
    let first = (min_y / page_height).floor().max(0.0) as usize;
    let last = (max_y / page_height).floor().max(0.0) as usize;
    (first..=last)
        .map(|page_index| {
            let mut local = curve.clone();
            local.page_index = page_index;
            for point in &mut local.points {
                point.1 -= page_index as f32 * page_height;
            }
            local
        })
        .collect()
}

fn stroke_radius(curve: &StrokeCurve) -> f32 {
    if curve.fill_path {
        return 0.0;
    }
    let pressure = curve
        .pressures
        .iter()
        .copied()
        .reduce(f32::max)
        .unwrap_or(1.0);
    let frac = curve
        .fractional_widths
        .iter()
        .copied()
        .reduce(f32::max)
        .unwrap_or(1.0);
    let width = if curve.style == STROKE_STYLE_PENCIL {
        pencil_width(curve.width, pressure, frac)
    } else if curve.style != STROKE_STYLE_HIGHLIGHTER
        && curve.dash_pattern.is_none()
        && curve_has_variable_width(curve)
    {
        pen_width(curve.width, pressure, frac)
    } else {
        curve.width.max(0.1)
    };
    if curve.points.len() == 1 && curve.style != STROKE_STYLE_PENCIL {
        (width * 0.5).max(0.5)
    } else {
        width * 0.5
    }
}

fn vertical_bounds(curve: &StrokeCurve) -> (f32, f32) {
    let mut bounds = (curve.points[0].1, curve.points[0].1);
    let mut add = |range: (f32, f32)| {
        bounds.0 = bounds.0.min(range.0);
        bounds.1 = bounds.1.max(range.1);
    };
    if !curve.path_commands.is_empty() {
        let mut index = 0;
        let mut current = curve.points[0];
        for command in &curve.path_commands {
            match *command {
                0 | 1 => {
                    current = curve.points[index];
                    index += 1;
                    add((current.1, current.1));
                }
                3 => {
                    let c1 = curve.points[index];
                    let c2 = curve.points[index + 1];
                    let end = curve.points[index + 2];
                    index += 3;
                    add(cubic_y_bounds([current.1, c1.1, c2.1, end.1]));
                    current = end;
                }
                _ => unreachable!("legacy outline commands are validated when parsed"),
            }
        }
    } else if !curve.preserve_vertices
        && curve.points.len() >= 4
        && (curve.points.len() - 1) % 3 == 0
    {
        for segment in (0..curve.points.len() - 1).step_by(3) {
            add(cubic_y_bounds([
                curve.points[segment].1,
                curve.points[segment + 1].1,
                curve.points[segment + 2].1,
                curve.points[segment + 3].1,
            ]));
        }
    } else if !curve.preserve_vertices
        && curve.points.len() > 2
        && curve.style != STROKE_STYLE_PENCIL
        && !(curve.style != STROKE_STYLE_HIGHLIGHTER
            && curve.dash_pattern.is_none()
            && curve_has_variable_width(curve))
    {
        let mut start = curve.points[0];
        for (c1, c2, end) in smoothed_cubic_segments(&curve.points) {
            add(cubic_y_bounds([start.1, c1.1, c2.1, end.1]));
            start = end;
        }
    } else {
        for point in &curve.points {
            add((point.1, point.1));
        }
    }
    bounds
}

fn cubic_y_bounds(y: [f32; 4]) -> (f32, f32) {
    let [y0, y1, y2, y3] = y.map(f64::from);
    let a = -y0 + 3.0 * y1 - 3.0 * y2 + y3;
    let b = 3.0 * y0 - 6.0 * y1 + 3.0 * y2;
    let c = 3.0 * (y1 - y0);
    let mut min = y0.min(y3);
    let mut max = y0.max(y3);
    let mut evaluate = |t: f64| {
        if t > 0.0 && t < 1.0 {
            let value = ((a * t + b) * t + c) * t + y0;
            min = min.min(value);
            max = max.max(value);
        }
    };
    if a.abs() < 1e-12 {
        if b.abs() >= 1e-12 {
            evaluate(-c / (2.0 * b));
        }
    } else {
        let discriminant = 4.0 * b * b - 12.0 * a * c;
        if discriminant >= 0.0 {
            let root = discriminant.sqrt();
            evaluate((-2.0 * b + root) / (6.0 * a));
            evaluate((-2.0 * b - root) / (6.0 * a));
        }
    }
    (min as f32, max as f32)
}

#[cfg(test)]
mod tests {
    use super::stroke_pages;
    use crate::converter::model::StrokeCurve;

    fn curve(points: Vec<(f32, f32)>, preserve_vertices: bool) -> StrokeCurve {
        StrokeCurve {
            page_index: 0,
            points,
            preserve_vertices,
            path_commands: Vec::new(),
            fill_path: false,
            width: 1.0,
            rgba: [0, 0, 0, 255],
            style: 3,
            dash_pattern: Some(1),
            pressures: vec![0.4, 1.6],
            fractional_widths: vec![1.0, 2.0],
            opacities: Vec::new(),
        }
    }

    #[test]
    fn crossing_line_keeps_the_full_path_and_width_data_on_both_pages() {
        let original = curve(vec![(10.0, 90.0), (10.0, 110.0)], true);
        let pages = stroke_pages(original.clone(), 100.0);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].points, original.points);
        assert_eq!(pages[1].points, vec![(10.0, -10.0), (10.0, 10.0)]);
        for page in pages {
            assert_eq!(page.pressures, original.pressures);
            assert_eq!(page.fractional_widths, original.fractional_widths);
            assert_eq!(page.dash_pattern, Some(1));
        }
    }

    #[test]
    fn crossing_cubic_and_curved_excursion_reach_the_second_page() {
        for points in [
            vec![(10.0, 90.0), (10.0, 96.0), (10.0, 104.0), (10.0, 110.0)],
            vec![(10.0, 90.0), (10.0, 120.0), (10.0, 120.0), (10.0, 90.0)],
        ] {
            let original = curve(points, false);
            let pages = stroke_pages(original.clone(), 100.0);
            assert_eq!(pages.len(), 2);
            assert_eq!(pages[0].points, original.points);
            for (local, global) in pages[1].points.iter().zip(&original.points) {
                assert_eq!(local.0, global.0);
                assert_eq!(local.1, global.1 - 100.0);
            }
        }
    }

    #[test]
    fn control_points_outside_the_page_do_not_create_a_spurious_page() {
        let pages = stroke_pages(
            curve(
                vec![(10.0, 80.0), (10.0, 101.0), (10.0, 101.0), (10.0, 80.0)],
                false,
            ),
            100.0,
        );
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn polygon_vertices_are_not_treated_as_cubic_controls() {
        let pages = stroke_pages(
            curve(
                vec![(0.0, 80.0), (100.0, 80.0), (50.0, 120.0), (0.0, 80.0)],
                true,
            ),
            100.0,
        );
        assert_eq!(pages.len(), 2);
        assert!(pages
            .iter()
            .all(|p| p.preserve_vertices && p.points.len() == 4));
    }

    #[test]
    fn erased_shape_outlines_cross_pages_without_changing_commands() {
        let mut original = curve(
            vec![(0.0, 90.0), (100.0, 90.0), (50.0, 110.0), (0.0, 90.0)],
            true,
        );
        original.fill_path = true;
        original.path_commands = vec![0, 1, 1, 1];
        let pages = stroke_pages(original.clone(), 100.0);
        assert_eq!(pages.len(), 2);
        assert!(pages
            .iter()
            .all(|p| p.fill_path && p.path_commands == original.path_commands));
    }
}
