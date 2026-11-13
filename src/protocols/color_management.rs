//! wp-color-management-v1: clients describe the colour of what they draw (HDR video in PQ and
//! BT.2020, a game's scRGB), and learn what each output shows.
//!
//! Only parametric descriptions are taken, no ICC files. A surface with a description other than
//! plain sRGB is converted as it's drawn ([`ColorParams`]); a surface without one is sRGB, drawn
//! as niri always has. An output in HDR mode describes itself as BT.2020 and PQ, its reference
//! white the SDR brightness and its target luminance the panel's; otherwise as gamma 2.2 sRGB.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_management_surface_feedback_v1::{self, WpColorManagementSurfaceFeedbackV1},
    wp_color_management_surface_v1::{self, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{self, WpColorManagerV1},
    wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
    wp_image_description_creator_params_v1::{self, WpImageDescriptionCreatorParamsV1},
    wp_image_description_info_v1::WpImageDescriptionInfoV1,
    wp_image_description_reference_v1::WpImageDescriptionReferenceV1,
    wp_image_description_v1::{self, WpImageDescriptionV1},
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum, Weak,
};
use smithay::wayland::compositor::{with_states, Cacheable, SurfaceData};

const VERSION: u32 = 2;

type Tf = wp_color_manager_v1::TransferFunction;
type NamedPrimaries = wp_color_manager_v1::Primaries;

/// CIE 1931 xy of red, green, blue and white.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Primaries(pub [[f64; 2]; 4]);

const D65: [f64; 2] = [0.3127, 0.329];

impl Primaries {
    pub const SRGB: Self = Self([[0.64, 0.33], [0.30, 0.60], [0.15, 0.06], D65]);
    pub const BT2020: Self = Self([[0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65]);

    fn named(name: NamedPrimaries) -> Option<Self> {
        Some(match name {
            NamedPrimaries::Srgb => Self::SRGB,
            NamedPrimaries::PalM => {
                Self([[0.67, 0.33], [0.21, 0.71], [0.14, 0.08], [0.310, 0.316]])
            }
            NamedPrimaries::Pal => Self([[0.64, 0.33], [0.29, 0.60], [0.15, 0.06], D65]),
            NamedPrimaries::Ntsc => Self([[0.630, 0.340], [0.310, 0.595], [0.155, 0.070], D65]),
            NamedPrimaries::GenericFilm => Self([
                [0.681, 0.319],
                [0.243, 0.692],
                [0.145, 0.049],
                [0.310, 0.316],
            ]),
            NamedPrimaries::Bt2020 => Self::BT2020,
            NamedPrimaries::DciP3 => Self([
                [0.680, 0.320],
                [0.265, 0.690],
                [0.150, 0.060],
                [0.314, 0.351],
            ]),
            NamedPrimaries::DisplayP3 => {
                Self([[0.680, 0.320], [0.265, 0.690], [0.150, 0.060], D65])
            }
            NamedPrimaries::AdobeRgb => Self([[0.64, 0.33], [0.21, 0.71], [0.15, 0.06], D65]),
            _ => return None,
        })
    }

    fn close_to(&self, other: &Self) -> bool {
        self.0
            .iter()
            .flatten()
            .zip(other.0.iter().flatten())
            .all(|(a, b)| (a - b).abs() < 0.002)
    }

    /// RGB to XYZ for these primaries.
    fn rgb_to_xyz(&self) -> [[f64; 3]; 3] {
        let xyz = |[x, y]: [f64; 2]| [x / y, 1., (1. - x - y) / y];
        let [r, g, b, w] = self.0.map(xyz);
        let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let s = mul_vec(&invert(&m), w);
        [
            [m[0][0] * s[0], m[0][1] * s[1], m[0][2] * s[2]],
            [m[1][0] * s[0], m[1][1] * s[1], m[1][2] * s[2]],
            [m[2][0] * s[0], m[2][1] * s[1], m[2][2] * s[2]],
        ]
    }

    /// Linear RGB in these primaries to linear BT.709, the white points adapted (Bradford), as a
    /// row-major 3×3.
    pub fn bt709_matrix(&self) -> [[f64; 3]; 3] {
        let src = self.rgb_to_xyz();
        let dst = invert(&Primaries::SRGB.rgb_to_xyz());
        let adapt = bradford(self.0[3], D65);
        mul(&dst, &mul(&adapt, &src))
    }
}

fn bradford(from: [f64; 2], to: [f64; 2]) -> [[f64; 3]; 3] {
    const B: [[f64; 3]; 3] = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let xyz = |[x, y]: [f64; 2]| [x / y, 1., (1. - x - y) / y];
    let (s, d) = (mul_vec(&B, xyz(from)), mul_vec(&B, xyz(to)));
    let scale = [
        [d[0] / s[0], 0., 0.],
        [0., d[1] / s[1], 0.],
        [0., 0., d[2] / s[2]],
    ];
    mul(&invert(&B), &mul(&scale, &B))
}

fn mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn mul_vec(a: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2])
}

