//! Bloom: a soft glow around bright things (fire, explosions, muzzle flashes, lasers), as in
//! future_rust_cop and nuclear_rust_strike.
//!
//! A game calls `glide::bloom` once its 3D views are drawn and before its HUD (so the HUD is
//! not bloomed), with the box the views fill. The picture there is copied (resolved with
//! MSAA); a first pass takes its bright part at half resolution (a soft threshold on the
//! brightness) where glide's glow mask says a glowing effect was drawn (blended, no depth
//! written: fire, explosions, shots, lights' glows; bright sky, sand or walls do not glow),
//! then a chain of smaller levels (13-tap filter down), back up (tent filter, each
//! level adding the one below), and the largest level is added to the picture inside the box at
//! low strength.
//!
//! No HDR target: the picture is 8 bits, so the threshold is below white. Setting
//! `BloomLevel` (0 off, 1 low, 2 medium, 3 high; `options::bloom`, the settings menu).

use super::advapi32;
use super::gl::compile;
use glow::HasContext;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

/// The levels' names, and their threshold and strength (the first is off).
pub const LEVEL_NAMES: [&str; 4] = ["Off", "Low", "Medium", "High"];
const LEVEL_LOOKS: [(f32, f32); 4] = [(1.0, 0.0), (0.72, 0.35), (0.6, 0.8), (0.5, 1.3)];
const DEFAULT_LEVEL: u8 = 2;

/// The level; 255 not read yet.
static LEVEL: AtomicU8 = AtomicU8::new(255);

pub fn level() -> usize {
    let l = LEVEL.load(Relaxed);
    if l != 255 {
        return l as usize;
    }
    let l = advapi32::value("", "BloomLevel")
        .and_then(|(_, d)| d.first().copied())
        .filter(|&v| (v as usize) < LEVEL_NAMES.len())
        .unwrap_or(DEFAULT_LEVEL);
    LEVEL.store(l, Relaxed);
    l as usize
}

/// Saved, and used from the next picture on.
pub fn set_level(l: usize) {
    let l = l.min(LEVEL_NAMES.len() - 1) as u32;
    LEVEL.store(l as u8, Relaxed);
    advapi32::set_value("", "BloomLevel", 4, &l.to_le_bytes());
}

pub fn enabled() -> bool {
    level() != 0
}

/// Levels below the half-resolution one.
const LEVELS: usize = 6;
/// The knee's width around the brightness where the glow begins (the level's threshold; its
/// strength: the glow's whole light when added, as a share of the bright part's).
const KNEE: f32 = 0.2;
/// Each larger level counts this much more going up: the glow reaches farther around a
/// bright thing instead of staying on it.
const SPREAD: f32 = 1.45;

/// One triangle over the whole target.
const VS: &str = r#"
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const PRE_FS: &str = r#"
uniform sampler2D picture;
uniform sampler2D mask;     // glide's glow mask: 1 where a glowing effect covers the pixel
uniform ivec2 size;
uniform float threshold;
uniform float knee;
out vec4 o;
float bright(vec3 c) {
    float b = max(c.r, max(c.g, c.b));
    float s = clamp(b - threshold + knee, 0.0, 2.0 * knee);
    s = s * s / (4.0 * knee + 1e-4);
    return max(s, b - threshold) / max(b, 1e-4);
}
void main() {
    // one half-resolution pixel: 2x2 full-resolution pixels
    ivec2 q = ivec2(gl_FragCoord.xy) * 2;
    vec3 sum = vec3(0.0);
    for (int i = 0; i < 4; i++) {
        ivec2 p = clamp(q + ivec2(i & 1, i >> 1), ivec2(0), size - 1);
        vec3 c = texelFetch(picture, p, 0).rgb;
        // weighted by 1 / (1 + brightness): one bright pixel does not flicker as a blob
        float w = 1.0 / (1.0 + max(c.r, max(c.g, c.b)));
        sum += c * bright(c) * w * texelFetch(mask, p, 0).r;
    }
    o = vec4(sum * 0.25 * 2.0, 1.0);
}
"#;

