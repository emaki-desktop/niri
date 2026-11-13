use std::cell::RefCell;
use std::rc::Rc;

use glam::{Mat3, Vec2};
use niri_config::{CornerRadius, MaterialParams};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{
    ffi, GlesError, GlesFrame, GlesRenderer, GlesTexture, Uniform,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Frame as _, FrameContext, Offscreen, Texture as _};
use smithay::gpu_span_location;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};
use crate::render_helpers::backdrop::SharedProbe;
use crate::render_helpers::background_effect::{glass_uniforms, RenderParams};
use crate::render_helpers::blur::{Blur, BlurOptions};
use crate::render_helpers::renderer::AsGlesFrame as _;
use crate::render_helpers::shaders::{mat3_uniform, Shaders};
use crate::utils::region::TransformedRegion;

#[derive(Debug)]
pub struct FramebufferEffect {
    id: Id,
    commit: CommitCounter,
    /// Ids of the elements drawing shapes from one capture, one per shape: smithay tells
    /// instances of one id apart only by where and how they're drawn.
    sample_ids: RefCell<Vec<Id>>,
    /// The last capture for shapes, shared with the elements drawing them.
    shared: SharedCapture,
    /// Further captures, for shapes whose material blurs with options of its own: the id of
    /// each one's capture-only element, and what it captured.
    groups: RefCell<Vec<(Id, SharedCapture)>>,
    /// How bright the backdrop is behind the shapes that report it.
    probe: SharedProbe,
}

/// What a capture-only element captured, for the elements that draw from it.
type SharedCapture = Rc<RefCell<Option<Captured>>>;

#[derive(Debug)]
struct Captured {
    texture: GlesTexture,
    /// Where the capture was taken, in output coordinates before the frame's transform.
    dst: Rectangle<i32, Physical>,
}

/// Draws one shape from a capture of the area behind all of a surface's shapes.
///
/// It isn't a framebuffer effect itself: several of them share one capture, which smithay would
/// otherwise redo for every one, every frame.
#[derive(Debug)]
pub struct FramebufferSampleElement {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<f64, Logical>,
    clip_geo: Rectangle<f64, Logical>,
    corner_radius: CornerRadius,
    scale: f32,
    alpha: f32,
    noise: f32,
    saturation: f32,
    material: Option<MaterialParams>,
    tint: [f32; 4],
    shared: SharedCapture,
    /// Where to read the backdrop for, and the shape's place in the surface's list.
    report: Option<(SharedProbe, u32)>,
}

#[derive(Debug)]
pub struct FramebufferEffectElement {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<f64, Logical>,
    clip_geo: Rectangle<f64, Logical>,
    corner_radius: CornerRadius,
    subregion: Option<TransformedRegion>,
    scale: f32,
    alpha: f32,
    blur_options: Option<BlurOptions>,
    noise: f32,
    saturation: f32,
    /// Set for a capture-only element: what it captures goes here, and it draws nothing.
    share: Option<SharedCapture>,
}

#[derive(Debug)]
struct Inner {
    framebuffer: Option<GlesTexture>,
    blur: Option<Blur>,
    intermediate: Option<GlesTexture>,
    /// Reusable storage for subregion-filtered damage rects.
    subregion_damage: Vec<Rectangle<i32, Physical>>,
}