fn invert(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let c = |a: usize, b: usize, c: usize, d: usize| m[a][b] * m[c][d];
    [
        [
            c(1, 1, 2, 2) - c(1, 2, 2, 1),
            c(0, 2, 2, 1) - c(0, 1, 2, 2),
            c(0, 1, 1, 2) - c(0, 2, 1, 1),
        ],
        [
            c(1, 2, 2, 0) - c(1, 0, 2, 2),
            c(0, 0, 2, 2) - c(0, 2, 2, 0),
            c(0, 2, 1, 0) - c(0, 0, 1, 2),
        ],
        [
            c(1, 0, 2, 1) - c(1, 1, 2, 0),
            c(0, 1, 2, 0) - c(0, 0, 2, 1),
            c(0, 0, 1, 1) - c(0, 1, 1, 0),
        ],
    ]
    .map(|row| row.map(|v| v / det))
}

/// How a description's signal turns into light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transfer {
    /// A pure power: gamma 2.2, 2.8, BT.1886's 2.4, or `set_tf_power`.
    Power(f64),
    /// The piecewise sRGB curve (also compound power 2.4).
    Srgb,
    /// ST 2084, absolute up to 10 000 nits.
    Pq,
    /// Linear, extended past 1.0.
    Linear,
}

impl Transfer {
    fn named(tf: Tf) -> Option<Self> {
        Some(match tf {
            Tf::Bt1886 => Self::Power(2.4),
            Tf::Gamma22 => Self::Power(2.2),
            Tf::Gamma28 => Self::Power(2.8),
            Tf::Srgb | Tf::CompoundPower24 => Self::Srgb,
            Tf::ExtLinear => Self::Linear,
            Tf::St2084Pq => Self::Pq,
            _ => return None,
        })
    }

