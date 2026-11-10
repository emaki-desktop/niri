//! HDR10 on a TTY output (fork): deciding it on connect, the connector's properties and
//! metadata, the half-float composition the frame is encoded from, and putting the connector
//! back in SDR for whoever drives it next.
//!
//! `tty.rs` calls into this at each step with one line, so a rebase meets few HDR hunks there.

use std::os::fd::AsFd;

use anyhow::{bail, Context};
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::compositor::{
    DrmCompositor, FrameFlags, PrimaryPlaneElement, RenderFrameResult,
};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::DrmDevice;
use smithay::backend::renderer::element::{RenderElement, RenderElementStates};
use smithay::output::{Output, OutputModeSource};
use smithay::reexports::drm::control::{self, connector, crtc, property, Device as _};

use super::{
    get_edid_info, reset_hdr, ConnectorProperties, GbmDrmCompositor, OutputDevice, Surface,
    TtyRenderer, SUPPORTED_COLOR_FORMATS,
};
use crate::render_helpers::color::{self, ColorTarget};
use crate::render_helpers::hdr::HdrBuffer;
use crate::render_helpers::renderer::AsGlesRenderer;

/// Scanout formats for an output in HDR mode: 10 bits a channel, which PQ needs.
const HDR_COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Xrgb2101010, Fourcc::Xbgr2101010];

/// Whether an output connecting now goes in HDR: it's asked for, its EDID has PQ, and it renders
/// on the primary GPU, where its composition lives. Also returns what the EDID says.
pub(super) fn decide(
    wanted: bool,
    drm: &DrmDevice,
    connector: connector::Handle,
    on_primary: bool,
    name: &str,
) -> (bool, Option<HdrCaps>) {
    let caps = hdr_caps(drm, connector);
    let hdr = wanted && caps.is_some() && on_primary;
    if wanted && !hdr {
        warn!("output {name}: HDR asked for, but it can't show HDR here");
    }
    (hdr, caps)
}

/// Puts the connector in HDR10 when `hdr`, or resets it otherwise. If HDR can't be set, the
/// output stays in SDR: `hdr` turns false and only that is logged. A failed reset is returned.
pub(super) fn apply(
    props: &ConnectorProperties,
    caps: Option<HdrCaps>,
    hdr: &mut bool,
) -> anyhow::Result<()> {
    let Some(caps) = caps.filter(|_| *hdr) else {
        return reset_sdr(props);
    };
    if let Err(err) = set_hdr(props, &caps) {
        warn!("couldn't set HDR properties, staying in SDR: {err:?}");
        *hdr = false;
        let _ = reset_sdr(props);
    }
    Ok(())
}

/// The scanout formats to ask the DRM compositor for.
pub(super) fn color_formats(hdr: bool) -> &'static [Fourcc] {
    if hdr {
        &HDR_COLOR_FORMATS
    } else {
        &SUPPORTED_COLOR_FORMATS
    }
}

/// Resets the connector's HDR properties, ignoring errors.
pub(super) fn reset(drm: &DrmDevice, connector: connector::Handle) {
    if let Ok(props) = ConnectorProperties::try_new(drm, connector) {
        let _ = reset_sdr(&props);
    }
}

/// The SDR compositor an output falls back to when the DRM can't take its 10-bit buffers, since
/// a screen left black is worse than SDR. Resets the connector first.
pub(super) fn sdr_compositor(
    device: &mut OutputDevice,
    crtc: crtc::Handle,
    mode: control::Mode,
    connector: &connector::Info,
    output: &Output,
    render_formats: FormatSet,
) -> anyhow::Result<GbmDrmCompositor> {
    reset(&device.drm, connector.handle());
    let surface = device
        .drm
        .create_surface(crtc, mode, &[connector.handle()])?;
    DrmCompositor::new(
        OutputModeSource::Auto(output.downgrade()),
        surface,
        None,
        device.allocator.clone(),
        GbmFramebufferExporter::new(device.gbm.clone(), device.render_node.into()),
        SUPPORTED_COLOR_FORMATS,
        render_formats,
        device.drm.cursor_size(),
        Some(device.gbm.clone()),
    )
    .context("error creating DRM compositor")
}