/// 13 taps (Jimenez 2014): the level above, filtered down to this one.
const DOWN_FS: &str = r#"
uniform sampler2D src;
uniform vec2 texel;     // the source's texel size
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy / vec2(textureSize(src, 0)) * 2.0;
    vec2 t = texel;
    vec3 a = texture(src, uv + t * vec2(-2.0, 2.0)).rgb;
    vec3 b = texture(src, uv + t * vec2(0.0, 2.0)).rgb;
    vec3 c = texture(src, uv + t * vec2(2.0, 2.0)).rgb;
    vec3 d = texture(src, uv + t * vec2(-2.0, 0.0)).rgb;
    vec3 e = texture(src, uv).rgb;
    vec3 f = texture(src, uv + t * vec2(2.0, 0.0)).rgb;
    vec3 g = texture(src, uv + t * vec2(-2.0, -2.0)).rgb;
    vec3 h = texture(src, uv + t * vec2(0.0, -2.0)).rgb;
    vec3 i = texture(src, uv + t * vec2(2.0, -2.0)).rgb;
    vec3 j = texture(src, uv + t * vec2(-1.0, 1.0)).rgb;
    vec3 k = texture(src, uv + t * vec2(1.0, 1.0)).rgb;
    vec3 l = texture(src, uv + t * vec2(-1.0, -1.0)).rgb;
    vec3 m = texture(src, uv + t * vec2(1.0, -1.0)).rgb;
    vec3 r = e * 0.125 + (a + c + g + i) * 0.03125 + (b + d + f + h) * 0.0625
        + (j + k + l + m) * 0.125;
    o = vec4(r, 1.0);
}
"#;

/// A 3x3 tent of the level below, added to this one (blending ONE, ONE); `strength` scales.
const UP_FS: &str = r#"
uniform sampler2D src;
uniform vec2 texel;     // the source's texel size
uniform vec2 dst_size;  // this level's size
uniform float strength;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy / dst_size;
    vec2 t = texel;
    vec3 r = texture(src, uv).rgb * 4.0;
    r += (texture(src, uv + vec2(t.x, 0.0)).rgb + texture(src, uv - vec2(t.x, 0.0)).rgb
        + texture(src, uv + vec2(0.0, t.y)).rgb + texture(src, uv - vec2(0.0, t.y)).rgb) * 2.0;
    r += texture(src, uv + t).rgb + texture(src, uv - t).rgb
        + texture(src, uv + vec2(t.x, -t.y)).rgb + texture(src, uv + vec2(-t.x, t.y)).rgb;
    o = vec4(r / 16.0 * strength, 1.0);
}
"#;

type Level = (glow::Texture, glow::Framebuffer, (i32, i32));

/// The passes' programs and textures.
pub struct Bloom {
    pre: glow::Program,
    down: glow::Program,
    up: glow::Program,
    vao: glow::VertexArray,
    /// for the target's size: the picture's copy, and the levels (half resolution first)
    targets: Option<((u32, u32), glow::Texture, glow::Framebuffer, Vec<Level>)>,
}

impl Bloom {
    pub fn new(gl: &glow::Context) -> Bloom {
        let vao = unsafe { gl.create_vertex_array().expect("GL vertex array") };
        Bloom {
            pre: compile(gl, VS, PRE_FS),
            down: compile(gl, VS, DOWN_FS),
            up: compile(gl, VS, UP_FS),
            vao,
            targets: None,
        }
    }

    fn drop_targets(&mut self, gl: &glow::Context) {
        if let Some((_, t, f, levels)) = self.targets.take() {
            unsafe {
                gl.delete_framebuffer(f);
                gl.delete_texture(t);
                for (t, f, _) in levels {
                    gl.delete_framebuffer(f);
                    gl.delete_texture(t);
                }
            }
        }
    }

