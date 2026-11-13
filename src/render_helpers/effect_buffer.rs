use std::mem;

use anyhow::{ensure, Context as _};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::{Id, RenderElementStates};
use smithay::backend::renderer::gles::{GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{CommitCounter, DamageBag, DamageSet};
use smithay::backend::renderer::{
    Bind as _, Color32F, ContextId, FrameContext as _, Offscreen as _, Renderer as _, Texture,
};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};

use crate::niri::OutputRenderElements;
use crate::render_helpers::blur::{Blur, BlurOptions};

/// How many commits of damage [`EffectBuffer`] keeps; an element asking about an older one
/// gets full damage.
const DAMAGE_HISTORY: usize = 16;

#[derive(Debug)]
pub struct EffectBuffer {
    /// Id to be used for this effect buffer's elements.
    id: Id,

    /// Size of the effect buffer.
    size: Size<i32, Buffer>,
    /// Scale of the effect buffer.
    scale: Scale<f64>,
    /// Options for blurring.
    blur_options: BlurOptions,

    /// Elements to be rendered on demand.
    elements: Elements,
    /// Offscreen buffer where elements get rendered.
    offscreen: Option<Offscreen>,
    /// Blurring program, if available.
    blur: Option<Blur>,
    /// Blurs with other options than `blur_options`, for materials that frost more or less
    /// than `blur {}`, most recently asked for last.
    variants: Vec<Variant>,

    /// Where the buffer changed, by commit, in buffer pixels (fork): the contents re-rendered
    /// there, or all of it for anything else (blur options, release, scale). The commit counter
    /// takes into account both original and blurred texture changes.
    damage: DamageBag<i32, Buffer>,

    /// Frames [`tick`](Self::tick)ed so far, and at which the buffer was last prepared and its
    /// own blur last asked for; a tick lets go of what wasn't for a while.
    frame: u64,
    used_at: u64,
    blur_used_at: u64,
}

/// The contents blurred with options of a material's own.
#[derive(Debug)]
struct Variant {
    options: BlurOptions,
    blur: Blur,
    /// Reset to `None` along with the main blurred texture.
    blurred: Option<GlesTexture>,
    /// Where the contents changed since `blurred` was made (fork); see [`Stale`].
    stale: Stale,
    /// The frame this variant was last asked for.
    used_at: u64,
}

/// How many blur variants a buffer keeps; the least recently asked for goes first.
const MAX_VARIANTS: usize = 4;

/// How many frames a buffer, its blur or a variant stays unused before a tick lets it go.
/// Frames, not time: a still screen renders none, and nothing is lost by keeping what the
/// next frame will want. A full output's offscreen is 20 MB at 2880×1800 and each blur chain
/// a third more, so a material no longer on screen or a background that went empty shouldn't
/// keep theirs for the session.
const IDLE_FRAMES: u64 = 600;

#[derive(Debug)]
enum Elements {
    /// Contents remain unchanged.
    Unchanged(
        // Storage to avoid reallocating it every time.
        Vec<OutputRenderElements<GlesRenderer>>,
    ),
    /// New contents, need to check damage and render.
    New(Vec<OutputRenderElements<GlesRenderer>>),
}

#[derive(Debug)]
struct Offscreen {
    /// The texture with the offscreen contents.
    texture: GlesTexture,
    /// Id of the renderer context that the texture comes from.
    renderer_context_id: ContextId<GlesTexture>,
    /// Scale of the texture.
    scale: Scale<f64>,
    /// Damage tracker for drawing to the texture.
    damage: OutputDamageTracker,
    /// Render element states from the last render into the offscreen.
    states: RenderElementStates,
    /// Rendered blurred version of the texture.
    ///
    /// When texture needs to be reblurred, this field must be reset to `None`.
    blurred: Option<GlesTexture>,
    /// Where the contents changed since `blurred` was made (fork); see [`Stale`].
    stale: Stale,
}

/// Where a buffer's contents changed since one of its blurs was made (fork). A blur is only
/// made again for an area that samples near a change: a desktop widget repainting leaves the
/// bar's frost, a strip at the top of the same full-screen buffer, as it was, instead of
/// blurring the whole buffer again for every tick of a clock. `NIRI_REBLUR_ALL=1` blurs again
/// on every change, as before.
#[derive(Debug, Default)]
struct Stale {
    rects: Vec<Rectangle<i32, Buffer>>,
}

/// How many changed rectangles a [`Stale`] keeps before it keeps their bounding box instead.
const MAX_STALE: usize = 16;