impl FramebufferEffect {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
            sample_ids: RefCell::new(Vec::new()),
            shared: Rc::new(RefCell::new(None)),
            groups: RefCell::new(Vec::new()),
            probe: SharedProbe::default(),
        }
    }

    /// The readings of the backdrop behind the shapes that report it.
    pub fn probe(&self) -> &SharedProbe {
        &self.probe
    }

    pub fn damage(&mut self) {
        self.commit.increment();
    }

    /// Lets go of the captures the shapes last drew from, for a surface that has no shapes
    /// now; each is a texture the size of the shapes' bounds, kept until the next capture.
    pub fn forget_captures(&self) {
        *self.shared.borrow_mut() = None;
        for (_, share) in self.groups.borrow().iter() {
            *share.borrow_mut() = None;
        }
    }

    pub fn render(
        &self,
        ns: Option<usize>,
        params: RenderParams,
        blur_options: Option<BlurOptions>,
        noise: f32,
        saturation: f32,
    ) -> FramebufferEffectElement {
        let (clip_geo, corner_radius) = params
            .clip
            .unwrap_or((params.geometry, CornerRadius::default()));

        let mut id = self.id.clone();
        if let Some(ns) = ns {
            id = id.namespaced(ns);
        }

        FramebufferEffectElement {
            id,
            commit: self.commit,
            geometry: params.geometry,
            clip_geo,
            corner_radius,
            subregion: params.subregion,
            scale: params.scale as f32,
            alpha: params.alpha,
            blur_options,
            noise,
            saturation,
            share: None,
        }
    }

    /// Elements drawing each of `shapes` from a capture of the area behind them, blurred with
    /// `blur_options` or the shape's material's own: one capture for each blur, of the area
    /// behind that blur's shapes.
    ///
    /// The capture-only elements come last: elements are drawn back to front, so every capture
    /// is taken before any shape draws, and none sees another's glass.
    pub fn render_shapes(
        &self,
        ns: Option<usize>,
        shapes: Vec<RenderParams>,
        blur_options: Option<BlurOptions>,
        noise: f32,
        saturation: f32,
        push: &mut dyn FnMut(FramebufferShapeElement),
    ) {
        let mut ids = self.sample_ids.borrow_mut();
        if ids.len() < shapes.len() {
            ids.resize_with(shapes.len(), Id::new);
        }

        let scale = shapes.first().map_or(1., |params| params.scale);

        // each blur's options and the area behind its shapes, in the order they first
        // appear; a lens bends the backdrop in from past the shape's edge, so the area
        // reaches that far around it
        let mut blurs: Vec<(Option<BlurOptions>, Rectangle<f64, Logical>)> = Vec::new();
        let mut group_of = Vec::with_capacity(shapes.len());
        for params in &shapes {
            let options = blur_options.map(|b| b.for_material(params.material.as_ref()));
            let reach = params
                .material
                .map_or(0., |m| f64::from(m.refraction.ceil()));
            let mut area = params.geometry;
            area.loc -= (reach, reach).into();
            area.size += (2. * reach, 2. * reach).into();
            match blurs.iter().position(|(o, _)| *o == options) {
                Some(at) => {
                    blurs[at].1 = blurs[at].1.merge(area);
                    group_of.push(at);
                }
                None => {
                    blurs.push((options, area));
                    group_of.push(blurs.len() - 1);
                }
            }
        }

        let mut groups = self.groups.borrow_mut();
        if groups.len() + 1 < blurs.len() {
            groups.resize_with(blurs.len() - 1, || (Id::new(), Rc::new(RefCell::new(None))));
        }
        let shared = |group: usize| match group {
            0 => self.shared.clone(),
            n => groups[n - 1].1.clone(),
        };

        for ((params, id), group) in shapes.into_iter().zip(ids.iter()).zip(&group_of) {
            let id = match ns {
                Some(ns) => id.namespaced(ns),
                None => id.clone(),
            };
            let (clip_geo, corner_radius) = params
                .clip
                .unwrap_or((params.geometry, CornerRadius::default()));

            push(FramebufferShapeElement::Sample(FramebufferSampleElement {
                id,
                commit: self.commit,
                geometry: params.geometry,
                clip_geo,
                corner_radius,
                scale: params.scale as f32,
                alpha: params.alpha,
                noise: params.material.and_then(|m| m.noise).unwrap_or(noise),
                saturation: params
                    .material
                    .and_then(|m| m.saturation)
                    .unwrap_or(saturation),
                material: params.material,
                tint: params.tint,
                shared: shared(*group),
                report: params.report.map(|index| (self.probe.clone(), index)),
            }));
        }

        for (group, (options, bounds)) in blurs.into_iter().enumerate() {
            let capture = self.render(
                ns,
                RenderParams {
                    geometry: bounds,
                    subregion: None,
                    clip: None,
                    scale,
                    alpha: 1.,
                    exact_clip: true,
                    material: None,
                    tint: [0.; 4],
                    fade: [0.; 4],
                    report: None,
                },
                options,
                noise,
                saturation,
            );
            let id = match (group, ns) {
                (0, _) => capture.id.clone(),
                (n, Some(ns)) => groups[n - 1].0.namespaced(ns),
                (n, None) => groups[n - 1].0.clone(),
            };
            push(FramebufferShapeElement::Capture(FramebufferEffectElement {
                id,
                share: Some(shared(group)),
                ..capture
            }));
        }
    }
}

