//! HDR output: an output in HDR mode is composited into a half-float texture first, then encoded
//! for the panel in one pass.
//!
//! The composition keeps the encoding every element and effect shader already works in, gamma 2.2
//! with 1.0 as SDR white, only extended past 1.0 (and below 0.0 for colours outside BT.709), so
//! SDR content and the fork's glass look exactly as they do in SDR mode. [`HdrEncodeElement`]
//! then decodes it to linear light, applies the gamma ramp a gamma-control client set (night
//! light, which can't use the CRTC's LUT under PQ), scales it so 1.0 is
//! [`HdrParams::sdr_white`] nits, converts BT.709 to BT.2020 and encodes PQ for the 10-bit
//! scanout buffer.

use anyhow::Context as _;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::{
    Element, Id, Kind, RenderElement, RenderElementStates, UnderlyingStorage,
};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture, Uniform};
use smithay::backend::renderer::utils::{
    CommitCounter, DamageBag, DamageSet, DamageSnapshot, OpaqueRegions,
};
use smithay::backend::renderer::{
    Bind as _, Color32F, ContextId, Frame as _, Offscreen as _, Renderer as _, Texture as _,
};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Size, Transform};

use super::renderer::{AsGlesFrame as _, AsGlesRenderer as _};
use super::shaders::Shaders;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