    /// The default (min, max, reference) luminance it implies.
    fn default_luminance(self) -> Luminance {
        match self {
            Self::Pq => Luminance {
                min: 0.005,
                max: 10000.,
                reference: 203.,
            },
            Self::Power(gamma) if (gamma - 2.4).abs() < 1e-6 => Luminance {
                min: 0.01,
                max: 100.,
                reference: 100.,
            },
            _ => Luminance {
                min: 0.2,
                max: 80.,
                reference: 80.,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Luminance {
    pub min: f64,
    pub max: f64,
    pub reference: f64,
}

/// A colour description, from a client or for an output.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageDescription {
    identity: u64,
    pub transfer: Transfer,
    pub primaries: Primaries,
    pub luminance: Luminance,
    /// The display the content was mastered on, and the content's light levels.
    pub mastering: Option<(Primaries, f64, f64)>,
    pub max_cll: Option<f64>,
    pub max_fall: Option<f64>,
    /// Whether `get_information` may be asked of it (only outputs' descriptions).
    info: bool,
}

fn next_identity() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl ImageDescription {
    pub fn srgb() -> Self {
        Self {
            identity: next_identity(),
            transfer: Transfer::Power(2.2),
            primaries: Primaries::SRGB,
            luminance: Transfer::Srgb.default_luminance(),
            mastering: None,
            max_cll: None,
            max_fall: None,
            info: true,
        }
    }

    /// An output in HDR mode: BT.2020 and PQ, SDR white at `sdr_white`, showing up to `peak`.
    pub fn hdr_output(sdr_white: f64, min: f64, peak: f64) -> Self {
        Self {
            identity: next_identity(),
            transfer: Transfer::Pq,
            primaries: Primaries::BT2020,
            luminance: Luminance {
                min: 0.005,
                max: 10000.,
                reference: sdr_white,
            },
            mastering: Some((Primaries::BT2020, min, peak)),
            max_cll: None,
            max_fall: None,
            info: true,
        }
    }

    /// Windows-scRGB: linear BT.709, 1.0 at 80 nits, its reference white taken as 203.
    fn windows_scrgb() -> Self {
        Self {
            identity: next_identity(),
            transfer: Transfer::Linear,
            primaries: Primaries::SRGB,
            luminance: Luminance {
                min: 0.,
                max: 80.,
                reference: 203.,
            },
            mastering: None,
            max_cll: None,
            max_fall: None,
            info: false,
        }
    }

    /// The same as another but for identity: two clients' sRGB descriptions don't redraw.
    fn same_colour(&self, other: &Self) -> bool {
        Self {
            identity: 0,
            info: false,
            ..self.clone()
        } == Self {
            identity: 0,
            info: false,
            ..other.clone()
        }
    }

    /// The highest light the content says it has, in nits.
    fn content_peak(&self) -> f64 {
        self.max_cll
            .or(self.mastering.map(|(_, _, max)| max))
            .unwrap_or(match self.transfer {
                Transfer::Pq => 10000.,
                _ => self.luminance.max,
            })
    }

    /// How a surface with it is drawn: `None` for what niri draws as is, gamma-2.2-like sRGB.
    ///
    /// BT.1886 (power 2.4) SDR video with sRGB primaries is drawn as is too, as desktops without
    /// color management always showed it: converting it costs the surface direct scanout (and an
    /// overlay plane) for a slight change in shadows, through the whole of every video (fork).
    pub fn params(&self) -> Option<ColorParams> {
        let plain_curve = matches!(self.transfer, Transfer::Srgb)
            || matches!(self.transfer, Transfer::Power(g) if (g - 2.2).abs() < 0.01)
            || matches!(self.transfer, Transfer::Power(g) if (g - 2.4).abs() < 0.01);
        let scale = match self.transfer {
            Transfer::Pq => 10000. / self.luminance.reference,
            _ => self.luminance.max / self.luminance.reference,
        };
        if plain_curve && self.primaries.close_to(&Primaries::SRGB) && (scale - 1.).abs() < 0.01 {
            return None;
        }

        let (tf, power) = match self.transfer {
            Transfer::Power(gamma) => (0., gamma),
            Transfer::Srgb => (1., 2.4),
            Transfer::Pq => (2., 1.),
            Transfer::Linear => (3., 1.),
        };
        let m = self.primaries.bt709_matrix();
        Some(ColorParams {
            tf,
            power: power as f32,
            matrix: [
                m[0][0] as f32,
                m[0][1] as f32,
                m[0][2] as f32,
                m[1][0] as f32,
                m[1][1] as f32,
                m[1][2] as f32,
                m[2][0] as f32,
                m[2][1] as f32,
                m[2][2] as f32,
            ],
            scale: scale as f32,
            content_peak: (self.content_peak() / self.luminance.reference) as f32,
        })
    }
}

/// What the shader needs to bring a surface into the composition: its transfer function (0
/// power, 1 sRGB, 2 PQ, 3 linear) and power, its primaries to BT.709 (row-major), how much
/// relative light 1.0 of its decoded signal is (1.0 being its reference white), and its peak in
/// the same units, for tone mapping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorParams {
    pub tf: f32,
    pub power: f32,
    pub matrix: [f32; 9],
    pub scale: f32,
    pub content_peak: f32,
}

/// A surface's description, double-buffered with its commits.
#[derive(Debug, Clone, Default)]
pub struct ColorCachedState {
    pub description: Option<Arc<ImageDescription>>,
}

impl Cacheable for ColorCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        self.clone()
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// How a surface is drawn, from the description it has now.
pub fn color_params(states: &SurfaceData) -> Option<ColorParams> {
    states
        .cached_state
        .get::<ColorCachedState>()
        .current()
        .description
        .as_ref()
        .and_then(|description| description.params())
}

pub struct ColorManagementState {
    /// Outputs' descriptions, by output name; an output that isn't here is sRGB.
    outputs: HashMap<String, Arc<ImageDescription>>,
    srgb: Arc<ImageDescription>,
    output_objects: Vec<(WpColorManagementOutputV1, String)>,
    feedbacks: Vec<WpColorManagementSurfaceFeedbackV1>,
    /// Information asked for, sent after the request that made it: its last event, `done`,
    /// destroys it, and destroyed inside that request the backend writes into freed memory.
    pending_information: Vec<(WpImageDescriptionInfoV1, Arc<ImageDescription>)>,
}

pub struct OutputObjectData(String);
pub struct SurfaceObjectData(Mutex<Weak<WlSurface>>);
pub struct FeedbackData(Mutex<Weak<WlSurface>>);
#[derive(Default)]
pub struct ParamsData(Mutex<Params>);
pub struct DescriptionData(Option<Arc<ImageDescription>>);

/// A surface's colour object, if it has one.
#[derive(Default)]
struct SurfaceColorData(Mutex<Option<WpColorManagementSurfaceV1>>);

#[derive(Default)]
struct Params {
    transfer: Option<Transfer>,
    primaries: Option<Primaries>,
    luminance: Option<Luminance>,
    mastering_primaries: Option<Primaries>,
    mastering_luminance: Option<(f64, f64)>,
    max_cll: Option<f64>,
    max_fall: Option<f64>,
}

pub trait ColorManagementHandler {
    fn color_management_state(&mut self) -> &mut ColorManagementState;
    /// The output a wl_output resource stands for.
    fn output_name(&self, output: &Output) -> String {
        output.name()
    }
}

impl ColorManagementState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<WpColorManagerV1, ()>,
        D: Dispatch<WpColorManagerV1, ()>,
        D: 'static,
    {
        display.create_global::<D, WpColorManagerV1, _>(VERSION, ());
        Self {
            outputs: HashMap::new(),
            srgb: Arc::new(ImageDescription::srgb()),
            output_objects: Vec::new(),
            feedbacks: Vec::new(),
            pending_information: Vec::new(),
        }
    }

    /// Sends the information asked for since the last call; niri calls it after dispatching
    /// clients' requests, before flushing them.
    pub fn send_pending_information(&mut self) {
        for (info, description) in self.pending_information.drain(..) {
            if info.is_alive() {
                send_information(&info, &description);
            }
        }
    }

    fn output_description(&self, name: &str) -> Arc<ImageDescription> {
        self.outputs
            .get(name)
            .cloned()
            .unwrap_or_else(|| self.srgb.clone())
    }

    /// What surfaces are told to draw in: the HDR output's description when there is one,
    /// since that's where HDR content gains, else sRGB.
    fn preferred(&self) -> Arc<ImageDescription> {
        let mut hdr: Vec<_> = self.outputs.iter().collect();
        hdr.sort_by(|a, b| a.0.cmp(b.0));
        hdr.first()
            .map(|(_, description)| (*description).clone())
            .unwrap_or_else(|| self.srgb.clone())
    }

    /// Sets which outputs are in HDR mode, with (SDR white, black, peak) in nits each, and tells
    /// clients when that changes.
    pub fn set_hdr_outputs(&mut self, hdr: impl IntoIterator<Item = (String, f64, f64, f64)>) {
        let before = self.preferred();
        let mut changed_outputs = Vec::new();

        let mut outputs = HashMap::new();
        for (name, sdr_white, min, peak) in hdr {
            let fresh = ImageDescription::hdr_output(sdr_white, min, peak);
            let description = match self.outputs.get(&name) {
                Some(old) if old.same_colour(&fresh) => old.clone(),
                _ => {
                    changed_outputs.push(name.clone());
                    Arc::new(fresh)
                }
            };
            outputs.insert(name, description);
        }
        changed_outputs.extend(
            self.outputs
                .keys()
                .filter(|name| !outputs.contains_key(*name))
                .cloned(),
        );
        self.outputs = outputs;

        self.output_objects.retain(|(object, _)| object.is_alive());
        for (object, name) in &self.output_objects {
            if changed_outputs.contains(name) {
                object.image_description_changed();
            }
        }

        let after = self.preferred();
        if !Arc::ptr_eq(&before, &after) {
            self.feedbacks.retain(|feedback| feedback.is_alive());
            for feedback in &self.feedbacks {
                send_preferred_changed(feedback, after.identity);
            }
        }
    }
}

fn send_preferred_changed(feedback: &WpColorManagementSurfaceFeedbackV1, identity: u64) {
    if feedback.version() >= 2 {
        feedback.preferred_changed2((identity >> 32) as u32, identity as u32);
    } else {
        feedback.preferred_changed(identity as u32);
    }
}

fn send_ready(description: &WpImageDescriptionV1, identity: u64) {
    if description.version() >= 2 {
        description.ready2((identity >> 32) as u32, identity as u32);
    } else {
        description.ready(identity as u32);
    }
}

impl<D> GlobalDispatch<WpColorManagerV1, (), D> for ColorManagementState
where
    D: GlobalDispatch<WpColorManagerV1, ()> + Dispatch<WpColorManagerV1, ()> + 'static,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        manager: New<WpColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(manager, ());