impl Stale {
    fn add(&mut self, damage: &[Rectangle<i32, Physical>]) {
        self.rects.extend(damage.iter().map(|rect| {
            Rectangle::new(
                (rect.loc.x, rect.loc.y).into(),
                (rect.size.w, rect.size.h).into(),
            )
        }));
        if self.rects.len() > MAX_STALE {
            let all = self.rects.iter().copied().reduce(|a, b| a.merge(b));
            self.rects = all.into_iter().collect();
        }
    }

    /// Whether a blur still shows `area` as it is: nothing changed within the blur's reach of it.
    fn still_shows(&self, area: Option<Rectangle<f64, Buffer>>, options: BlurOptions) -> bool {
        static REBLUR_ALL: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
            std::env::var_os("NIRI_REBLUR_ALL").is_some_and(|v| v == "1")
        });
        if self.rects.is_empty() {
            return true;
        }
        let Some(area) = area else {
            return false;
        };
        if *REBLUR_ALL {
            return false;
        }
        let reach = blur_reach(options);
        let mut near = area;
        near.loc.x -= reach;
        near.loc.y -= reach;
        near.size.w += 2. * reach;
        near.size.h += 2. * reach;
        let near = near.to_i32_up();
        !self.rects.iter().any(|rect| rect.overlaps(near))
    }
}

impl Default for Elements {
    fn default() -> Self {
        Self::Unchanged(Vec::new())
    }
}