    fn make_targets(&mut self, gl: &glow::Context, target: (u32, u32)) {
        if self.targets.as_ref().is_some_and(|c| c.0 == target) {
            return;
        }
        self.drop_targets(gl);
        unsafe {
            let make = |w: i32, h: i32, format: u32, kind: u32| {
                let t = super::gl::texture(gl, true);
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    format as i32,
                    w,
                    h,
                    0,
                    glow::RGBA,
                    kind,
                    glow::PixelUnpackData::Slice(None),
                );
                let f = gl.create_framebuffer().expect("GL framebuffer");
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(t),
                    0,
                );
                (t, f)
            };
            let (t, f) = make(
                target.0 as i32,
                target.1 as i32,
                glow::RGBA8,
                glow::UNSIGNED_BYTE,
            );
            let mut levels = Vec::new();
            let (mut w, mut h) = (target.0 as i32, target.1 as i32);
            for _ in 0..=LEVELS {
                w = (w / 2).max(1);
                h = (h / 2).max(1);
                let (lt, lf) = make(w, h, glow::RGBA16F, glow::HALF_FLOAT);
                levels.push((lt, lf, (w, h)));
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            self.targets = Some((target, t, f, levels));
        }
    }

    pub fn delete(&mut self, gl: &glow::Context) {
        self.drop_targets(gl);
        unsafe {
            for p in [self.pre, self.down, self.up] {
                super::gl::delete_program(gl, p);
            }
            gl.delete_vertex_array(self.vao);
        }
    }

    /// The glow over the picture in `out` (`target` its size) inside `clip` (x y w h, GL's:
    /// from the lower left), where `mask` (R8, the target's size) is set.
    pub fn run(
        &mut self,
        gl: &glow::Context,
        out: glow::Framebuffer,
        mask: glow::Texture,
        target: (u32, u32),
        clip: [i32; 4],
    ) {
        self.make_targets(gl, target);
        let Some((_, copy, copy_fbo, levels)) = &self.targets else {
            return;
        };
        let (w, h) = (target.0 as i32, target.1 as i32);
        let [x, y, cw, ch] = clip;
        let u = |p: glow::Program, n: &'static str| super::gl::uniform(gl, p, n);
        let (threshold, strength) = LEVEL_LOOKS[level()];
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
            gl.bind_vertex_array(Some(self.vao));
            // the box's part of the picture, resolved
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(out));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(*copy_fbo));
            gl.blit_framebuffer(
                x,
                y,
                x + cw,
                y + ch,
                x,
                y,
                x + cw,
                y + ch,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            // the bright part at half resolution, only inside the box
            let (l0, l0f, (w0, h0)) = levels[0];
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(l0f));
            gl.viewport(0, 0, w0, h0);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(x / 2, y / 2, cw / 2, ch / 2);
            gl.use_program(Some(self.pre));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(mask));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(*copy));
            gl.uniform_1_i32(u(self.pre, "picture").as_ref(), 0);
            gl.uniform_1_i32(u(self.pre, "mask").as_ref(), 1);
            gl.uniform_2_i32(u(self.pre, "size").as_ref(), w, h);
            gl.uniform_1_f32(u(self.pre, "threshold").as_ref(), threshold);
            gl.uniform_1_f32(u(self.pre, "knee").as_ref(), KNEE);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.disable(glow::SCISSOR_TEST);
            // down the chain
            gl.use_program(Some(self.down));
            gl.uniform_1_i32(u(self.down, "src").as_ref(), 0);
            for i in 1..levels.len() {
                let (src, _, (sw, sh)) = levels[i - 1];
                let (_, f, (dw, dh)) = levels[i];
                gl.bind_texture(glow::TEXTURE_2D, Some(src));
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                gl.viewport(0, 0, dw, dh);
                gl.uniform_2_f32(
                    u(self.down, "texel").as_ref(),
                    1.0 / sw as f32,
                    1.0 / sh as f32,
                );
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
            // back up, each level adding the one below
            gl.use_program(Some(self.up));
            gl.uniform_1_i32(u(self.up, "src").as_ref(), 0);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE);
            for i in (0..levels.len() - 1).rev() {
                let (src, _, (sw, sh)) = levels[i + 1];
                let (_, f, (dw, dh)) = levels[i];
                gl.bind_texture(glow::TEXTURE_2D, Some(src));
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
                gl.viewport(0, 0, dw, dh);
                gl.uniform_2_f32(
                    u(self.up, "texel").as_ref(),
                    1.0 / sw as f32,
                    1.0 / sh as f32,
                );
                gl.uniform_2_f32(u(self.up, "dst_size").as_ref(), dw as f32, dh as f32);
                gl.uniform_1_f32(u(self.up, "strength").as_ref(), SPREAD);
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
            // onto the picture, inside the box
            gl.bind_texture(glow::TEXTURE_2D, Some(l0));
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(out));
            gl.viewport(0, 0, w, h);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(x, y, cw, ch);
            gl.uniform_2_f32(
                u(self.up, "texel").as_ref(),
                1.0 / w0 as f32,
                1.0 / h0 as f32,
            );
            gl.uniform_2_f32(u(self.up, "dst_size").as_ref(), w as f32, h as f32);
            // the levels' sum (weights SPREAD^k) brought back to one, times the strength
            let sum: f32 = (0..=LEVELS).map(|k| SPREAD.powi(k as i32)).sum();
            let k = (LEVELS + 1) as f32 / sum;
            gl.uniform_1_f32(u(self.up, "strength").as_ref(), strength * k);
            // the picture's alpha left as it is
            gl.color_mask(true, true, true, false);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.color_mask(true, true, true, true);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.bind_texture(glow::TEXTURE_2D, None);
            // the mask is drawn into again: not left bound
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }
}