crate::niri_render_elements! {
    FramebufferShapeElement => {
        Capture = FramebufferEffectElement,
        Sample = FramebufferSampleElement,
    }
}

/// Uniforms for the clipping shader, drawing `crop` of an element at `geometry`, clipped to
/// `clip_geo`, from `texture_region` of its texture (origin and size, normalized; all of it when
/// `None`).
#[allow(clippy::too_many_arguments)]
fn clip_uniforms(
    geometry: Rectangle<f64, Logical>,
    clip_geo: Rectangle<f64, Logical>,
    corner_radius: CornerRadius,
    scale: f32,
    noise: f32,
    saturation: f32,
    crop: Rectangle<f64, Logical>,
    transform: Transform,
    texture_region: Option<(Vec2, Vec2)>,
) -> [Uniform<'static>; 7] {
    let input_to_clip_geo = clip_matrix(geometry, clip_geo, crop, transform, texture_region);
    let clip_geo_size = (clip_geo.size.w as f32, clip_geo.size.h as f32);

    [
        Uniform::new("niri_scale", scale),
        Uniform::new("geo_size", clip_geo_size),
        Uniform::new("corner_radius", <[f32; 4]>::from(corner_radius)),
        mat3_uniform("input_to_geo", input_to_clip_geo),
        Uniform::new("noise", noise),
        Uniform::new("saturation", saturation),
        Uniform::new("bg_color", [0f32, 0., 0., 0.]),
    ]
}

/// Takes the shader's input to [0, 1] inside `clip_geo`; see `clip_uniforms`.
fn clip_matrix(
    geometry: Rectangle<f64, Logical>,
    clip_geo: Rectangle<f64, Logical>,
    crop: Rectangle<f64, Logical>,
    transform: Transform,
    texture_region: Option<(Vec2, Vec2)>,
) -> Mat3 {
    let offset = crop.loc - (clip_geo.loc - geometry.loc);
    let offset = Vec2::new(offset.x as f32, offset.y as f32);
    let crop_size = Vec2::new(crop.size.w as f32, crop.size.h as f32);
    let clip_size = Vec2::new(clip_geo.size.w as f32, clip_geo.size.h as f32);

    // Our v_coords are [0, 1] inside crop. We want them to be [0, 1] inside clip_geo.
    let input_to_clip_geo =
        Mat3::from_scale(crop_size / clip_size) * Mat3::from_translation(offset / crop_size);

    // Revert the effect of the texture transform.
    let transform_mat = Mat3::from_translation(Vec2::new(0.5, 0.5))
        * Mat3::from_cols_array(transform.matrix().as_ref())
        * Mat3::from_translation(Vec2::new(-0.5, -0.5));
    let mut input_to_clip_geo = input_to_clip_geo * transform_mat;

    // The shader's input is the texture coordinate; take it to [0, 1] inside the region first.
    if let Some((origin, size)) = texture_region {
        input_to_clip_geo = input_to_clip_geo
            * Mat3::from_scale(Vec2::ONE / size)
            * Mat3::from_translation(-origin);
    }

    input_to_clip_geo
}

impl FramebufferEffectElement {
    fn compute_uniforms(
        &self,
        crop: Rectangle<f64, Logical>,
        transform: Transform,
    ) -> [Uniform<'static>; 7] {
        clip_uniforms(
            self.geometry,
            self.clip_geo,
            self.corner_radius,
            self.scale,
            self.noise,
            self.saturation,
            crop,
            transform,
            None,
        )
    }
}

impl FramebufferEffectElement {
    fn share_capture(&self, inner: &Inner, dst: Rectangle<i32, Physical>) {
        if let Some(share) = &self.share {
            *share.borrow_mut() = inner
                .intermediate
                .clone()
                .map(|texture| Captured { texture, dst });
        }
    }
}