        manager.supported_intent(wp_color_manager_v1::RenderIntent::Perceptual);
        for feature in [
            wp_color_manager_v1::Feature::Parametric,
            wp_color_manager_v1::Feature::SetPrimaries,
            wp_color_manager_v1::Feature::SetTfPower,
            wp_color_manager_v1::Feature::SetLuminances,
            wp_color_manager_v1::Feature::SetMasteringDisplayPrimaries,
            wp_color_manager_v1::Feature::ExtendedTargetVolume,
            wp_color_manager_v1::Feature::WindowsScrgb,
        ] {
            manager.supported_feature(feature);
        }
        for tf in [
            Tf::Bt1886,
            Tf::Gamma22,
            Tf::Gamma28,
            Tf::ExtLinear,
            Tf::St2084Pq,
            Tf::Srgb,
        ] {
            manager.supported_tf_named(tf);
        }
        if manager.version() >= 2 {
            manager.supported_tf_named(Tf::CompoundPower24);
        }
        for primaries in [
            NamedPrimaries::Srgb,
            NamedPrimaries::PalM,
            NamedPrimaries::Pal,
            NamedPrimaries::Ntsc,
            NamedPrimaries::GenericFilm,
            NamedPrimaries::Bt2020,
            NamedPrimaries::DciP3,
            NamedPrimaries::DisplayP3,
            NamedPrimaries::AdobeRgb,
        ] {
            manager.supported_primaries_named(primaries);
        }
        manager.done();
    }
}

