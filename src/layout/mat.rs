//! A backing painted behind the scrolling layout's windows.
//!
//! The mat fills the gaps between and around the columns, so they read as one surface in normal
//! mode, and fades out as the overview opens. It only reads the geometry the scrolling layout
//! already computes each frame. See `docs/fork/layout/workspace-mat.md`.

use std::cell::RefCell;

use niri_config::{Color, CornerRadius, GradientInterpolation, MatColumnFill, MatEnds};
use smithay::backend::renderer::element::Kind;
use smithay::utils::{Logical, Point, Rectangle, Size};

use super::Options;
use crate::niri_render_elements;
use crate::render_helpers::background_effect::{material_named, RenderParams};
use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::xray::{XrayElement, XrayPos};
use crate::render_helpers::RenderCtx;
use crate::utils::round_logical_in_physical;

/// A column's visible extent, in view coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatColumn {
    pub x: f64,
    pub width: f64,
    /// Bottom of the column's last visible tile.
    pub bottom: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatGeometry {
    /// The bounding rectangle of the mat, soft ends included.
    pub slab: Rectangle<f64, Logical>,
    /// A partition of the slab's x range between the soft ends, sharing boundaries; one cell
    /// when the cells would all be the full height.
    pub cells: Vec<Rectangle<f64, Logical>>,
    /// The fades along the inside of the slab's left and right edges.
    pub soft_ends: Option<[Rectangle<f64, Logical>; 2]>,
}

/// Where the mat goes for `columns`, between `top` and `bottom` in view coordinates.
///
/// `working_bottom` is the bottom of the working area: a column whose tiles reach it covers down
/// to `bottom`, struts included.
pub fn mat_geometry(
    columns: &[MatColumn],
    config: &niri_config::Mat,
    gap: f64,
    (top, bottom): (f64, f64),
    working_bottom: f64,
    scale: f64,
) -> Option<MatGeometry> {
    if config.off || columns.is_empty() {
        return None;
    }

    let round = |x: f64| round_logical_in_physical(scale, x);

    let mut columns = columns.to_vec();
    columns.sort_by(|a, b| a.x.total_cmp(&b.x));

    // Hard ends stop one gap past the outer columns. Soft ends fade out over the outer columns'
    // last `soft-end-length`, so the mat is transparent by their outer edges.
    let soft = config.ends == MatEnds::Soft;
    let outer = if soft { 0. } else { gap };

    // Every boundary is rounded once and used by both cells beside it, so the cells tile the
    // slab with no seams at fractional scales and no holes while columns move.
    let mut bounds = Vec::with_capacity(columns.len() + 1);
    bounds.push(round(columns[0].x - outer));
    for pair in columns.windows(2) {
        let mid = (pair[0].x + pair[0].width + pair[1].x) / 2.;
        let prev = *bounds.last().unwrap();
        bounds.push(f64::max(prev, round(mid)));
    }
    let right = columns
        .iter()
        .map(|col| col.x + col.width)
        .fold(f64::NEG_INFINITY, f64::max);
    let prev = *bounds.last().unwrap();
    bounds.push(f64::max(prev, round(right + outer)));

    let full_h = bottom - top;
    let height = |col: &MatColumn| match config.column_fill {
        MatColumnFill::Full => full_h,
        MatColumnFill::Tiles => {
            let col_bottom = col.bottom + gap;
            // Allow for rounding of tile sizes.
            if col_bottom >= working_bottom - 0.5 {
                full_h
            } else {
                (round(col_bottom) - top).clamp(0., full_h)
            }
        }
    };

    let slab_x = bounds[0];
    let slab_w = bounds[bounds.len() - 1] - slab_x;
    let slab = Rectangle::new(Point::from((slab_x, top)), Size::from((slab_w, full_h)));

    // The fades take at most half the slab each.
    let length = if soft {
        let half = (slab_w / 2. * scale).floor() / scale;
        round(config.soft_end_length).min(half)
    } else {
        0.
    };
    let inner_left = slab_x + length;
    let inner_right = slab_x + slab_w - length;

    let heights: Vec<f64> = columns.iter().map(height).collect();
    let cells = if heights.iter().all(|h| *h == full_h) {
        let size = Size::from((inner_right - inner_left, full_h));
        vec![Rectangle::new(Point::from((inner_left, top)), size)]
    } else {
        // The cells leave room for the fades, which keep the end columns' heights.
        let bounds: Vec<f64> = bounds
            .iter()
            .map(|x| x.clamp(inner_left, inner_right))
            .collect();
        heights
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let x = bounds[i];
                Rectangle::new(Point::from((x, top)), Size::from((bounds[i + 1] - x, *h)))
            })
            .collect()
    };

    let soft_ends = (length > 0.).then(|| {
        let left_h = heights[0];
        let right_h = heights[heights.len() - 1];
        [
            Rectangle::new(Point::from((slab_x, top)), Size::from((length, left_h))),
            Rectangle::new(
                Point::from((inner_right, top)),
                Size::from((length, right_h)),
            ),
        ]
    });

    Some(MatGeometry {
        slab,
        cells,
        soft_ends,
    })
}