impl Element for FramebufferEffectElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        // We don't use src for drawing but we can use it to figure out how we were cropped.
        let size = self.geometry.size.to_buffer(1., Transform::Normal);
        Rectangle::from_size(size)
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("FramebufferEffectElement::capture_framebuffer");
        let location = gpu_span_location!("FramebufferEffectElement::capture_framebuffer");
        frame.with_gpu_span(location, |frame| {
            let output_rect = Rectangle::from_size(frame.output_size());
            let transform = frame.transformation();

            let mut guard = frame.renderer();

            let inner = cache
                .get_or_insert::<RefCell<Inner>, _>(|| RefCell::new(Inner::new(guard.as_mut())));
            let mut inner = inner.borrow_mut();
            let inner = &mut *inner;

            inner.intermediate = None;

            // The shapes' capture is the blur's own output texture: let it go now, before the
            // blur checks that its textures are its alone, or it would make the whole chain
            // again every frame. The shapes draw after this capture, so they see the new one.
            if let Some(share) = &self.share {
                *share.borrow_mut() = None;
            }

            // We want clamp-to-edge behavior for out-of-bounds pixels. However, glBlitFramebuffer
            // seems to skip out-of-bounds pixels, even though my reading of the docs suggests
            // otherwise (we use GL_LINEAR filter). So, clamp dst to the framebuffer bounds
            // ourselves.
            let clamped_dst = match dst.intersection(output_rect) {
                Some(clamped) => clamped,
                None => return Ok(()),
            };
            let clamp_scale = clamped_dst.size.to_f64() / dst.size.to_f64();

            let dst = transform.transform_rect_in(clamped_dst, &output_rect.size);

            // Compute size from our geometry and scale.
            //
            // The "correct" size is always dst.size since that's the pixel region we're actually
            // blitting. However, using dst.size causes two undesirable things when zooming out for
            // the overview:
            // 1. dst.size shrinks every frame, causing a texture realloaction for every fb effect
            //    element every frame.
            // 2. The underlying blur visually expands. This is technically correct, since the
            //    underlying contents shrink, but it's not what you visually expect: you expect the
            //    blur to also shrink as the windows zoom out, to give the zooming out effect.
            //
            // Using size computed from geometry and scale solves both of those problems (even
            // though there's a bit of a cost in that zoomed-out elements still blur the entire
            // unzoomed texture size, and even though the blur ends up slightly wrong as there's two
            // layers of texture resampling, up and back down).
            //
            // Here we use src.size rather than geometry directly because src takes into account
            // cropping.
            let size = src
                .size
                .to_logical(1., Transform::Normal)
                .upscale(clamp_scale)
                .to_physical_precise_round(self.scale);
            let size = transform.transform_size(size);

            let size = size.to_logical(1).to_buffer(1, Transform::Normal);

            // Recreate framebuffer if needed.
            if inner
                .framebuffer
                .as_ref()
                .is_some_and(|fb| fb.size() != size)
            {
                inner.framebuffer = None;
            }
            let framebuffer = if let Some(fb) = &inner.framebuffer {
                fb
            } else {
                trace!("creating framebuffer texture sized {} × {}", size.w, size.h);
                let renderer = guard.as_mut();
                let texture = renderer.create_buffer(Fourcc::Abgr8888, size)?;
                inner.framebuffer.insert(texture)
            };

            // Prepare blur textures.
            let mut blur = Option::zip(inner.blur.as_mut(), self.blur_options);
            if let Some((b, options)) = &mut blur {
                let renderer = guard.as_mut();
                if let Err(err) = b.prepare_textures(
                    |fourcc, size| renderer.create_buffer(fourcc, size),
                    framebuffer,
                    *options,
                ) {
                    warn!("error preparing blur textures: {err:?}");
                    blur = None;
                }
            }

            // We can't use renderer.with_context() as that will reset the GlesFrame binding that we
            // want to blit from.
            drop(guard);

            // Blit the framebuffer contents.
            frame.with_context(|gl| unsafe {
                while gl.GetError() != ffi::NO_ERROR {}

                let mut current_fbo = 0i32;
                gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo as *mut _);

                // BlitFramebuffer is affected by the scissor test, we don't want that.
                gl.Disable(ffi::SCISSOR_TEST);

                let mut fbo = 0;
                gl.GenFramebuffers(1, &mut fbo as *mut _);
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);

                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    framebuffer.tex_id(),
                    0,
                );

                gl.BlitFramebuffer(
                    dst.loc.x,
                    dst.loc.y,
                    dst.loc.x + dst.size.w,
                    dst.loc.y + dst.size.h,
                    0,
                    0,
                    size.w,
                    size.h,
                    ffi::COLOR_BUFFER_BIT,
                    ffi::LINEAR,
                );

                // Restore state set by GlesFrame that we just modified.
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
                gl.Enable(ffi::SCISSOR_TEST);

                gl.DeleteFramebuffers(1, &mut fbo as *mut _);

                if gl.GetError() != ffi::NO_ERROR {
                    Err(GlesError::BlitError)
                } else {
                    Ok(())
                }
            })??;

            // If blur is off, use the unblurred texture.
            if self.blur_options.is_none() {
                inner.intermediate = Some(framebuffer.clone());
                self.share_capture(inner, clamped_dst);
                return Ok(());
            }

            if let Some((blur, options)) = blur {
                let mut guard = frame.renderer();
                let renderer = guard.as_mut();
                match blur.render(renderer, framebuffer, options, None) {
                    Ok(blurred) => inner.intermediate = Some(blurred),
                    Err(err) => {
                        warn!("error rendering blur: {err:?}");
                    }
                }
            }

            self.share_capture(inner, clamped_dst);

            Ok(())
        })
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        // A capture-only element leaves the drawing to the shapes.
        if self.share.is_some() {
            return Ok(());
        }

        let Some(cache) = cache else {
            return Ok(());
        };
        let Some(inner) = cache.get::<RefCell<Inner>>() else {
            return Ok(());
        };
        let mut inner = inner.borrow_mut();
        let inner = &mut *inner;

        let Some(texture) = &inner.intermediate else {
            return Ok(());
        };

        // Clamp the same way as in capture_framebuffer().
        let output_rect = Rectangle::from_size(frame.output_size());
        let clamped_dst = match dst.intersection(output_rect) {
            Some(clamped) => clamped,
            None => return Ok(()),
        };
        let clamp_offset = clamped_dst.loc - dst.loc;

        // Filter damage by subregion, reusing the stored Vec to avoid allocation.
        let filtered = &mut inner.subregion_damage;
        filtered.clear();

        if let Some(subregion) = &self.subregion {
            // Convert to subregion coordinates.
            let mut crop = src.to_logical(1., Transform::Normal, &src.size);
            crop.loc += self.geometry.loc;
            subregion.filter_damage(crop, dst, damage, filtered);
        } else {
            filtered.extend(damage.iter());
        };

        // Adjust for clamped dst.
        if clamped_dst != dst {
            let r = Rectangle::new(clamp_offset, clamped_dst.size);
            filtered.retain_mut(|d| {
                if let Some(mut crop) = d.intersection(r) {
                    crop.loc -= clamp_offset;
                    *d = crop;
                    true
                } else {
                    false
                }
            });
        }

        if filtered.is_empty() {
            return Ok(());
        }
        let damage = &filtered[..];

        // Adjust src proportionally to the dst clamping.
        let src_loc = src.loc.to_logical(1., Transform::Normal, &src.size);
        let dst_to_src = src.size / dst.size.to_f64();
        let crop = Rectangle::new(
            src_loc + clamp_offset.to_f64().upscale(dst_to_src).to_logical(1.),
            clamped_dst.size.to_f64().upscale(dst_to_src).to_logical(1.),
        );

        let program = Shaders::get_from_frame(frame).postprocess_and_clip.clone();
        let uniforms = program
            .is_some()
            .then(|| self.compute_uniforms(crop, frame.transformation()));
        let uniforms = uniforms.as_ref().map_or(&[][..], |x| &x[..]);

        frame.render_texture_from_to(
            texture,
            Rectangle::from_size(texture.size().to_f64()),
            clamped_dst,
            damage,
            &[],
            // The intermediate texture has the same transform as the frame.
            frame.transformation().invert(),
            self.alpha,
            program.as_ref(),
            uniforms,
        )
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), TtyRendererError<'render>> {
        let gles_frame = frame.as_gles_frame();
        RenderElement::<GlesRenderer>::capture_framebuffer(&self, gles_frame, src, dst, cache)?;
        Ok(())
    }

    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let gles_frame = frame.as_gles_frame();
        RenderElement::<GlesRenderer>::draw(
            &self,
            gles_frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )?;
        Ok(())
    }
}