/// Leaves HDR when the scanout format came out 8-bit anyway, since PQ in 8 bits bands badly.
pub(super) fn check_scanout(
    hdr: &mut bool,
    format: Fourcc,
    drm: &DrmDevice,
    connector: connector::Handle,
    name: &str,
) {
    if *hdr && !HDR_COLOR_FORMATS.contains(&format) {
        warn!("output {name}: no 10-bit scanout ({format:?}), staying in SDR");
        *hdr = false;
        reset(drm, connector);
    }
    if *hdr {
        info!("output {name}: HDR on, scanning out {format:?}");
    }
}

/// Before the session pauses: the next VT's owner (a text console, a greeter) shouldn't be
/// shown as PQ.
pub(super) fn reset_before_pause(device: &OutputDevice) {
    for surface in device.surfaces.values().filter(|s| s.hdr.is_some()) {
        reset(&device.drm, surface.connector);
    }
}

/// On resume, an HDR output gets its HDR back, since whoever had the VT may have reset it; any
/// other output is reset.
pub(super) fn restore(props: &ConnectorProperties, surface: &Surface) -> anyhow::Result<()> {
    match surface.hdr_caps.filter(|_| surface.hdr.is_some()) {
        Some(caps) => set_hdr(props, &caps),
        None => reset_sdr(props),
    }
}

/// On disconnect: whatever drives the connector next shouldn't inherit PQ.
pub(super) fn reset_on_disconnect(drm: &DrmDevice, surface: &Surface) {
    if surface.hdr.is_none() {
        return;
    }
    if let Ok(props) = ConnectorProperties::try_new(drm, surface.connector) {
        if let Err(err) = reset_sdr(&props) {
            debug!("couldn't reset HDR properties: {err:?}");
        }
    }
}

/// An output in HDR mode draws for an HDR target, from its elements (and the windows drawn
/// offscreen among them) to its composition: surfaces with HDR content keep their highlights,
/// up to what the panel shows. Called before its elements are rendered.
pub(super) fn set_target(surface: &Surface, renderer: &mut TtyRenderer) {
    if let Some(hdr) = &surface.hdr {
        let peak = surface.hdr_caps.map_or(1000., |caps| caps.max);
        let target = ColorTarget::hdr(hdr.params().sdr_white, peak);
        color::set_target(renderer.as_gles_renderer(), target);
    }
}

/// What a frame handed to the DRM compositor came to.
pub(super) struct Rendered {
    /// Whether nothing changed.
    pub is_empty: bool,
    /// The states of the elements in it.
    pub states: RenderElementStates,
}

/// Hands an output's elements to the DRM. In HDR mode they're composited first, and the DRM
/// gets the composition encoded for the panel: nothing goes straight to a plane, since a
/// client's or the cursor's sRGB buffer on a PQ plane would show at the wrong brightness.
/// Waits for the GPU when the frame can't be fenced, or when `wait`.
pub(super) fn render_frame<'render, E: RenderElement<TtyRenderer<'render>>>(
    compositor: &mut GbmDrmCompositor,
    hdr: Option<&mut HdrBuffer>,
    renderer: &mut TtyRenderer<'render>,
    output: &Output,
    elements: &[E],
    flags: FrameFlags,
    wait: bool,
) -> Result<Rendered, String> {
    let Some(hdr) = hdr else {
        return compositor
            .render_frame::<_, _>(renderer, elements, [0.; 4], flags)
            .map(|res| settle_frame(res, wait))
            .map_err(|err| err.to_string());
    };

    let size = output
        .current_transform()
        .transform_size(output.current_mode().map(|m| m.size).unwrap_or_default());
    let scale = output.current_scale().fractional_scale().into();
    let res = hdr.render(renderer, size, scale, elements);
    color::set_target(renderer.as_gles_renderer(), ColorTarget::SDR);
    match res {
        Ok((element, states)) => compositor
            .render_frame::<_, _>(renderer, &[element], [0.; 4], FrameFlags::empty())
            .map(|res| Rendered {
                is_empty: settle_frame(res, wait).is_empty,
                states,
            })
            .map_err(|err| err.to_string()),
        Err(err) => Err(format!("{err:?}")),
    }
}

/// In HDR mode the gamma ramp is applied while encoding, since under PQ the CRTC's gamma LUT
/// would bend the encoded signal; the LUT stays linear. Returns whether the ramp was taken.
pub(super) fn set_ramp(surface: &mut Surface, ramp: Option<&[u16]>) -> bool {
    let Some(hdr) = &mut surface.hdr else {
        return false;
    };
    let mut params = hdr.params();
    params.set_ramp(ramp);
    hdr.set_params(params);
    true
}

