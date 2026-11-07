//! The light travelling around the border while there's nothing to select: two soft lights on
//! opposite sides, drawn by a small shader at an effect slot that covers the panel.
//!
//! The shader measures how far each pixel is inside the panel's edge (with Rune's own
//! rounded-rectangle distance, continuous corners and all) and how far round the border it
//! is, and lights it by both with smooth falloffs: a bright hairline on the edge with a faint
//! glow inside it, fading along the border either side of each light.

use rune::{
    El,
    compose::{Compositor, DeviceKey, Frame},
    render::{CLIP_WGSL, EffectSite},
};

use crate::Launcher;

/// The effect slot's tag.
const MARK: u32 = 1;
/// Seconds for each light to go once around: slow enough to read as an accent, not a chase.
const LAP: f64 = 18.0;
/// A pause longer than this means the lights are showing afresh: they start from the corners.
const RESTART: f64 = 0.3;

const UNIFORM_SIZE: u32 = 96;

const SHADER: &str = r"
struct U {
    rect: vec4f,
    clip: vec4f,
    fade: vec4<u32>,
    frame: vec4f,
    color: vec4f,
    // phase (laps), device pixels per point, corner radius in device pixels, sRGB output
    params: vec4f,
}
var<immediate> u: U;

struct V { @builtin(position) pos: vec4f }

@vertex fn vs(@builtin(vertex_index) vi: u32) -> V {
    let c = vec2f(f32(vi & 1u), f32(vi >> 1u));
    let l = (u.rect.xy + c * u.rect.zw - u.frame.xy) / u.frame.zw;
    return V(vec4f(l.x * 2.0 - 1.0, 1.0 - l.y * 2.0, 0.0, 1.0));
}

const PI: f32 = 3.14159265;
// How the light looks, in points: how far each light reaches along the border (one standard
// deviation of its falloff), the hairline's width and strength, and the inner glow's reach
// and strength. Long and faint, so it reads as an accent on the edge.
const REACH: f32 = 130.0;
const LINE_WIDTH: f32 = 1.1;
const LINE: f32 = 0.65;
const GLOW_REACH: f32 = 7.0;
const GLOW: f32 = 0.12;

// How far round the border `p` (from the centre, y down) is, clockwise from where the top
// edge leaves the top-left corner, measured on the nearest straight edge or corner arc.
fn around(p: vec2f, half: vec2f, r: f32) -> f32 {
    let w = half.x - r;
    let h = half.y - r;
    let arc = 0.5 * PI * r;
    let q = clamp(p, -vec2f(w, h), vec2f(w, h));
    var d = p - q;
    if (d.x == 0.0 && d.y == 0.0) {
        // Deep inside: take the nearer edge.
        if (h - abs(p.y) < w - abs(p.x)) { d.y = select(-1.0, 1.0, p.y > 0.0); }
        else { d.x = select(-1.0, 1.0, p.x > 0.0); }
    }
    let top = 2.0 * w;
    let side = 2.0 * h;
    if (d.y < 0.0 && d.x == 0.0) { return p.x + w; }
    if (d.x > 0.0 && d.y < 0.0) { return top + r * atan2(d.x, -d.y); }
    if (d.x > 0.0 && d.y == 0.0) { return top + arc + p.y + h; }
    if (d.x > 0.0 && d.y > 0.0) { return top + arc + side + r * atan2(d.y, d.x); }
    if (d.y > 0.0 && d.x == 0.0) { return top + 2.0 * arc + side + w - p.x; }
    if (d.x < 0.0 && d.y > 0.0) { return 2.0 * top + 2.0 * arc + side + r * atan2(-d.x, d.y); }
    if (d.x < 0.0 && d.y == 0.0) { return 2.0 * top + 3.0 * arc + side + h - p.y; }
    return 2.0 * top + 3.0 * arc + 2.0 * side + r * atan2(-d.y, -d.x);
}

fn enc(x: vec3f) -> vec3f {
    return select(1.055 * pow(x, vec3f(1.0 / 2.4)) - 0.055, x * 12.92, x <= vec3f(0.0031308));
}

@fragment fn fs(v: V) -> @location(0) vec4f {
    let wp = v.pos.xy + u.frame.xy;
    let half = u.rect.zw * 0.5;
    let p = wp - u.rect.xy - half;
    let s = u.params.y;
    let r = min(u.params.z, min(half.x, half.y));

    // Across the edge: a bright hairline on it and a faint glow reaching in.
    let inside = max(-sd_rounded(p, half, vec4f(-r)), 0.0);
    let line = LINE * exp(-pow(inside / (LINE_WIDTH * s), 2.0));
    let glow = GLOW * exp(-inside / (GLOW_REACH * s));

    // Along the edge: two lights half a lap apart, starting on the top-right and
    // bottom-left corners.
    let w = half.x - r;
    let h = half.y - r;
    let lap = 4.0 * w + 4.0 * h + 2.0 * PI * r;
    let corner = (2.0 * w + 0.25 * PI * r) / lap;
    let at = around(p, half, r);
    var along = 0.0;
    for (var i = 0; i < 2; i++) {
        let centre = fract(u.params.x + corner + 0.5 * f32(i)) * lap;
        var gap = abs(at - centre);
        gap = min(gap, lap - gap);
        along += exp(-pow(gap / (REACH * s), 2.0));
    }

    let a = u.color.a * (line + glow) * along * clip_alpha(wp, u.clip, u.fade);
    var rgb = u.color.rgb;
    if (u.params.w > 0.5) { rgb = enc(rgb); }
    return vec4f(rgb * a, a);
}
";

