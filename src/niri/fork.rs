//! The fork's parts of `Niri` (fork): its protocol states, materials, desktop layers' fade and
//! popups, and letting capture buffers go. `niri.rs` holds one field and one-line calls, so a
//! rebase meets few fork hunks there.

use niri_config::OutputName;
use smithay::backend::renderer::Color32F;
use smithay::desktop::LayerMap;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::wayland::shell::wlr_layer::Layer;

use super::{Niri, State};
use crate::layer::mapped::LayerSurfaceRenderElement;
use crate::layer::MappedLayer;
use crate::protocols::blur_shapes::BlurShapesManagerState;
use crate::protocols::color_management::ColorManagementState;
use crate::protocols::secure_fill::SecureFillManagerState;
use crate::protocols::vibrancy::VibrancyManagerState;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::{background_effect, RenderCtx, RenderTarget};

/// The fork's Wayland protocol states.
pub struct ForkStates {
    pub blur_shapes: BlurShapesManagerState,
    pub vibrancy: VibrancyManagerState,
    pub secure_fill: SecureFillManagerState,
    pub color_management: ColorManagementState,
    /// How much the wallpaper is dimmed, 0–100 (`SetWallpaperDim`): emaki's Black appearance.
    pub wallpaper_dim: u8,
}

impl ForkStates {
    /// Creates the fork's globals, and gives blur shapes the configured materials.
    pub fn new(
        display_handle: &DisplayHandle,
        materials: &niri_config::Materials,
        client_is_unrestricted: impl Fn(&Client) -> bool + Send + Sync + 'static,
    ) -> Self {
        let mut states = Self {
            blur_shapes: BlurShapesManagerState::new::<State>(display_handle),
            vibrancy: VibrancyManagerState::new::<State>(display_handle),
            secure_fill: SecureFillManagerState::new::<State, _>(
                display_handle,
                client_is_unrestricted,
            ),
            color_management: ColorManagementState::new::<State>(display_handle),
            wallpaper_dim: 0,
        };
        states.set_materials(materials);
        states
    }

    /// The output's backdrop colour as the config gives it, made black while the wallpaper is
    /// dimmed: a dimmed wallpaper fades to black, and a hidden one leaves black.
    pub fn black_backdrop(&self, color: &mut [f32; 4]) {
        if self.wallpaper_dim > 0 {
            *color = [0., 0., 0., 1.];
        }
    }

    fn set_materials(&mut self, materials: &niri_config::Materials) {
        background_effect::set_materials(materials);
        let names = materials.0.iter().map(|m| m.name.clone()).collect();
        self.blur_shapes.set_materials(names);
    }
}

impl Niri {
    /// On a config reload: materials that changed reach the renderer and blur-shape clients, and
    /// everything draws again.
    pub fn reload_materials(&mut self, materials: &niri_config::Materials) {
        if self.config.borrow().materials == *materials {
            return;
        }
        self.fork.set_materials(materials);
        self.queue_redraw_all();
    }

    /// Lets go of the screencast and screen-capture targets' xray buffers on every output,
    /// after a render that used them once. A running cast renders its target again next frame
    /// and gets them back then.
    pub fn release_capture_xray(&self) {
        for state in self.output_state.values() {
            state.xray.release_target(RenderTarget::Screencast);
            state.xray.release_target(RenderTarget::ScreenCapture);
        }
    }

    /// Dims the wallpaper by `percent` (0–100) over a black backdrop, as `SetWallpaperDim`
    /// asks, and draws every output again.
    pub fn set_wallpaper_dim(&mut self, percent: u8) {
        let percent = percent.min(100);
        if self.fork.wallpaper_dim == percent {
            return;
        }
        self.fork.wallpaper_dim = percent;

        let config = self.config.borrow();
        for (output, state) in self.output_state.iter_mut() {
            let name = output.user_data().get::<OutputName>().unwrap();
            let mut color = config
                .outputs
                .find(name)
                .and_then(|c| c.backdrop_color)
                .unwrap_or(config.overview.backdrop_color)
                .to_array_unpremul();
            color[3] = 1.;
            self.fork.black_backdrop(&mut color);
            state.backdrop_buffer.set_color(Color32F::from(color));
        }
        drop(config);
        self.queue_redraw_all();
    }

    /// The opacity a layer surface is drawn with on top of its rules: the wallpaper (a
    /// background layer that isn't a desktop layer) dims with `SetWallpaperDim`, and a desktop
    /// layer fades out as the overview opens.
    pub(super) fn layer_fade(&self, mapped: &MappedLayer) -> f32 {
        if !mapped.is_desktop() {
            if mapped.surface().layer() == Layer::Background {
                return wallpaper_fade(self.fork.wallpaper_dim);
            }
            return 1.;
        }

        (1. - self.layout.overview_progress_value()).clamp(0., 1.) as f32
    }

    /// Renders the popups of desktop layers, which stay put at the output's origin like the
    /// layers themselves but go with the other background popups, above the windows.
    pub(super) fn render_desktop_popups<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        layer_map: &LayerMap,
        push: &mut dyn FnMut(LayerSurfaceRenderElement<R>),
    ) {
        for (mapped, geo) in self.layers_in_render_order(layer_map, Layer::Background, true) {
            if !mapped.is_desktop() {
                continue;
            }

            let fade = self.layer_fade(mapped);
            if fade <= 0. {
                continue;
            }

            let loc = geo.loc.to_f64();
            let xray_pos = XrayPos::default().offset(loc);
            mapped.render_popups(ctx.r(), None, loc, xray_pos, fade, push);
        }
    }
}

/// The wallpaper's opacity at `dim` percent: 0 at 100, where it isn't drawn at all.
fn wallpaper_fade(dim: u8) -> f32 {
    1. - f32::from(dim.min(100)) / 100.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_dim_hides_the_wallpaper() {
        assert_eq!(wallpaper_fade(0), 1.);
        assert_eq!(wallpaper_fade(75), 0.25);
        assert_eq!(wallpaper_fade(100), 0.);
        assert_eq!(wallpaper_fade(200), 0.);
    }
}