#[derive(Debug)]
pub struct Mat {
    geometry: Option<MatGeometry>,
    /// One per cell, kept across frames so damage tracking only sees real changes.
    buffers: Vec<SolidColorBuffer>,
    /// The soft ends' gradients, kept across frames for the same reason: a new element is a
    /// new id, which the damage tracker sees as damaged however unchanged it is. Updated at
    /// render, where the alpha is known.
    ends: RefCell<[BorderRenderElement; 2]>,
    color: Color,
    material: Option<niri_config::MaterialParams>,
    /// The material name last warned about, to warn once.
    warned_material: Option<String>,
    scale: f64,
}

niri_render_elements! {
    MatRenderElement => {
        SolidColor = SolidColorRenderElement,
        Border = BorderRenderElement,
        Xray = XrayElement,
    }
}

impl Mat {
    pub fn new() -> Self {
        Self {
            geometry: None,
            buffers: Vec::new(),
            ends: RefCell::new([BorderRenderElement::empty(), BorderRenderElement::empty()]),
            color: Color::default(),
            material: None,
            warned_material: None,
            scale: 1.,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        columns: &[MatColumn],
        config: &niri_config::Mat,
        gap: f64,
        view_size: Size<f64, Logical>,
        parent_area: Rectangle<f64, Logical>,
        working_bottom: f64,
        scale: f64,
    ) {
        self.color = config.color;
        self.scale = scale;

        // looked up each time, so a reloaded material applies
        self.material = config.material.as_deref().and_then(|name| {
            let material = material_named(name);
            if material.is_none() && self.warned_material.as_deref() != Some(name) {
                warn!("mat: unknown material {name:?}, drawing a solid mat");
                self.warned_material = Some(name.to_owned());
            }
            material
        });

        // The full view height, or the view less the bars' exclusive zones (`parent_area`
        // still includes niri's own struts).
        let span = if config.behind_bars {
            (0., view_size.h)
        } else {
            let top = round_logical_in_physical(scale, parent_area.loc.y);
            let bottom = round_logical_in_physical(scale, parent_area.loc.y + parent_area.size.h);
            (top, bottom)
        };
        self.geometry = mat_geometry(columns, config, gap, span, working_bottom, scale);

        let Some(geo) = &self.geometry else {
            self.buffers.clear();
            return;
        };

        self.buffers
            .resize_with(geo.cells.len(), SolidColorBuffer::default);
        for (buffer, cell) in self.buffers.iter_mut().zip(&geo.cells) {
            buffer.update(cell.size, self.color);
        }
    }

    pub fn render<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        xray_pos: XrayPos,
        alpha: f32,
        options: &Options,
        push: &mut dyn FnMut(MatRenderElement),
    ) {
        let Some(geo) = &self.geometry else {
            return;
        };
        if alpha <= 0. {
            return;
        }

        if let (Some(material), Some(xray)) = (self.material, ctx.xray) {
            // The soft ends fade across the slab's outer edges, in the slab's coordinates.
            let fade = geo.soft_ends.map_or([0.; 4], |[left, right]| {
                let w = geo.slab.size.w as f32;
                let (l, r) = (left.size.w as f32, right.size.w as f32);
                [0., l, w - r, w]
            });
            let ends = geo.soft_ends.iter().flatten();
            let areas = geo
                .cells
                .iter()
                .chain(ends)
                .filter(|area| !area.is_empty())
                .map(|cell| {
                    let params = RenderParams {
                        geometry: *cell,
                        subregion: None,
                        // The rims sit on the slab's outer edges, not between cells.
                        clip: Some((geo.slab, CornerRadius::default())),
                        scale: self.scale,
                        alpha,
                        exact_clip: false,
                        material: Some(material),
                        tint: self.color.to_array_premul(),
                        fade,
                        report: None,
                    };
                    (params, xray_pos.offset(cell.loc))
                })
                .collect();

            let blur = &options.blur;
            let blur_options = (!blur.off).then(|| BlurOptions::from(*blur));
            let (noise, saturation) = if blur.off {
                (0., 1.)
            } else {
                (blur.noise as f32, blur.saturation as f32)
            };
            xray.render(
                ctx.as_gles(),
                areas,
                blur_options,
                noise,
                saturation,
                &mut |elem| push(elem.into()),
            );
            return;
        }

        if let Some(ends) = &geo.soft_ends {
            let transparent = Color {
                a: 0.,
                ..self.color
            };
            // The gradient runs along x, from the transparent outer edge to the slab.
            let mut elems = self.ends.borrow_mut();
            for (elem, (end, from, to)) in elems.iter_mut().zip([
                (ends[0], transparent, self.color),
                (ends[1], self.color, transparent),
            ]) {
                let area = Rectangle::from_size(end.size);
                elem.update(
                    end.size,
                    area,
                    GradientInterpolation::default(),
                    from,
                    to,
                    0.,
                    area,
                    0.,
                    CornerRadius::default(),
                    self.scale as f32,
                    alpha,
                );
                push(elem.clone().with_location(end.loc).into());
            }
        }

        for (buffer, cell) in self.buffers.iter().zip(&geo.cells) {
            let elem =
                SolidColorRenderElement::from_buffer(buffer, cell.loc, alpha, Kind::Unspecified);
            push(elem.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(column_fill: MatColumnFill, ends: MatEnds) -> niri_config::Mat {
        niri_config::Mat {
            off: false,
            column_fill,
            ends,
            ..Default::default()
        }
    }

    fn col(x: f64, width: f64, bottom: f64) -> MatColumn {
        MatColumn { x, width, bottom }
    }

    const VIEW: (f64, f64) = (0., 1080.);

    fn geo(columns: &[MatColumn], config: &niri_config::Mat, scale: f64) -> MatGeometry {
        // gaps 8, no bottom strut
        mat_geometry(columns, config, 8., VIEW, 1080., scale).unwrap()
    }

    /// Checks that the soft ends and cells partition the slab's x range exactly.
    #[track_caller]
    fn assert_partition(geo: &MatGeometry, scale: f64) {
        let slab = geo.slab;
        let mut x = slab.loc.x;
        let mut cells = geo.cells.clone();
        if let Some([left, right]) = geo.soft_ends {
            cells.insert(0, left);
            cells.push(right);
        }
        for cell in &cells {
            assert_eq!(cell.loc.x, x, "cell starts where the last ended");
            assert!(cell.size.w >= 0.);
            assert_eq!(cell.loc.y, slab.loc.y);
            x = cell.loc.x + cell.size.w;
            // boundaries sit on physical pixels
            let phys = x * scale;
            assert!((phys - phys.round()).abs() < 1e-6, "{x} at scale {scale}");
        }
        assert!((x - (slab.loc.x + slab.size.w)).abs() < 1e-9);
    }

    #[test]
    fn off_or_empty() {
        let full = config(MatColumnFill::Full, MatEnds::Hard);
        assert_eq!(mat_geometry(&[], &full, 8., VIEW, 1080., 1.), None);

        let off = niri_config::Mat::default();
        let columns = [col(8., 100., 1072.)];
        assert_eq!(mat_geometry(&columns, &off, 8., VIEW, 1080., 1.), None);
    }

    #[test]
    fn one_column() {
        let full = config(MatColumnFill::Full, MatEnds::Hard);
        let geo = geo(&[col(8., 100., 1072.)], &full, 1.);
        assert_eq!(
            geo.slab,
            Rectangle::new((0., 0.).into(), (116., 1080.).into())
        );
        assert_eq!(geo.cells, vec![geo.slab]);
        assert_eq!(geo.soft_ends, None);
    }

    #[test]
    fn partition_at_fractional_scales() {
        let tiles = config(MatColumnFill::Tiles, MatEnds::Hard);
        let columns = [
            col(8.3, 633.37, 500.),
            col(649.67, 633.37, 700.),
            col(1291.04, 412.1, 300.),
        ];
        for scale in [1., 1.25, 1.5] {
            let geo = geo(&columns, &tiles, scale);
            assert_eq!(geo.cells.len(), 3);
            assert_partition(&geo, scale);
        }
    }

    #[test]
    fn moving_columns_leave_no_holes() {
        let tiles = config(MatColumnFill::Tiles, MatEnds::Hard);
        // overlapping mid-swap, given out of order
        let overlap = [col(300., 400., 500.), col(8., 400., 700.)];
        // pulled apart
        let apart = [col(8., 400., 500.), col(600., 400., 700.)];
        for columns in [&overlap[..], &apart[..]] {
            for scale in [1., 1.25, 1.5] {
                assert_partition(&geo(columns, &tiles, scale), scale);
            }
        }
        // a narrow column inside a wide one's span
        let nested = [col(8., 1000., 500.), col(200., 100., 700.)];
        assert_partition(&geo(&nested, &tiles, 1.), 1.);
    }

    #[test]
    fn heights() {
        let full = config(MatColumnFill::Full, MatEnds::Hard);
        let tiles = config(MatColumnFill::Tiles, MatEnds::Hard);
        let columns = [col(8., 400., 500.), col(416., 400., 1072.)];

        // full: one slab cell, full view height regardless of the working area
        let geo_full = mat_geometry(&columns, &full, 8., VIEW, 1000., 1.).unwrap();
        assert_eq!(geo_full.cells, vec![geo_full.slab]);
        assert_eq!(geo_full.slab.size.h, 1080.);

        // tiles: a skyline; the full-height column reaches the view bottom past a bottom strut
        let geo_tiles = mat_geometry(&columns, &tiles, 8., VIEW, 1000., 1.).unwrap();
        let heights: Vec<f64> = geo_tiles.cells.iter().map(|c| c.size.h).collect();
        assert_eq!(heights, [508., 1080.]);
    }

    #[test]
    fn soft_ends() {
        let soft = config(MatColumnFill::Tiles, MatEnds::Soft);
        let columns = [col(8., 400., 500.), col(416., 400., 700.)];
        let soft_geo = geo(&columns, &soft, 1.);
        let [left, right] = soft_geo.soft_ends.unwrap();
        // transparent by the outer columns' edges: no outer gap
        assert_eq!(soft_geo.slab.loc.x, 8.);
        assert_eq!(soft_geo.slab.loc.x + soft_geo.slab.size.w, 816.);
        assert_eq!(left.loc.x, 8.);
        assert_eq!(right.loc.x + right.size.w, 816.);
        assert_eq!((left.size.w, right.size.w), (96., 96.));
        assert_eq!((left.size.h, right.size.h), (508., 708.));
        for scale in [1., 1.25, 1.5] {
            assert_partition(&geo(&columns, &soft, scale), scale);
        }

        // with full fill, one cell between the fades
        let full = config(MatColumnFill::Full, MatEnds::Soft);
        let geo_full = geo(&columns, &full, 1.);
        assert_eq!(geo_full.cells.len(), 1);
        assert_partition(&geo_full, 1.);

        // a span short of the view: a bottom bar's exclusive zone
        let tiles = config(MatColumnFill::Tiles, MatEnds::Hard);
        let columns_bar = [col(8., 400., 500.), col(416., 400., 1032.)];
        let geo_bar = mat_geometry(&columns_bar, &tiles, 8., (0., 1040.), 1040., 1.).unwrap();
        assert_eq!((geo_bar.slab.loc.y, geo_bar.slab.size.h), (0., 1040.));
        let heights: Vec<f64> = geo_bar.cells.iter().map(|c| c.size.h).collect();
        assert_eq!(heights, [508., 1040.]);

        // a narrow strip: the fades meet in the middle
        let narrow = geo(&[col(8., 100., 1072.)], &full, 1.);
        let [left, right] = narrow.soft_ends.unwrap();
        assert_eq!((left.size.w, right.size.w), (50., 50.));
        assert_eq!(narrow.cells[0].size.w, 0.);
    }
}