/// Whether the output's HDR setting changed in a way it can follow. HDR changes the scanout
/// format and the connector's properties, so the output is then set up again from scratch, as
/// when it's turned off and on.
pub(super) fn needs_reconnect(wanted: bool, surface: &Surface) -> bool {
    let on = surface.hdr.is_some();
    wanted != on && (wanted || on) && (!wanted || surface.hdr_caps.is_some())
}

/// Follows the configured SDR brightness on an output in HDR mode.
pub(super) fn set_sdr_white(surface: &mut Surface, nits: f32) {
    if let Some(hdr) = &mut surface.hdr {
        let mut params = hdr.params();
        params.sdr_white = nits;
        hdr.set_params(params);
    }
}

/// What a frame handed to the DRM compositor came to. Waits for the GPU first when the frame
/// can't be fenced, or when asked to.
fn settle_frame<
    B: smithay::backend::allocator::Buffer,
    F: smithay::backend::drm::Framebuffer,
    E,
>(
    res: RenderFrameResult<'_, B, F, E>,
    wait: bool,
) -> Rendered {
    if res.needs_sync() || wait {
        if let PrimaryPlaneElement::Swapchain(element) = &res.primary_element {
            let _span = tracy_client::span!("wait for completion");
            if let Err(err) = element.sync.wait() {
                warn!("error waiting for frame completion: {err:?}");
            }
        }
    }
    Rendered {
        is_empty: res.is_empty,
        states: res.states,
    }
}

/// What an output's EDID says about HDR: enough for the metadata niri sends it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HdrCaps {
    /// Peak, full-frame average and black, in nits.
    pub(super) max: f32,
    pub(super) average: f32,
    pub(super) min: f32,
    /// Red, green and blue, then white, CIE 1931 xy; none when the EDID doesn't say.
    pub(super) primaries: Option<[[f32; 2]; 4]>,
}

/// HDR10 support from the EDID: the panel decodes PQ.
pub(super) fn hdr_caps(device: &DrmDevice, connector: connector::Handle) -> Option<HdrCaps> {
    let info = get_edid_info(device, connector).ok()?;
    let meta = info.hdr_static_metadata();
    if !meta.pq {
        return None;
    }

    let colors = info.default_color_primaries();
    let primaries = (colors.has_primaries && colors.has_default_white_point).then(|| {
        let [r, g, b] = colors.primary;
        let w = colors.default_white;
        [[r.x, r.y], [g.x, g.y], [b.x, b.y], [w.x, w.y]]
    });

    // a panel that leaves its luminance out is taken as HDR10's usual 1000 nits
    let max = Some(meta.desired_content_max_luminance)
        .filter(|nits| *nits > 0.)
        .unwrap_or(1000.);
    Some(HdrCaps {
        max,
        average: Some(meta.desired_content_max_frame_avg_luminance)
            .filter(|nits| *nits > 0.)
            .unwrap_or(max),
        min: meta.desired_content_min_luminance.max(0.),
        primaries,
    })
}

/// `struct hdr_output_metadata` with HDMI static metadata type 1, as the kernel takes it: PQ,
/// the panel's primaries (BT.2020's when unknown) and its luminance.
fn hdr_metadata_blob(caps: &HdrCaps) -> [u8; 32] {
    const BT2020: [[f32; 2]; 4] = [
        [0.708, 0.292],
        [0.170, 0.797],
        [0.131, 0.046],
        [0.3127, 0.329],
    ];
    const EOTF_SMPTE_ST2084: u8 = 2;

    let primaries = caps.primaries.unwrap_or(BT2020);
    // chromaticity in 0.00002 steps, as CTA-861 has it
    let xy = |v: f32| ((v * 50000.).round() as u32).min(50000) as u16;
    let nits = |v: f32| (v.round() as u32).min(u32::from(u16::MAX)) as u16;

    let mut words = Vec::with_capacity(12);
    for [x, y] in primaries {
        words.push(xy(x));
        words.push(xy(y));
    }
    words.push(nits(caps.max));
    // the darkest in 0.0001 nit steps
    words.push(((caps.min * 10000.).round() as u32).min(u32::from(u16::MAX)) as u16);
    // the content's light levels: niri can't know them, so the panel's own
    words.push(nits(caps.max));
    words.push(nits(caps.average));

    let mut blob = [0u8; 32];
    // metadata_type 0 (HDMI static metadata type 1), then the infoframe: EOTF and its type 0
    blob[4] = EOTF_SMPTE_ST2084;
    for (i, word) in words.iter().enumerate() {
        blob[6 + i * 2..8 + i * 2].copy_from_slice(&word.to_ne_bytes());
    }
    blob
}