/// Makes an image description object for `description`, ready at once; `None` fails it as
/// unsupported.
fn init_description<D>(
    data_init: &mut DataInit<'_, D>,
    id: New<WpImageDescriptionV1>,
    description: Option<Arc<ImageDescription>>,
) where
    D: Dispatch<WpImageDescriptionV1, DescriptionData> + 'static,
{
    let identity = description.as_ref().map(|d| d.identity);
    let object = data_init.init(id, DescriptionData(description));
    match identity {
        Some(identity) => send_ready(&object, identity),
        None => object.failed(
            wp_image_description_v1::Cause::Unsupported,
            "this description isn't supported".into(),
        ),
    }
}

impl<D> Dispatch<WpColorManagerV1, (), D> for ColorManagementState
where
    D: Dispatch<WpColorManagerV1, ()>
        + Dispatch<WpColorManagementOutputV1, OutputObjectData>
        + Dispatch<WpColorManagementSurfaceV1, SurfaceObjectData>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, FeedbackData>
        + Dispatch<WpImageDescriptionCreatorParamsV1, ParamsData>
        + Dispatch<WpImageDescriptionCreatorIccV1, ()>
        + Dispatch<WpImageDescriptionV1, DescriptionData>
        + ColorManagementHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &WpColorManagerV1,
        request: wp_color_manager_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use wp_color_manager_v1::Request;
        match request {
            Request::GetOutput { id, output } => {
                let name = Output::from_resource(&output)
                    .map(|output| state.output_name(&output))
                    .unwrap_or_default();
                let object = data_init.init(id, OutputObjectData(name.clone()));
                state
                    .color_management_state()
                    .output_objects
                    .push((object, name));
            }
            Request::GetSurface { id, surface } => {
                let attached = with_states(&surface, |states| {
                    let data = states
                        .data_map
                        .get_or_insert_threadsafe(SurfaceColorData::default);
                    data.0
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|object| object.is_alive())
                });
                // a new object is always made, even one met with an error: the backend tears
                // down what a request created, and one never made crashes it
                let object = data_init.init(id, SurfaceObjectData(Mutex::new(surface.downgrade())));
                if attached {
                    manager.post_error(
                        wp_color_manager_v1::Error::SurfaceExists,
                        "wl_surface already has a color management surface",
                    );
                    return;
                }
                with_states(&surface, |states| {
                    if let Some(data) = states.data_map.get::<SurfaceColorData>() {
                        *data.0.lock().unwrap() = Some(object);
                    }
                });
            }
            Request::GetSurfaceFeedback { id, surface } => {
                let object = data_init.init(id, FeedbackData(Mutex::new(surface.downgrade())));
                state.color_management_state().feedbacks.push(object);
            }
            Request::CreateIccCreator { obj } => {
                data_init.init(obj, ());
                manager.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "ICC profiles aren't supported",
                );
            }
            Request::CreateParametricCreator { obj } => {
                data_init.init(obj, ParamsData::default());
            }
            Request::CreateWindowsScrgb { image_description } => {
                init_description(
                    data_init,
                    image_description,
                    Some(Arc::new(ImageDescription::windows_scrgb())),
                );
            }
            Request::GetImageDescription {
                image_description, ..
            } => {
                // niri hands out no references
                init_description(data_init, image_description, None);
            }
            Request::Destroy => (),
            _ => (),
        }
    }
}

impl<D> Dispatch<WpColorManagementOutputV1, OutputObjectData, D> for ColorManagementState
where
    D: Dispatch<WpColorManagementOutputV1, OutputObjectData>
        + Dispatch<WpImageDescriptionV1, DescriptionData>
        + ColorManagementHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _object: &WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        data: &OutputObjectData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_output_v1::Request::GetImageDescription { image_description } => {
                let description = state.color_management_state().output_description(&data.0);
                init_description(data_init, image_description, Some(description));
            }
            wp_color_management_output_v1::Request::Destroy => (),
            _ => (),
        }
    }
}