impl Inner {
    fn new(renderer: &mut GlesRenderer) -> Self {
        Inner {
            framebuffer: None,
            blur: Blur::new(renderer),
            intermediate: None,
            subregion_damage: Vec::new(),
        }
    }
}

impl Element for FramebufferSampleElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        // Like the framebuffer effect, src only tells how we were cropped.
        let size = self.geometry.size.to_buffer(1., Transform::Normal);
        Rectangle::from_size(size)
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }
}

impl RenderElement<GlesRenderer> for FramebufferSampleElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let shared = self.shared.borrow();
        let Some(captured) = shared.as_ref() else {
            return Ok(());
        };

        // Clamp the same way as the capture.
        let output_rect = Rectangle::from_size(frame.output_size());
        let Some(clamped_dst) = dst.intersection(output_rect) else {
            return Ok(());
        };
        let clamp_offset = clamped_dst.loc - dst.loc;

        let within = Rectangle::new(clamp_offset, clamped_dst.size);
        let damage: Vec<_> = damage
            .iter()
            .filter_map(|d| d.intersection(within))
            .map(|mut d| {
                d.loc -= clamp_offset;
                d
            })
            .collect();
        if damage.is_empty() {
            return Ok(());
        }

        // Where this shape is in the capture, which has the frame's transform.
        let transform = frame.transformation();
        let captured_rect = transform.transform_rect_in(captured.dst, &output_rect.size);
        let rect = transform.transform_rect_in(clamped_dst, &output_rect.size);
        let texture_size = captured.texture.size().to_f64();
        let sx = texture_size.w / f64::from(captured_rect.size.w.max(1));
        let sy = texture_size.h / f64::from(captured_rect.size.h.max(1));
        let texture_src = Rectangle::<f64, Buffer>::new(
            (
                f64::from(rect.loc.x - captured_rect.loc.x) * sx,
                f64::from(rect.loc.y - captured_rect.loc.y) * sy,
            )
                .into(),
            (f64::from(rect.size.w) * sx, f64::from(rect.size.h) * sy).into(),
        );

        // Adjust src proportionally to the dst clamping, for the clip.
        let src_loc = src.loc.to_logical(1., Transform::Normal, &src.size);
        let dst_to_src = src.size / dst.size.to_f64();
        let crop = Rectangle::new(
            src_loc + clamp_offset.to_f64().upscale(dst_to_src).to_logical(1.),
            clamped_dst.size.to_f64().upscale(dst_to_src).to_logical(1.),
        );

        let texture_region = Some((
            Vec2::new(
                (texture_src.loc.x / texture_size.w) as f32,
                (texture_src.loc.y / texture_size.h) as f32,
            ),
            Vec2::new(
                (texture_src.size.w / texture_size.w) as f32,
                (texture_src.size.h / texture_size.h) as f32,
            ),
        ));
        // read how bright the backdrop is here, now and then, without waiting for it
        if let Some((probe, index)) = &self.report {
            let mut probe = probe.borrow_mut();
            let src = Rectangle::<i32, Buffer>::new(
                (
                    texture_src.loc.x.round() as i32,
                    texture_src.loc.y.round() as i32,
                )
                    .into(),
                (
                    texture_src.size.w.round() as i32,
                    texture_src.size.h.round() as i32,
                )
                    .into(),
            );
            let texture = captured.texture.tex_id();
            frame.with_context(|gl| unsafe {
                probe.poll(gl);
                if probe.due(*index) {
                    probe.issue(gl, texture, src, *index);
                }
            })?;
        }

        let shaders = Shaders::get_from_frame(frame);
        let (program, glass) = match (&self.material, &shaders.glass) {
            (Some(material), Some(glass)) => (Some(glass.clone()), Some(material)),
            _ => (shaders.postprocess_and_clip.clone(), None),
        };
        let uniforms = program.is_some().then(|| {
            let mut uniforms = clip_uniforms(
                self.geometry,
                self.clip_geo,
                self.corner_radius,
                self.scale,
                self.noise,
                self.saturation,
                crop,
                transform,
                texture_region,
            )
            .to_vec();
            if let Some(material) = glass {
                let matrix = clip_matrix(
                    self.geometry,
                    self.clip_geo,
                    crop,
                    transform,
                    texture_region,
                );
                uniforms.extend(glass_uniforms(
                    material, self.tint, matrix, self.alpha, [0.; 4],
                ));
            }
            uniforms
        });
        let uniforms = uniforms.as_ref().map_or(&[][..], |x| &x[..]);

        frame.render_texture_from_to(
            &captured.texture,
            texture_src,
            clamped_dst,
            &damage,
            &[],
            transform.invert(),
            self.alpha,
            program.as_ref(),
            uniforms,
        )
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for FramebufferSampleElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let gles_frame = frame.as_gles_frame();
        RenderElement::<GlesRenderer>::draw(
            &self,
            gles_frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )?;
        Ok(())
    }
}