/// The `max bpc` [`set_hdr`] asks for.
const HDR_MAX_BPC: u64 = 10;

/// The `max bpc` an SDR connector goes back to: 8 bits a channel carry the 8-bit scanout
/// formats SDR uses, and a narrower link draws less power.
const SDR_MAX_BPC: u64 = 8;

/// Puts the connector back in SDR: upstream's [`reset_hdr`], and the link's bits per channel,
/// which upstream never raises and so never resets. Left at 10 after an HDR episode
/// (`hdr-auto`), an eDP panel would keep a wider link for the rest of the session.
fn reset_sdr(props: &ConnectorProperties) -> anyhow::Result<()> {
    let hdr = reset_hdr(props);
    let bpc = reset_max_bpc(props);
    hdr.and(bpc)
}

/// Lowers `max bpc` from the value HDR set. Any other value is someone else's choice (the
/// kernel's default, a tool's), so it's left as it is, as is a connector without the property.
fn reset_max_bpc(props: &ConnectorProperties) -> anyhow::Result<()> {
    let Ok((info, value)) = props.find(c"max bpc") else {
        return Ok(());
    };
    if *value != HDR_MAX_BPC {
        return Ok(());
    }
    if let property::ValueType::UnsignedRange(min, max) = info.value_type() {
        props
            .device
            .set_property(props.connector, info.handle(), SDR_MAX_BPC.clamp(min, max))
            .context("error setting max bpc")?;
    }
    Ok(())
}

/// Puts the connector in HDR10: 10 bits a channel on the link, BT.2020 colorimetry, and PQ
/// metadata. amdgpu turns these into a modeset of their own.
fn set_hdr(props: &ConnectorProperties, caps: &HdrCaps) -> anyhow::Result<()> {
    let (info, _) = props.find(c"max bpc")?;
    if let property::ValueType::UnsignedRange(min, max) = info.value_type() {
        props
            .device
            .set_property(props.connector, info.handle(), HDR_MAX_BPC.clamp(min, max))
            .context("error setting max bpc")?;
    }

    let (info, _) = props.find(c"Colorspace")?;
    let property::ValueType::Enum(values) = info.value_type() else {
        bail!("wrong property type")
    };
    let bt2020 = values
        .values()
        .1
        .iter()
        .find(|value| value.name() == c"BT2020_RGB")
        .context("no BT2020_RGB colorspace")?
        .value();
    props
        .device
        .set_property(props.connector, info.handle(), bt2020)
        .context("error setting Colorspace")?;

    let (info, _) = props.find(c"HDR_OUTPUT_METADATA")?;
    let property::ValueType::Blob = info.value_type() else {
        bail!("wrong property type")
    };
    let mut data = hdr_metadata_blob(caps);
    let blob = drm_ffi::mode::create_property_blob(props.device.as_fd(), &mut data)
        .context("error creating HDR metadata blob")?;
    let res = props
        .device
        .set_property(
            props.connector,
            info.handle(),
            property::Value::Blob(u64::from(blob.blob_id)).into(),
        )
        .context("error setting HDR_OUTPUT_METADATA");
    // the connector holds its own reference
    let _ = props.device.destroy_property_blob(u64::from(blob.blob_id));
    res
}

/// How long content brighter than SDR white shows before an `hdr-auto` output goes HDR, so a
/// thumbnail scrolled past doesn't blank the screen for a modeset.
const AUTO_ON: std::time::Duration = std::time::Duration::from_secs(1);

/// How long an `hdr-auto` output stays in HDR after the last bright content went, so pausing or
/// seeking a video doesn't switch twice.
const AUTO_OFF: std::time::Duration = std::time::Duration::from_secs(10);

/// What `hdr-auto` knows of one output, by connector name: it outlives the reconnect that
/// switching takes.
#[derive(Debug, Clone, Copy)]
struct Auto {
    /// Whether the last render drew bright content, and since when that has been so.
    bright: bool,
    since: std::time::Instant,
    /// Whether the output is in HDR for it.
    on: bool,
    /// Whether a check is already on the event loop.
    armed: bool,
}