impl<D> Dispatch<WpColorManagementSurfaceV1, SurfaceObjectData, D> for ColorManagementState
where
    D: Dispatch<WpColorManagementSurfaceV1, SurfaceObjectData> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        object: &WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        data: &SurfaceObjectData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let surface = data.0.lock().unwrap().upgrade().ok();
        let description = match request {
            wp_color_management_surface_v1::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                let Some(surface) = &surface else {
                    object.post_error(
                        wp_color_management_surface_v1::Error::Inert,
                        "the wl_surface is gone",
                    );
                    return;
                };
                let _ = surface;
                if !matches!(
                    render_intent,
                    WEnum::Value(wp_color_manager_v1::RenderIntent::Perceptual)
                ) {
                    object.post_error(
                        wp_color_management_surface_v1::Error::RenderIntent,
                        "only the perceptual render intent is supported",
                    );
                    return;
                }
                let Some(description) = image_description
                    .data::<DescriptionData>()
                    .and_then(|data| data.0.clone())
                else {
                    object.post_error(
                        wp_color_management_surface_v1::Error::ImageDescription,
                        "the image description isn't ready",
                    );
                    return;
                };
                Some(description)
            }
            wp_color_management_surface_v1::Request::UnsetImageDescription => None,
            wp_color_management_surface_v1::Request::Destroy => {
                if let Some(surface) = &surface {
                    with_states(surface, |states| {
                        if let Some(data) = states.data_map.get::<SurfaceColorData>() {
                            *data.0.lock().unwrap() = None;
                        }
                    });
                }
                None
            }
            _ => return,
        };

        if let Some(surface) = surface {
            with_states(&surface, |states| {
                states
                    .cached_state
                    .get::<ColorCachedState>()
                    .pending()
                    .description = description;
            });
        }
    }
}

impl<D> Dispatch<WpColorManagementSurfaceFeedbackV1, FeedbackData, D> for ColorManagementState
where
    D: Dispatch<WpColorManagementSurfaceFeedbackV1, FeedbackData>
        + Dispatch<WpImageDescriptionV1, DescriptionData>
        + ColorManagementHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        object: &WpColorManagementSurfaceFeedbackV1,
        request: wp_color_management_surface_feedback_v1::Request,
        data: &FeedbackData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use wp_color_management_surface_feedback_v1::Request;
        let (id, parametric) = match request {
            Request::GetPreferred { image_description } => (image_description, false),
            Request::GetPreferredParametric { image_description } => (image_description, true),
            Request::Destroy => return,
            _ => return,
        };
        let _ = parametric;
        if data.0.lock().unwrap().upgrade().is_err() {
            init_description(data_init, id, None);
            object.post_error(
                wp_color_management_surface_feedback_v1::Error::Inert,
                "the wl_surface is gone",
            );
            return;
        }
        let preferred = state.color_management_state().preferred();
        init_description(data_init, id, Some(preferred));
    }
}