/// The slot for the view: it covers the panel, and `data` tells the compositor the light's
/// strength, whether to hold still (Reduce Motion) and the corner radius.
pub fn slot(alpha: f32, reduced: bool, radius: f32) -> El {
    rune::effect(MARK).effect_data([alpha, if reduced { 1.0 } else { 0.0 }, radius, 0.0])
}

struct Gpu {
    device: DeviceKey,
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
}

impl Gpu {
    fn new(device: &wgpu::Device, key: DeviceKey, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("border light"),
            source: wgpu::ShaderSource::Wgsl(format!("{CLIP_WGSL}\n{SHADER}").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("border light"),
            bind_group_layouts: &[],
            immediate_size: UNIFORM_SIZE,
        });

        let mut gpu = Self {
            device: key,
            shader,
            layout,
            pipelines: Vec::new(),
        };

        // The first frame is the hidden pose. Prepare the window and Rune's isolated-layer
        // formats here so fading the panel and the light uses cached pipelines.
        gpu.prepare_pipeline(device, format);
        gpu.prepare_pipeline(device, wgpu::TextureFormat::Rgba16Float);

        gpu
    }

    /// One pipeline per target format: an isolated layer (the slot fading in or out) hands
    /// the hook a different one from the window's.
    fn prepare_pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.pipelines.iter().any(|(f, _)| *f == format) {
            return;
        }

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("border light"),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.push((format, pipeline));
    }
}

#[derive(Default)]
pub struct BorderLight {
    gpu: Option<Gpu>,
    /// When the lights started their way round, and when they were last drawn.
    started: f64,
    drawn: Option<f64>,
    moving: bool,
}

impl BorderLight {
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, site: &EffectSite<'_>, phase: f32) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };

        let i = site.instance;
        let [alpha, _, radius, _] = site.data;

        let mut uniform = [0u32; 24];
        let mut put = |at: usize, values: [f32; 4]| {
            for (k, value) in values.into_iter().enumerate() {
                uniform[at + k] = value.to_bits();
            }
        };
        put(0, i.rect);
        put(4, i.clip);
        put(
            12,
            [
                site.origin[0],
                site.origin[1],
                site.size[0] as f32,
                site.size[1] as f32,
            ],
        );
        put(16, [1.0, 1.0, 1.0, alpha]);
        put(
            20,
            [
                phase,
                site.scale,
                radius * site.scale,
                if site.srgb_blend { 1.0 } else { 0.0 },
            ],
        );
        uniform[8..12].copy_from_slice(&i.fade);
        let bytes = uniform.map(u32::to_le_bytes);

        let pipeline = &gpu
            .pipelines
            .iter()
            .find(|(format, _)| *format == site.format)
            .expect("border light pipeline prepared before drawing")
            .1;

        pass.set_pipeline(pipeline);
        pass.set_immediates(0, bytes.as_flattened());
        pass.draw(0..4, 0..1);
    }
}

impl Compositor<Launcher> for BorderLight {
    fn features(&self) -> wgpu::Features {
        // Metal supports 4 KB of inline data, including both shader stages.
        wgpu::Features::IMMEDIATES
    }

    fn animating(&self) -> bool {
        self.moving
    }

    fn render(&mut self, f: &mut Frame<'_, Launcher>) {
        let key = f.device_key();
        if self.gpu.as_ref().is_none_or(|gpu| gpu.device != key) {
            self.gpu = Some(Gpu::new(f.device, key, f.format));
        } else if let Some(gpu) = self.gpu.as_mut() {
            gpu.prepare_pipeline(f.device, f.format);
        }

        let now = f.rt.t;
        if self.drawn.is_none_or(|drawn| now - drawn > RESTART) {
            self.started = now;
        }
        let laps = ((now - self.started) / LAP) as f32;

        let (device, queue) = (f.device, f.queue);
        let clear = f.rt.clear_color();
        f.ui.prepare(device, queue, &mut f.rt.text.atlas, &f.rt.instances, f.size);

        let mut drew = None;
        f.ui.draw_with(f.encoder, f.target, clear, &mut |pass, site| {
            if site.mark != MARK {
                return;
            }

            let still = site.data[1] > 0.5;
            drew = Some(still);
            self.draw(pass, site, if still { 0.0 } else { laps });
        });

        self.drawn = drew.map(|_| now);
        self.moving = drew == Some(false);
    }
}