impl EffectBuffer {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            size: Size::default(),
            scale: Scale::from(1.),
            blur_options: BlurOptions::default(),
            elements: Elements::default(),
            offscreen: None,
            blur: None,
            variants: Vec::new(),
            damage: DamageBag::new(DAMAGE_HISTORY),
            frame: 0,
            used_at: 0,
            blur_used_at: 0,
        }
    }

    pub fn id(&self) -> &Id {
        &self.id
    }

    /// Whether there is nothing to render into the buffer.
    pub fn is_empty(&self) -> bool {
        match &self.elements {
            Elements::Unchanged(elements) | Elements::New(elements) => elements.is_empty(),
        }
    }

    /// Lets go of the offscreen texture and every blur chain; the elements and size stay, so
    /// the next [`prepare`](Self::prepare) renders afresh.
    pub fn release(&mut self) {
        if self.offscreen.is_none() && self.blur.is_none() && self.variants.is_empty() {
            return;
        }

        trace!("releasing effect buffer");
        self.offscreen = None;
        self.blur = None;
        self.variants.clear();
        self.damage_all();
    }

    /// Counts a frame, and lets go of what hasn't been used for [`IDLE_FRAMES`]: variants
    /// and the buffer's own blur chain first, then the whole buffer. Called once per output
    /// frame, before the frame renders.
    pub fn tick(&mut self) {
        self.frame += 1;
        let frame = self.frame;

        if self.offscreen.is_some() && frame - self.used_at > IDLE_FRAMES {
            self.release();
            return;
        }

        let before = self.variants.len();
        self.variants
            .retain(|variant| frame - variant.used_at <= IDLE_FRAMES);
        if self.variants.len() != before {
            trace!(
                "released {} idle blur variants",
                before - self.variants.len()
            );
        }

        if self.blur.is_some() && frame - self.blur_used_at > IDLE_FRAMES {
            trace!("releasing idle blur chain");
            self.blur = None;
            if let Some(offscreen) = &mut self.offscreen {
                offscreen.blurred = None;
            }
        }
    }

    pub fn blur_options(&self) -> BlurOptions {
        self.blur_options
    }

    pub fn commit(&self) -> CommitCounter {
        self.damage.current_commit()
    }

    /// Where the buffer changed since `commit`, in buffer pixels; `None` for all of it (fork).
    pub fn damage_since(&self, commit: Option<CommitCounter>) -> Option<DamageSet<i32, Buffer>> {
        self.damage.damage_since(commit)
    }

    /// All of it changed: a new commit that damages the whole buffer.
    fn damage_all(&mut self) {
        let size = self.size;
        self.damage.add([Rectangle::from_size(size)]);
    }

    pub fn logical_size(&self) -> Size<f64, Logical> {
        self.size.to_f64().to_logical(self.scale, Transform::Normal)
    }

    pub fn scale(&self) -> Scale<f64> {
        self.scale
    }

    pub fn render_element_states(&self) -> Option<&RenderElementStates> {
        self.offscreen.as_ref().map(|o| &o.states)
    }

    pub fn update_size(&mut self, size: Size<i32, Physical>, scale: Scale<f64>) {
        self.size = size.to_logical(1).to_buffer(1, Transform::Normal);
        self.scale = scale;
    }

    pub fn update_blur_options(&mut self, options: BlurOptions) {
        if self.blur_options == options {
            return;
        }

        self.blur_options = options;

        if let Some(offscreen) = &mut self.offscreen {
            if offscreen.blurred.is_some() {
                offscreen.blurred = None;
                self.damage_all();
            }
        }
    }

    pub fn elements(&mut self) -> &mut Vec<OutputRenderElements<GlesRenderer>> {
        // Assume we're going to insert new elements, switch to New.
        match mem::take(&mut self.elements) {
            Elements::Unchanged(elements) | Elements::New(elements) => {
                self.elements = Elements::New(elements);
            }
        }
        let Elements::New(elements) = &mut self.elements else {
            unreachable!();
        };
        elements
    }

    pub fn prepare(&mut self, renderer: &mut GlesRenderer, blur: bool) -> bool {
        self.prepare_with(renderer, blur, &[], true)
    }

    /// [`prepare`](Self::prepare), and blurs with each of `variants` too, for materials with
    /// options of their own. Options the same as the buffer's need no variant. The buffer's
    /// own blur chain is only prepared when `base_used`, i.e. some area draws with the
    /// buffer's options; one that turns out to need it later gets it then.
    pub fn prepare_with(
        &mut self,
        renderer: &mut GlesRenderer,
        blur: bool,
        variants: &[BlurOptions],
        base_used: bool,
    ) -> bool {
        self.used_at = self.frame;

        if let Err(err) = self.prepare_offscreen(renderer) {
            warn!("error preparing offscreen: {err:?}");
            return false;
        };

        if blur {
            if base_used {
                if let Err(err) = self.prepare_blur(renderer) {
                    warn!("error preparing blur: {err:?}");
                    return false;
                }
            }
            for options in variants {
                if *options == self.blur_options {
                    continue;
                }
                if let Err(err) = self.prepare_variant(renderer, *options) {
                    warn!("error preparing blur variant: {err:?}");
                }
            }
        }

        true
    }

    fn prepare_variant(
        &mut self,
        renderer: &mut GlesRenderer,
        options: BlurOptions,
    ) -> anyhow::Result<()> {
        let offscreen = self.offscreen.as_ref().context("missing offscreen")?;

        self.variants
            .retain(|v| v.blur.context_id() == renderer.context_id());

        // most recently asked for last
        let variant = match self.variants.iter().position(|v| v.options == options) {
            Some(at) => self.variants.remove(at),
            None => {
                let Some(blur) = Blur::new(renderer) else {
                    // Missing blur shader.
                    return Ok(());
                };
                Variant {
                    options,
                    blur,
                    blurred: None,
                    stale: Stale::default(),
                    used_at: self.frame,
                }
            }
        };
        if self.variants.len() >= MAX_VARIANTS {
            self.variants.remove(0);
        }
        let frame = self.frame;
        let variant = {
            self.variants.push(variant);
            self.variants.last_mut().unwrap()
        };
        variant.used_at = frame;

        if variant.blurred.is_some() {
            return Ok(());
        }
        variant
            .blur
            .prepare_textures(
                |fourcc, size| renderer.create_buffer(fourcc, size),
                &offscreen.texture,
                options,
            )
            .context("error preparing blur textures")
    }

    fn prepare_offscreen(&mut self, renderer: &mut GlesRenderer) -> anyhow::Result<()> {
        let _span = tracy_client::span!("EffectBuffer::prepare_offscreen");

        // Check if we need to create or recreate the texture.
        let size_string;
        let mut reason = "";
        if let Some(Offscreen {
            texture,
            renderer_context_id,
            ..
        }) = &mut self.offscreen
        {
            let old_size = texture.size();
            if old_size != self.size {
                size_string = format!(
                    "size changed from {} × {} to {} × {}",
                    old_size.w, old_size.h, self.size.w, self.size.h
                );
                reason = &size_string;

                self.offscreen = None;
            } else if !texture.is_unique_reference() {
                reason = "not unique";

                self.offscreen = None;
            } else if *renderer_context_id != renderer.context_id() {
                reason = "renderer id changed";

                self.offscreen = None;
            }
        } else {
            reason = "first render";
        }

        let offscreen = if let Some(offscreen) = &mut self.offscreen {
            offscreen
        } else {
            trace!("creating new offscreen texture: {reason}");
            unblur(&mut self.variants);
            let span = tracy_client::span!("creating effect offscreen texture");
            span.emit_text(reason);

            let texture: GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, self.size)
                .context("error creating texture")?;

            let buffer_size = self.size.to_logical(1, Transform::Normal).to_physical(1);
            let damage = OutputDamageTracker::new(buffer_size, self.scale, Transform::Normal);

            self.offscreen.insert(Offscreen {
                texture,
                renderer_context_id: renderer.context_id(),
                scale: self.scale,
                damage,
                states: RenderElementStates::default(),
                blurred: None,
                stale: Stale::default(),
            })
        };

        // Recreate the damage tracker if the scale changes. We already recreate it for buffer size
        // changes, and transform is always Normal.
        if offscreen.scale != self.scale {
            offscreen.scale = self.scale;

            trace!("recreating damage tracker due to scale change");
            let buffer_size = self.size.to_logical(1, Transform::Normal).to_physical(1);
            offscreen.damage = OutputDamageTracker::new(buffer_size, self.scale, Transform::Normal);

            offscreen.blurred = None;
            unblur(&mut self.variants);
            self.damage.add([Rectangle::from_size(self.size)]);
        }

        // Render the elements if any.
        let mut elements = match mem::take(&mut self.elements) {
            Elements::New(elements) => elements,
            x @ Elements::Unchanged(_) => {
                // No redrawing necessary.
                self.elements = x;
                return Ok(());
            }
        };

        let res = {
            let mut target = renderer
                .bind(&mut offscreen.texture)
                .context("error binding texture")?;
            offscreen
                .damage
                .render_output(renderer, &mut target, 1, &elements, Color32F::TRANSPARENT)
                .context("error rendering")?
        };

        offscreen.states = res.states;

        if let Some(damage) = res.damage {
            // the tracker renders at scale 1 with no transform: physical is buffer pixels
            self.damage.add(damage.iter().map(|rect| {
                Rectangle::<i32, Buffer>::new(
                    (rect.loc.x, rect.loc.y).into(),
                    (rect.size.w, rect.size.h).into(),
                )
            }));

            // Original texture changed: each blur is made again where it's next asked for near
            // the change (Stale), and stays for areas the change doesn't reach.
            if offscreen.blurred.is_some() {
                offscreen.stale.add(damage);
            }
            for variant in &mut self.variants {
                if variant.blurred.is_some() {
                    variant.stale.add(damage);
                }
            }
        }

        // Clear and put the storage back.
        elements.clear();
        self.elements = Elements::Unchanged(elements);

        Ok(())
    }

    fn prepare_blur(&mut self, renderer: &mut GlesRenderer) -> anyhow::Result<()> {
        self.blur_used_at = self.frame;

        let offscreen = self.offscreen.as_mut().context("missing offscreen")?;
        if offscreen.blurred.is_some() {
            // Already rendered.
            return Ok(());
        }

        if let Some(blur) = &self.blur {
            if blur.context_id() != renderer.context_id() {
                debug!("recreating blur: renderer changed");
                self.blur = None;
            }
        }

        let blur = if let Some(blur) = &mut self.blur {
            blur
        } else {
            let Some(blur) = Blur::new(renderer) else {
                // Missing blur shader.
                return Ok(());
            };
            self.blur.insert(blur)
        };

        ensure!(
            offscreen.renderer_context_id == renderer.context_id(),
            "wrong renderer context id"
        );

        blur.prepare_textures(
            |fourcc, size| renderer.create_buffer(fourcc, size),
            &offscreen.texture,
            self.blur_options,
        )
        .context("error preparing blur textures")?;

        Ok(())
    }

    pub fn render(&mut self, frame: &mut GlesFrame, blur: bool) -> anyhow::Result<GlesTexture> {
        self.render_with(frame, blur, None, None)
    }

    /// [`render`](Self::render), blurred with `options` in place of the buffer's when given:
    /// a variant [`prepare_with`](Self::prepare_with) prepared. One it didn't falls back to the
    /// buffer's own blur. `area` is the part of the buffer the caller samples, when it knows:
    /// a blur made before a change elsewhere is kept for it (fork).
    pub fn render_with(
        &mut self,
        frame: &mut GlesFrame,
        blur: bool,
        options: Option<BlurOptions>,
        area: Option<Rectangle<f64, Buffer>>,
    ) -> anyhow::Result<GlesTexture> {
        let offscreen = self.offscreen.as_mut().context("offscreen is missing")?;

        if !blur {
            return Ok(offscreen.texture.clone());
        }

        let variant = options
            .filter(|options| *options != self.blur_options)
            .and_then(|options| self.variants.iter_mut().find(|v| v.options == options));
        if let Some(variant) = variant {
            if let Some(texture) = &variant.blurred {
                if variant.stale.still_shows(area, variant.options) {
                    return Ok(texture.clone());
                }
            }
            variant.stale = Stale::default();
            // the old blur is a clone of the chain's output texture: drop it, or the chain
            // can't render into it (Blur::render wants the only reference)
            variant.blurred = None;
            let mut guard = frame.renderer();
            let renderer = guard.as_mut();
            let blurred = variant
                .blur
                .render(renderer, &offscreen.texture, variant.options)
                .context("error rendering blur variant")?;
            return Ok(variant.blurred.insert(blurred).clone());
        }

        if let Some(texture) = &offscreen.blurred {
            if offscreen.stale.still_shows(area, self.blur_options) {
                return Ok(texture.clone());
            }
        }
        offscreen.stale = Stale::default();
        // as for a variant above: the chain renders into the texture this holds
        offscreen.blurred = None;

        // An area that turns out to draw with the buffer's own options, which prepare_with
        // was told nothing needs: prepare that chain now.
        if self.blur.is_none() {
            let mut guard = frame.renderer();
            self.prepare_blur(guard.as_mut())
                .context("error preparing blur late")?;
        }

        let offscreen = self.offscreen.as_mut().context("offscreen is missing")?;
        let blur = self.blur.as_mut().context("blur is missing")?;
        let mut guard = frame.renderer();
        let renderer = guard.as_mut();
        let blurred = blur
            .render(renderer, &offscreen.texture, self.blur_options)
            .context("error rendering blur")?;

        Ok(offscreen.blurred.insert(blurred).clone())
    }
}