impl<D> Dispatch<WpImageDescriptionCreatorParamsV1, ParamsData, D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionCreatorParamsV1, ParamsData>
        + Dispatch<WpImageDescriptionV1, DescriptionData>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        object: &WpImageDescriptionCreatorParamsV1,
        request: wp_image_description_creator_params_v1::Request,
        data: &ParamsData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use wp_image_description_creator_params_v1::{Error, Request};
        let mut params = data.0.lock().unwrap();
        let already = |object: &WpImageDescriptionCreatorParamsV1, what: &str| {
            object.post_error(Error::AlreadySet, format!("{what} is already set"));
        };
        let xy = |v: i32| f64::from(v) / 1_000_000.;
        let primaries = |r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y| {
            Primaries([
                [xy(r_x), xy(r_y)],
                [xy(g_x), xy(g_y)],
                [xy(b_x), xy(b_y)],
                [xy(w_x), xy(w_y)],
            ])
        };

        match request {
            Request::SetTfNamed { tf } => {
                if params.transfer.is_some() {
                    return already(object, "the transfer function");
                }
                match tf.into_result().ok().and_then(Transfer::named) {
                    Some(transfer) => params.transfer = Some(transfer),
                    None => object.post_error(Error::InvalidTf, "unsupported transfer function"),
                }
            }
            Request::SetTfPower { eexp } => {
                if params.transfer.is_some() {
                    return already(object, "the transfer function");
                }
                let exponent = f64::from(eexp) / 10000.;
                if !(1.0..=10.0).contains(&exponent) {
                    object.post_error(Error::InvalidTf, "the exponent must be from 1 to 10");
                    return;
                }
                params.transfer = Some(Transfer::Power(exponent));
            }
            Request::SetPrimariesNamed { primaries: named } => {
                if params.primaries.is_some() {
                    return already(object, "the primaries");
                }
                match named.into_result().ok().and_then(Primaries::named) {
                    Some(named) => params.primaries = Some(named),
                    None => {
                        object.post_error(Error::InvalidPrimariesNamed, "unsupported primaries")
                    }
                }
            }
            Request::SetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                if params.primaries.is_some() {
                    return already(object, "the primaries");
                }
                params.primaries = Some(primaries(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y));
            }
            Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                if params.luminance.is_some() {
                    return already(object, "the luminances");
                }
                let min = f64::from(min_lum) / 10000.;
                let (max, reference) = (f64::from(max_lum), f64::from(reference_lum));
                if max <= min || reference <= min {
                    object.post_error(Error::InvalidLuminance, "max and reference must exceed min");
                    return;
                }
                params.luminance = Some(Luminance {
                    min,
                    max,
                    reference,
                });
            }
            Request::SetMasteringDisplayPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                if params.mastering_primaries.is_some() {
                    return already(object, "the mastering primaries");
                }
                params.mastering_primaries =
                    Some(primaries(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y));
            }
            Request::SetMasteringLuminance { min_lum, max_lum } => {
                if params.mastering_luminance.is_some() {
                    return already(object, "the mastering luminance");
                }
                let min = f64::from(min_lum) / 10000.;
                let max = f64::from(max_lum);
                if max <= min {
                    object.post_error(Error::InvalidLuminance, "max must exceed min");
                    return;
                }
                params.mastering_luminance = Some((min, max));
            }
            Request::SetMaxCll { max_cll } => {
                if params.max_cll.is_some() {
                    return already(object, "max_cll");
                }
                params.max_cll = Some(f64::from(max_cll));
            }
            Request::SetMaxFall { max_fall } => {
                if params.max_fall.is_some() {
                    return already(object, "max_fall");
                }
                params.max_fall = Some(f64::from(max_fall));
            }
            Request::Create { image_description } => {
                let (Some(transfer), Some(primaries)) = (params.transfer, params.primaries) else {
                    init_description(data_init, image_description, None);
                    object.post_error(
                        Error::IncompleteSet,
                        "a transfer function and primaries are needed",
                    );
                    return;
                };
                if let (Some(cll), Some(fall)) = (params.max_cll, params.max_fall) {
                    if fall > cll {
                        init_description(data_init, image_description, None);
                        object.post_error(Error::InvalidLuminance, "max_fall exceeds max_cll");
                        return;
                    }
                }
                let mut luminance = params
                    .luminance
                    .unwrap_or_else(|| transfer.default_luminance());
                // PQ's range is fixed: 10 000 nits over its black
                if transfer == Transfer::Pq {
                    luminance.max = luminance.min + 10000.;
                }
                let mastering = params
                    .mastering_luminance
                    .map(|(min, max)| (params.mastering_primaries.unwrap_or(primaries), min, max));
                let description = ImageDescription {
                    identity: next_identity(),
                    transfer,
                    primaries,
                    luminance,
                    mastering,
                    max_cll: params.max_cll,
                    max_fall: params.max_fall,
                    info: false,
                };
                init_description(data_init, image_description, Some(Arc::new(description)));
            }
            _ => (),
        }
    }
}

impl<D> Dispatch<WpImageDescriptionV1, DescriptionData, D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionV1, DescriptionData>
        + Dispatch<WpImageDescriptionInfoV1, ()>
        + ColorManagementHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        object: &WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        data: &DescriptionData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        let wp_image_description_v1::Request::GetInformation { information } = request else {
            return;
        };
        let info = data_init.init(information, ());
        let Some(description) = &data.0 else {
            object.post_error(
                wp_image_description_v1::Error::NotReady,
                "the image description failed",
            );
            return;
        };
        if !description.info {
            object.post_error(
                wp_image_description_v1::Error::NoInformation,
                "this image description gives no information",
            );
            return;
        }
        state
            .color_management_state()
            .pending_information
            .push((info, description.clone()));
    }
}