std::thread_local! {
    static AUTO: std::cell::RefCell<std::collections::HashMap<String, Auto>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Whether the output should be in HDR: `hdr`, or `hdr-auto` with bright content showing.
pub(super) fn wanted(config: &niri_config::Output, connector: &str) -> bool {
    config.hdr
        || (config.hdr_auto
            && AUTO.with_borrow(|auto| auto.get(connector).is_some_and(|auto| auto.on)))
}

/// Where `hdr-auto` stands after one look at an output.
enum Step {
    Same,
    Switch,
    CheckIn(std::time::Duration),
}

fn next_step(auto: &mut Auto, now: std::time::Instant) -> Step {
    let elapsed = now.saturating_duration_since(auto.since);
    let (wait, switch) = match (auto.bright, auto.on) {
        (true, false) => (AUTO_ON, elapsed >= AUTO_ON),
        (false, true) => (AUTO_OFF, elapsed >= AUTO_OFF),
        _ => return Step::Same,
    };
    if switch {
        auto.on = !auto.on;
        Step::Switch
    } else {
        Step::CheckIn(wait - elapsed)
    }
}

/// After an output's elements were gathered: follows `hdr-auto` from whether they held content
/// brighter than SDR white, switching the output (a reconnect, through the config path) once it
/// has shown for [`AUTO_ON`] or been gone for [`AUTO_OFF`].
pub(super) fn follow_content(
    config: &niri_config::Output,
    connector: &str,
    event_loop: &smithay::reexports::calloop::LoopHandle<'static, crate::niri::State>,
) {
    let bright = crate::render_helpers::hdr::take_bright();
    if config.hdr || !config.hdr_auto {
        return;
    }

    let now = std::time::Instant::now();
    let step = AUTO.with_borrow_mut(|autos| {
        let auto = autos.entry(connector.to_owned()).or_insert(Auto {
            bright,
            since: now,
            on: false,
            armed: false,
        });
        if auto.bright != bright {
            auto.bright = bright;
            auto.since = now;
        }
        let step = next_step(auto, now);
        if matches!(step, Step::CheckIn(_)) {
            if auto.armed {
                return Step::Same;
            }
            auto.armed = true;
        }
        step
    });

    arm(step, connector, event_loop);
}

fn arm(
    step: Step,
    connector: &str,
    event_loop: &smithay::reexports::calloop::LoopHandle<'static, crate::niri::State>,
) {
    use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

    match step {
        Step::Same => (),
        Step::Switch => {
            info!("output {connector}: hdr-auto switches HDR");
            event_loop.insert_idle(|state| state.backend.on_output_config_changed(&mut state.niri));
        }
        // nothing may render in the meantime (a paused video, an idle screen), so a timer looks
        Step::CheckIn(wait) => {
            let connector = connector.to_owned();
            let handle = event_loop.clone();
            let _ = event_loop.insert_source(Timer::from_duration(wait), move |_, _, state| {
                let step = AUTO.with_borrow_mut(|autos| {
                    let Some(auto) = autos.get_mut(&connector) else {
                        return Step::Same;
                    };
                    auto.armed = false;
                    next_step(auto, std::time::Instant::now())
                });
                // a check still too early arms again from here
                if let Step::CheckIn(_) = step {
                    AUTO.with_borrow_mut(|autos| {
                        if let Some(auto) = autos.get_mut(&connector) {
                            auto.armed = true;
                        }
                    });
                }
                arm(step, &connector, &handle);
                let _ = state;
                TimeoutAction::Drop
            });
        }
    }
}

#[cfg(test)]
mod auto_tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn bright_content_switches_on_after_a_second_and_off_ten_after_it_goes() {
        let start = Instant::now();
        let mut auto = Auto {
            bright: true,
            since: start,
            on: false,
            armed: false,
        };
        assert!(matches!(next_step(&mut auto, start), Step::CheckIn(_)));
        assert!(matches!(
            next_step(&mut auto, start + Duration::from_millis(1100)),
            Step::Switch
        ));
        assert!(auto.on);

        let gone = start + Duration::from_secs(5);
        auto.bright = false;
        auto.since = gone;
        assert!(matches!(
            next_step(&mut auto, gone + Duration::from_secs(9)),
            Step::CheckIn(_)
        ));
        assert!(auto.on);
        assert!(matches!(
            next_step(&mut auto, gone + Duration::from_secs(10)),
            Step::Switch
        ));
        assert!(!auto.on);
        assert!(matches!(
            next_step(&mut auto, gone + Duration::from_secs(11)),
            Step::Same
        ));
    }
}