std::thread_local! {
    /// Whether the output being rendered drew a surface brighter than SDR white (`hdr-auto`).
    static BRIGHT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Notes a surface whose content goes past SDR white (its description's peak over its reference
/// white), while an output's elements are gathered.
pub fn note_bright(content_peak: f32) {
    if content_peak > 1.01 {
        BRIGHT.set(true);
    }
}

/// Whether the elements gathered since the last call held a surface brighter than SDR white.
pub fn take_bright() -> bool {
    BRIGHT.replace(false)
}

/// How an output in HDR mode shows its SDR composition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrParams {
    /// Nits of SDR white (1.0 in the composition).
    pub sdr_white: f32,
    /// The gamma-control ramp, fitted per channel as `gain * x^exponent`.
    pub ramp_gain: [f32; 3],
    pub ramp_exponent: [f32; 3],
}

impl HdrParams {
    pub fn new(sdr_white: f32) -> Self {
        Self {
            sdr_white,
            ramp_gain: [1.; 3],
            ramp_exponent: [1.; 3],
        }
    }

    /// Fits a gamma-control ramp (red, green and blue tables one after another, as the protocol
    /// sends them); `None` resets it.
    pub fn set_ramp(&mut self, ramp: Option<&[u16]>) {
        let fitted = ramp.and_then(fit_ramp);
        let (gain, exponent) = fitted.unwrap_or(([1.; 3], [1.; 3]));
        self.ramp_gain = gain;
        self.ramp_exponent = exponent;
    }
}

/// `gain * x^exponent` per channel through a ramp's last and middle entries.
fn fit_ramp(ramp: &[u16]) -> Option<([f32; 3], [f32; 3])> {
    let size = ramp.len() / 3;
    if size < 3 || ramp.len() % 3 != 0 {
        return None;
    }
    let mut gain = [1.; 3];
    let mut exponent = [1.; 3];
    for channel in 0..3 {
        let table = &ramp[channel * size..(channel + 1) * size];
        let last = f32::from(table[size - 1]) / 65535.;
        let mid_x = (size / 2) as f32 / (size - 1) as f32;
        let mid = f32::from(table[size / 2]) / 65535.;
        gain[channel] = last;
        exponent[channel] = if last > 0. && mid > 0. {
            ((mid / last).ln() / mid_x.ln()).clamp(0.1, 10.)
        } else {
            1.
        };
    }
    Some((gain, exponent))
}

/// An output's half-float composition, kept between frames so only damage is redrawn.
#[derive(Debug)]
pub struct HdrBuffer {
    id: Id,
    params: HdrParams,
    inner: Option<Inner>,
}

#[derive(Debug)]
struct Inner {
    texture: GlesTexture,
    renderer_context_id: ContextId<GlesTexture>,
    scale: Scale<f64>,
    damage: OutputDamageTracker,
    /// Damage of the encode element, facing the DRM compositor.
    outer_damage: DamageBag<i32, Buffer>,
}

impl HdrBuffer {
    pub fn new(params: HdrParams) -> Self {
        Self {
            id: Id::new(),
            params,
            inner: None,
        }
    }

    pub fn params(&self) -> HdrParams {
        self.params
    }

    /// Changes how it's encoded, which redraws the whole output.
    pub fn set_params(&mut self, params: HdrParams) {
        if params == self.params {
            return;
        }
        self.params = params;
        if let Some(inner) = &mut self.inner {
            inner
                .outer_damage
                .add([Rectangle::from_size(inner.texture.size())]);
        }
    }

    /// Lets go of the texture; the next render makes one again.
    pub fn clear(&mut self) {
        self.inner = None;
    }

    /// Renders `elements` (in the output's untransformed physical space, `size` big) into the
    /// composition, and gives the element that encodes it and the states of the elements.
    pub fn render<'render>(
        &mut self,
        renderer: &mut TtyRenderer<'render>,
        size: Size<i32, Physical>,
        scale: Scale<f64>,
        elements: &[impl RenderElement<TtyRenderer<'render>>],
    ) -> anyhow::Result<(HdrEncodeElement, RenderElementStates)> {
        let _span = tracy_client::span!("HdrBuffer::render");

        let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
        let context_id = renderer.as_gles_renderer().context_id();

        let stale = self.inner.as_ref().is_some_and(|inner| {
            inner.texture.size() != buffer_size
                || inner.renderer_context_id != context_id
                || inner.scale != scale
        });
        if stale {
            self.inner = None;
        }

        let inner = match &mut self.inner {
            Some(inner) => inner,
            None => {
                let _span = tracy_client::span!("creating HDR buffer");
                let texture: GlesTexture = renderer
                    .create_buffer(Fourcc::Abgr16161616f, buffer_size)
                    .context("error creating the half-float texture")?;
                self.inner.insert(Inner {
                    texture,
                    renderer_context_id: context_id,
                    scale,
                    damage: OutputDamageTracker::new(size, scale, Transform::Normal),
                    outer_damage: DamageBag::default(),
                })
            }
        };

        let res = {
            let mut target = renderer.bind(&mut inner.texture)?;
            inner
                .damage
                .render_output(renderer, &mut target, 1, elements, Color32F::BLACK)
                .context("error rendering into the HDR buffer")?
        };

        if let Some(damage) = res.damage {
            // the tracker says Physical, but it rendered into a texture: Buffer space
            let logical = size.to_logical(1);
            inner.outer_damage.add(
                damage
                    .iter()
                    .map(|rect| rect.to_logical(1).to_buffer(1, Transform::Normal, &logical)),
            );
        }

        let element = HdrEncodeElement {
            id: self.id.clone(),
            texture: inner.texture.clone(),
            renderer_context_id: inner.renderer_context_id.clone(),
            damage: inner.outer_damage.snapshot(),
            size,
            params: self.params,
        };
        Ok((element, res.states))
    }
}

/// Draws the composition onto the scanout buffer, encoded for the panel.
#[derive(Debug, Clone)]
pub struct HdrEncodeElement {
    id: Id,
    texture: GlesTexture,
    renderer_context_id: ContextId<GlesTexture>,
    damage: DamageSnapshot<i32, Buffer>,
    size: Size<i32, Physical>,
    params: HdrParams,
}

impl Element for HdrEncodeElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.damage.current_commit()
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::from_size(self.size)
    }

    fn transform(&self) -> Transform {
        Transform::Normal
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.texture.size()).to_f64()
    }

    fn damage_since(
        &self,
        _scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        let damage = self
            .damage
            .damage_since(commit)
            .unwrap_or_else(|| DamageSet::from_slice(&[Rectangle::from_size(self.texture.size())]));
        // the texture is the output's size, so buffer and physical pixels are the same
        damage
            .into_iter()
            .map(|rect| {
                Rectangle::new(
                    (rect.loc.x, rect.loc.y).into(),
                    (rect.size.w, rect.size.h).into(),
                )
            })
            .collect()
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::from_slice(&[Rectangle::from_size(self.size)])
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for HdrEncodeElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dest: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        if frame.context_id() != self.renderer_context_id {
            warn!("trying to render the HDR buffer from a different renderer");
            return Ok(());
        }

        let Some(program) = Shaders::get_from_frame(frame).hdr_encode.clone() else {
            // checked before an output turns HDR on; drawn plain, it's at least visible
            return frame.render_texture_from_to(
                &self.texture,
                src,
                dest,
                damage,
                opaque_regions,
                Transform::Normal,
                1.,
                None,
                &[],
            );
        };

        let params = self.params;
        let uniforms = vec![
            Uniform::new("sdr_white", params.sdr_white),
            Uniform::new("ramp_gain", params.ramp_gain),
            Uniform::new("ramp_exponent", params.ramp_exponent),
        ];
        frame.override_default_tex_program(program, uniforms);
        let res = frame.render_texture_from_to(
            &self.texture,
            src,
            dest,
            damage,
            opaque_regions,
            Transform::Normal,
            1.,
            None,
            &[],
        );
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for HdrEncodeElement {
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

    fn underlying_storage(
        &self,
        _renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(size: usize, f: impl Fn(usize, f32) -> f32) -> Vec<u16> {
        (0..3)
            .flat_map(|channel| {
                let f = &f;
                (0..size).map(move |i| {
                    let x = i as f32 / (size - 1) as f32;
                    (f(channel, x).clamp(0., 1.) * 65535.).round() as u16
                })
            })
            .collect()
    }

    #[test]
    fn an_identity_ramp_fits_to_nothing() {
        let (gain, exponent) = fit_ramp(&ramp(256, |_, x| x)).unwrap();
        for c in 0..3 {
            assert!((gain[c] - 1.).abs() < 1e-3, "{gain:?}");
            assert!((exponent[c] - 1.).abs() < 0.02, "{exponent:?}");
        }
    }

    #[test]
    fn a_warm_ramp_keeps_its_tint() {
        // wlsunset at about 4000 K: red whole, blue cut
        let white = [1.0, 0.86, 0.69];
        let (gain, exponent) = fit_ramp(&ramp(1024, |c, x| x * white[c])).unwrap();
        for c in 0..3 {
            assert!((gain[c] - white[c]).abs() < 1e-3, "{gain:?}");
            assert!((exponent[c] - 1.).abs() < 0.02, "{exponent:?}");
        }
        assert!(fit_ramp(&[0; 7]).is_none());
    }
}