/// How far a change spreads through a blur with `options`, in buffer pixels: dual kawase
/// reaches about offset · 2^(passes + 1); kept wide on purpose (fork).
pub fn blur_reach(options: BlurOptions) -> f64 {
    (options.offset * f64::from(1u32 << (options.passes + 1))).ceil() + 2.
}

/// Drops the variants' blurred textures, for contents that changed.
fn unblur(variants: &mut [Variant]) {
    for variant in variants {
        variant.blurred = None;
    }
}

#[cfg(test)]
mod stale_tests {
    use super::*;

    const OPTIONS: BlurOptions = BlurOptions {
        passes: 4,
        offset: 2.,
    };

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn area(x: f64, y: f64, w: f64, h: f64) -> Option<Rectangle<f64, Buffer>> {
        Some(Rectangle::new((x, y).into(), (w, h).into()))
    }

    #[test]
    fn a_change_far_from_the_bar_keeps_its_blur() {
        let mut stale = Stale::default();
        assert!(stale.still_shows(area(0., 0., 2880., 42.), OPTIONS));

        // a widget repaint in the middle of the screen
        stale.add(&[rect(1200, 600, 400, 300)]);
        assert!(stale.still_shows(area(0., 0., 2880., 42.), OPTIONS));
        // the widget's own glass samples right there
        assert!(!stale.still_shows(area(1100., 500., 600., 500.), OPTIONS));
        // a caller that doesn't say where it samples always gets a fresh blur
        assert!(!stale.still_shows(None, OPTIONS));
    }

    #[test]
    fn a_change_within_the_blurs_reach_blurs_again() {
        let mut stale = Stale::default();
        // 4 passes at offset 2 reach about 64 px
        stale.add(&[rect(0, 90, 100, 10)]);
        assert!(!stale.still_shows(area(0., 0., 2880., 42.), OPTIONS));
        let mut stale = Stale::default();
        stale.add(&[rect(0, 200, 100, 10)]);
        assert!(stale.still_shows(area(0., 0., 2880., 42.), OPTIONS));
    }

    #[test]
    fn many_changes_fold_into_their_bounding_box() {
        let mut stale = Stale::default();
        for i in 0..40 {
            stale.add(&[rect(1000 + i, 800, 1, 1)]);
        }
        assert!(stale.rects.len() <= MAX_STALE);
        assert!(stale.still_shows(area(0., 0., 2880., 42.), OPTIONS));
    }
}