fn send_information(info: &WpImageDescriptionInfoV1, description: &ImageDescription) {
    let xy = |v: f64| (v * 1_000_000.).round() as i32;
    let [[r_x, r_y], [g_x, g_y], [b_x, b_y], [w_x, w_y]] = description.primaries.0;
    info.primaries(
        xy(r_x),
        xy(r_y),
        xy(g_x),
        xy(g_y),
        xy(b_x),
        xy(b_y),
        xy(w_x),
        xy(w_y),
    );
    if description.primaries == Primaries::SRGB {
        info.primaries_named(NamedPrimaries::Srgb);
    } else if description.primaries == Primaries::BT2020 {
        info.primaries_named(NamedPrimaries::Bt2020);
    }
    match description.transfer {
        Transfer::Pq => info.tf_named(Tf::St2084Pq),
        Transfer::Linear => info.tf_named(Tf::ExtLinear),
        Transfer::Srgb => info.tf_named(Tf::Srgb),
        Transfer::Power(gamma) if (gamma - 2.2).abs() < 1e-6 => info.tf_named(Tf::Gamma22),
        Transfer::Power(gamma) => info.tf_power((gamma * 10000.).round() as u32),
    }
    let lum = description.luminance;
    info.luminances(
        (lum.min * 10000.).round() as u32,
        lum.max.round() as u32,
        lum.reference.round() as u32,
    );
    let (target, min, max) =
        description
            .mastering
            .unwrap_or((description.primaries, lum.min, lum.max));
    let [[r_x, r_y], [g_x, g_y], [b_x, b_y], [w_x, w_y]] = target.0;
    info.target_primaries(
        xy(r_x),
        xy(r_y),
        xy(g_x),
        xy(g_y),
        xy(b_x),
        xy(b_y),
        xy(w_x),
        xy(w_y),
    );
    info.target_luminance((min * 10000.).round() as u32, max.round() as u32);
    if let Some(cll) = description.max_cll {
        info.target_max_cll(cll.round() as u32);
    }
    if let Some(fall) = description.max_fall {
        info.target_max_fall(fall.round() as u32);
    }
    info.done();
}

impl<D> Dispatch<WpImageDescriptionInfoV1, (), D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionInfoV1, ()> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionInfoV1,
        _request: <WpImageDescriptionInfoV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }
}

// Never made: niri doesn't advertise ICC, and hands out no references.
impl<D> Dispatch<WpImageDescriptionCreatorIccV1, (), D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionCreatorIccV1, ()> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionCreatorIccV1,
        _request: <WpImageDescriptionCreatorIccV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }
}

impl<D> Dispatch<WpImageDescriptionReferenceV1, (), D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionReferenceV1, ()> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionReferenceV1,
        _request: <WpImageDescriptionReferenceV1 as Resource>::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }
}

#[macro_export]
macro_rules! delegate_color_management {
    ($ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_output_v1::WpColorManagementOutputV1: $crate::protocols::color_management::OutputObjectData
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_v1::WpColorManagementSurfaceV1: $crate::protocols::color_management::SurfaceObjectData
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1: $crate::protocols::color_management::FeedbackData
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1: $crate::protocols::color_management::ParamsData
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_v1::WpImageDescriptionV1: $crate::protocols::color_management::DescriptionData
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_reference_v1::WpImageDescriptionReferenceV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3], eps: f64) -> bool {
        a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(x, y)| (x - y).abs() < eps)
    }

    #[test]
    fn srgb_to_bt709_is_identity() {
        let identity = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
        assert!(close(&Primaries::SRGB.bt709_matrix(), &identity, 1e-9));
    }

    #[test]
    fn bt2020_to_bt709_matches_bt2087() {
        // ITU-R BT.2087's inverse, the well-known 2020 → 709 matrix
        let expected = [
            [1.6605, -0.5876, -0.0728],
            [-0.1246, 1.1329, -0.0083],
            [-0.0182, -0.1006, 1.1187],
        ];
        assert!(close(&Primaries::BT2020.bt709_matrix(), &expected, 2e-3));
    }

    #[test]
    fn plain_srgb_draws_as_is_and_pq_doesnt() {
        assert_eq!(ImageDescription::srgb().params(), None);

        let pq = ImageDescription {
            identity: 0,
            transfer: Transfer::Pq,
            primaries: Primaries::BT2020,
            luminance: Transfer::Pq.default_luminance(),
            mastering: Some((Primaries::BT2020, 0.005, 1000.)),
            max_cll: None,
            max_fall: None,
            info: false,
        };
        let params = pq.params().unwrap();
        assert_eq!(params.tf, 2.);
        // 10 000 nits over a 203 nit reference
        assert!((params.scale - 10000. / 203.).abs() < 1e-3);
        assert!((params.content_peak - 1000. / 203.).abs() < 1e-3);

        let scrgb = ImageDescription::windows_scrgb().params().unwrap();
        assert_eq!(scrgb.tf, 3.);
        assert!((scrgb.scale - 80. / 203.).abs() < 1e-4);
    }

    #[test]
    fn bt1886_sdr_video_draws_as_is_unless_wide_gamut() {
        let bt1886 = ImageDescription {
            identity: 0,
            transfer: Transfer::Power(2.4),
            primaries: Primaries::SRGB,
            luminance: Transfer::Power(2.4).default_luminance(),
            mastering: None,
            max_cll: None,
            max_fall: None,
            info: false,
        };
        assert_eq!(bt1886.params(), None);
        let wide = ImageDescription {
            primaries: Primaries::BT2020,
            ..bt1886
        };
        assert!(wide.params().is_some());
    }
}
