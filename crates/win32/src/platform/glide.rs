//! The game's 3dfx Glide 2.x renderer on OpenGL. The game loads glide2x.dll itself and takes
//! its functions with GetProcAddress (`proc_address`): every name gets a thunk, the decorated
//! name (`_grDrawTriangle@12`) gives the stdcall argument size.
//!
//! The game draws its depth-sorted triangles with Glide's colour and alpha combine units, alpha
//! blending and a chroma key (black texels are not drawn), textures 256x256 ARGB1555 in TMU
//! memory, no depth buffer. We draw into a texture as tall as the window or the resolution
//! chosen in Preferences (`display.rs`; high resolution:
//! the Glide screen of `grSstWinOpen` only gives the coordinates; the game opens 1280x1024
//! for its 1280x960 mode and stretches its 4:3 picture to it) and show it at `grBufferSwap`. Vertices are in Glide screen pixels, or in the space
//! the display list walker sets (`set_space`, see `walker.rs`); clip windows are always in
//! screen pixels. Widescreen (`wide.rs`): the texture takes the window's aspect, the 3D
//! picture fills it and the HUD is drawn into 4:3 parts of it (`set_view`). With filtering on, the chroma key leaves the keyed texels
//! out of the bilinear filter, and their share becomes a soft alpha edge.
//!
//! Textures are kept premultiplied by their coverage (the key is black, as the game always
//! sets it): rgb and coverage in `array`, the texture's alpha (covered and keyed share) in
//! `alpha`. Filtered by the GPU that is the keyed bilinear filter above, and it also gives
//! mipmaps and anisotropic filtering without dark fringes (`filtering`: REG_DWORD
//! `TextureFilter`). A page is an atlas of small pictures: a primitive samples inside the
//! rectangle of its texture coordinates, at a level that still has a few texels of it.

use super::gl::{compile, texture};
use super::{arg, log, mem, thunk, thunk_not_implemented};
use recomp::rt::{Cpu, EDX};
use glow::HasContext;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// The module handle LoadLibraryA gives for glide2x.dll.
pub const MODULE: u32 = 0x1000_0000;

/// TMU memory reported: 4 MB.
const TMU_RAM: u32 = 4 << 20;

#[derive(Clone, Copy, PartialEq, Debug)]
struct State {
    /// rgb source and destination factor
    blend: (u32, u32),
    chroma: bool,
    key: u32,
    linear: bool,
    clamp: (bool, bool),
    clip: [i32; 4],
    /// grDepthBufferMode (0 off, 1 z, 2 w, 3 z / 4 w compared to the bias), grDepthBufferFunction
    /// (GR_CMP_*), grDepthMask, grDepthBiasLevel
    depth_mode: u32,
    depth_func: u32,
    depth_mask: bool,
    depth_bias: i32,
    /// grFogMode, grFogColorValue (ARGB)
    fog: u32,
    fog_color: u32,
    /// grColorMask: rgb, alpha
    color_mask: (bool, bool),
}

/// What grGlideGetState saves: everything drawing depends on.
#[derive(Clone, Copy)]
struct Saved {
    state: State,
    cc: [u32; 5],
    ac: [u32; 5],
    constant: u32,
    tex: Option<u32>,
    tex_alpha_zero: bool,
    alpha_test: (u32, u32),
    cull: u32,
    lower_left: bool,
}

/// A locked linear frame buffer: a guest buffer of the Glide screen's pixels (16-bit 565),
/// written by the game and drawn over the picture at grLfbUnlock where it changed.
struct Lfb {
    buf: u32,
    write: bool,
    /// GR_LFBWRITEMODE_565 (0), 555 (1), 1555 (2)
    mode: u32,
    /// rows from the bottom (GR_ORIGIN_LOWER_LEFT)
    lower: bool,
}

/// LFB pixels handed out for writing start as this value: words the game leaves are not drawn.
const LFB_UNTOUCHED: u16 = 0x0821;
/// Bytes per row of a locked buffer (Glide's Voodoo: 1024 pixels of 16 bits).
const LFB_STRIDE: u32 = 2048;

/// Where the following drawing goes in the frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    /// the whole frame
    Full,
    /// a 4:3 part: 0 at the left edge, 0.5 centred, 1 at the right edge
    Hud(f32),
    /// split screen: a 4:3 HUD whose left (or right) half is centred in the left (right)
    /// player's half of the frame, and cut to it
    Half(bool),
}

/// A texture in TMU memory: a layer of the texture array.
struct Texture {
    layer: u32,
    w: u32,
    h: u32,
    /// its size in TMU memory
    bytes: u32,
    /// when it was last downloaded (a count)
    stamp: u64,
    /// its layer in the replacements' array, when parts of it are replaced (`hd.rs`)
    hd: Option<u32>,
    /// the texels of its top level as last downloaded, in `format` (empty: not known): rows
    /// downloaded again unchanged are skipped (Nuclear Strike marks a texture page's rows
    /// dirty and downloads them again every frame, ~30 000 single-row uploads a second, ~87%
    /// unchanged)
    raw: Vec<u8>,
    /// the rows of `raw` that are known
    known: Vec<bool>,
    format: u32,
    /// its top level as it goes up (premultiplied rgba, alpha and cover), and the rows not
    /// yet uploaded: a game may download textures a row at a time (Nuclear Strike ~30 000
    /// rows a second, one upload and a flush each); they go up together when the texture is
    /// next drawn with (`commit`)
    staged: Vec<u8>,
    staged_alpha: Vec<u8>,
    pending: Option<(u32, u32)>,
}

/// Layers of the texture array at first: TMU memory holds 64 textures of 256x256 at 8 bits.
/// Small textures fit many more (Recoil downloads 621 at a mission's start): the array then
/// grows (`grow`), up to `MAX_LAYERS` or the driver's limit.
const LAYERS: u32 = 64;
const MAX_LAYERS: u32 = 2048;
/// Levels of a 256x256 texture: 256 ... 1.
const LEVELS: i32 = 9;
/// Layers of the replacements' array.
const HD_LAYERS: u32 = 32;

/// The replacement textures: pages at `scale` times the size, rgb premultiplied by the
/// coverage and the coverage (as `array`), with mipmaps; framebuffers to copy between layers.
struct Hd {
    array: glow::Texture,
    scale: u32,
    stale: bool,
}

/// Texture filtering when the game filters: 0 the original (bilinear, no mipmaps),
/// 1 trilinear mipmaps, 2...16 also anisotropic.
pub const FILTER_CHOICES: &[u32] = &[0, 1, 2, 4, 8, 16];
static FILTER: AtomicU32 = AtomicU32::new(16);

/// The saved choice (default: anisotropic 16x); `<PREFIX>_FILTER=<n>` overrides it.
pub fn filtering() -> u32 {
    if let Some(n) = recomp::config::env("FILTER").ok_or(()).ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    let v = super::advapi32::value("", "TextureFilter").and_then(|(_, d)| Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?)));
    v.filter(|v| FILTER_CHOICES.contains(v)).unwrap_or(16)
}

/// Multisample anti-aliasing: samples per pixel (0 off).
pub const MSAA_CHOICES: &[u32] = &[0, 2, 4, 8];
static MSAA: AtomicU32 = AtomicU32::new(4);

/// The saved choice (REG_DWORD `Msaa`, default 4x); `<PREFIX>_MSAA=<n>` overrides it.
pub fn msaa() -> u32 {
    if let Some(n) = recomp::config::env("MSAA").ok_or(()).ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    let v = super::advapi32::value("", "Msaa").and_then(|(_, d)| Some(u32::from_le_bytes(d.get(..4)?.try_into().ok()?)));
    v.filter(|v| MSAA_CHOICES.contains(v)).unwrap_or(4)
}

/// Preferences' OK: saved, and used from the next frame on.
pub fn set_msaa(n: u32) {
    super::advapi32::set_value("", "Msaa", 4, &n.to_le_bytes());
    MSAA.store(n, Ordering::Relaxed);
}

/// At start, after the EXE is loaded.
pub fn init() {
    FILTER.store(filtering(), Ordering::Relaxed);
    MSAA.store(msaa(), Ordering::Relaxed);
}

/// Preferences' OK: saved, and used from the next batch on.
pub fn set_filtering(n: u32) {
    super::advapi32::set_value("", "TextureFilter", 4, &n.to_le_bytes());
    FILTER.store(n, Ordering::Relaxed);
}

struct Glide {
    /// the Glide screen
    res: (u32, u32),
    /// the texture drawn into
    target: (u32, u32),
    /// the vertices' coordinate space
    space: (u32, u32),
    view: View,
    /// the texture shown (drawn into through each thread's framebuffer, `Local`)
    color: glow::Texture,
    /// with MSAA: the multisampled renderbuffer drawn into, its samples and its number (a
    /// thread's framebuffer for it is found by that, not by the GL name, which may come again);
    /// resolved into `color` at grBufferSwap
    ms: Option<(glow::Renderbuffer, u32, u64, glow::Renderbuffer)>,
    made: u64,
    ms_max: u32,
    prog: glow::Program,
    vbo: glow::Buffer,
    /// what contexts do not share (vertex arrays, framebuffers), per thread drawing
    locals: HashMap<u64, Local>,
    /// the threads that drew and the swap they last drew at (`with`), the swaps so far
    drawers: Vec<(u64, u64)>,
    swaps: u64,
    /// every texture, 256x256 at most, in one array: rgb premultiplied by coverage and the
    /// coverage, with mipmaps (see the module)
    array: glow::Texture,
    /// the textures' alpha: its share in covered texels, and in keyed ones
    alpha: glow::Texture,
    /// the arrays' layers now, and the most they grow to
    layers: u32,
    max_layers: u32,
    /// replacement textures, when there are any
    hd: Option<Hd>,
    /// the anisotropy set on the arrays, and the most the driver takes
    aniso: f32,
    aniso_max: f32,
    textures: BTreeMap<u32, Texture>,
    downloads: u64,
    state: State,
    /// Vertex attributes, so that changing them draws nothing yet: the colour and alpha
    /// combine units (function, factor, local, other, invert), the constant colour (ARGB) and
    /// the TMU address of the texture
    cc: [u32; 5],
    ac: [u32; 5],
    constant: u32,
    tex: Option<u32>,
    /// grTexCombine's alpha function is ZERO: the texture's alpha reads 0 (Recoil)
    tex_alpha_zero: bool,
    /// the alpha test: function (GR_CMP_*, 7 always) and reference value. A vertex attribute
    /// too: games change it between almost all primitives (Nuclear Strike: one draw each, the
    /// driver's draw calls ~10% of its game thread as uniforms)
    alpha_test: (u32, u32),
    /// uniform locations
    uniforms: Uniforms,
    /// vertices waiting to be drawn with `state` (`VERTEX` words each): x y r g b a s t,
    /// constant r g b a (floats), the combine units and texture (see `add`), then the
    /// rectangle of the primitive's texture coordinates s0 t0 s1 t1 (floats)
    batch: Vec<u32>,
    batch_mode: u32,
    gamma: f32,
    /// the depth buffer of `color` (its storage follows the target's size)
    depth_rb: glow::Renderbuffer,
    /// grCullMode: 0 none, 1 negative, 2 positive (triangles are culled as they are added)
    cull: u32,
    /// grSstOrigin: lower left (y up) instead of upper left
    lower_left: bool,
    /// grGlideGetState's saves, by the guest address of their GrState
    saved: HashMap<u32, Saved>,
    lfb: Option<Lfb>,
    /// drawing LFB pixels over the picture: a program and a texture
    blit: (glow::Program, glow::Texture),
}

/// Glide's state, global: a game may draw from more than one thread (Nuclear Strike: its game
/// thread, and a thread writing the text before and after a mission). Each thread draws with
/// its own context sharing the window's (`window::with_gl`).
static GLIDE: Mutex<Option<Glide>> = Mutex::new(None);

// GL names only (numbers); they are used with a context current on the calling thread
unsafe impl Send for Glide {}

/// A thread's own GL objects: a vertex array, a framebuffer for each texture (true) or
/// renderbuffer (false, by its number) drawn into, two for copies.
struct Local {
    vao: glow::VertexArray,
    /// without attributes, for drawing LFB pixels
    empty: glow::VertexArray,
    fbos: HashMap<(bool, u64), glow::Framebuffer>,
    copy: [glow::Framebuffer; 2],
}

impl Local {
    fn new(gl: &glow::Context, vbo: glow::Buffer) -> Local {
        unsafe {
            let vao = gl.create_vertex_array().expect("GL vertex array");
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            vertex_layout(gl);
            let empty = gl.create_vertex_array().expect("GL vertex array");
            let fbo = || gl.create_framebuffer().expect("GL framebuffer");
            Local { vao, empty, fbos: HashMap::new(), copy: [fbo(), fbo()] }
        }
    }

    /// Delete it (on its own thread).
    fn delete(self, gl: &glow::Context) {
        unsafe {
            gl.delete_vertex_array(self.vao);
            gl.delete_vertex_array(self.empty);
            for f in self.fbos.into_values().chain(self.copy) {
                gl.delete_framebuffer(f);
            }
        }
    }
}

/// The batched vertices' layout (see `VERTEX`), for the vertex array bound.
fn vertex_layout(gl: &glow::Context) {
    unsafe {
        for (loc, n, off) in [(0, 2, 0), (1, 4, 8), (2, 3, 24), (3, 4, 36), (5, 4, 68), (6, 2, 84)] {
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_f32(loc, n, glow::FLOAT, false, 4 * VERTEX as i32, off);
        }
        gl.enable_vertex_attrib_array(4);
        gl.vertex_attrib_pointer_i32(4, 4, glow::UNSIGNED_INT, 4 * VERTEX as i32, 52);
    }
}

/// A number for the calling thread (asked at every Glide call: thread::current() cost).
fn thread_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    thread_local! {
        static ID: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    ID.with(|i| *i)
}

const VS: &str = r#"
uniform vec2 space;
layout(location = 0) in vec2 pos;
layout(location = 1) in vec4 col;
layout(location = 2) in vec3 tc;
layout(location = 3) in vec4 kc;
layout(location = 4) in uvec4 mode;
layout(location = 5) in vec4 box;
layout(location = 6) in vec2 dz;
out vec4 vcol;
noperspective out vec3 stq;
noperspective out vec2 depth;
flat out vec4 konst;
flat out uvec4 vmode;
flat out vec4 st_box;
void main() {
    vcol = col / 255.0;
    konst = kc;
    st_box = box;
    vmode = mode;
    // s/w, t/w and 1/w go linearly over the screen: divided per pixel (perspective-correct)
    stq = tc;
    depth = dz;
    gl_Position = vec4(pos.x / space.x * 2.0 - 1.0, 1.0 - pos.y / space.y * 2.0, 0.0, 1.0);
}
"#;

const FS: &str = r#"
uniform sampler2DArray tex;     // rgb * coverage, coverage
uniform sampler2DArray tex_a;   // alpha * coverage, alpha * (1 - coverage)
uniform sampler2DArray tex_hd;  // replacements: rgb * coverage, coverage, larger
uniform int mipmaps;            // filtered by the GPU with mipmaps
uniform ivec2 clamp_st;
uniform int chroma;
uniform vec3 key;
uniform int linear_filter;
uniform int edge;      // how the chroma key's soft edge is applied (see `flush`)
uniform int depth_mode;   // 0 off, 1 z-buffer (ooz), 2 w-buffer (1/oow)
uniform float depth_bias; // grDepthBiasLevel
uniform int fog_mode;     // grFogMode: 1 iterated alpha, | 0x100 mult2, | 0x200 add2
uniform vec3 fog_color;
in vec4 vcol;
noperspective in vec3 stq;
noperspective in vec2 depth;  // ooz, oow
vec2 st;
flat in vec4 konst;    // the constant colour
flat in uvec4 vmode;   // combine units and texture, packed by `add`
flat in vec4 st_box;   // the primitive's texture rectangle
out vec4 color;

ivec4 cc;              // colour combine: function, factor, local, other
int cc_inv;
ivec4 ac;              // alpha combine
int ac_inv;
int use_tex;
int layer;
int hd_layer;          // the replacement's layer, -1: none
ivec2 tex_size;

ivec4 bytes4(uint w) { return ivec4(w & 255u, (w >> 8) & 255u, (w >> 16) & 255u, w >> 24); }

vec4 texel(ivec2 p) {
    ivec2 s = tex_size;
    p = ivec2(clamp_st.x != 0 ? clamp(p.x, 0, s.x - 1) : ((p.x % s.x) + s.x) % s.x,
              clamp_st.y != 0 ? clamp(p.y, 0, s.y - 1) : ((p.y % s.y) + s.y) % s.y);
    vec4 a = texelFetch(tex_a, ivec3(p, layer), 0);
    return vec4(texelFetch(tex, ivec3(p, layer), 0).rgb, a.r + a.g);
}

// The GPU's trilinear (and anisotropic) filter: inside the primitive's texture rectangle,
// at most at the level where that still spans 4 texels (a page is an atlas). A replacement
// gives the colour and coverage; the texel alpha stays the original's.
vec4 sample_mip(out float cover) {
    vec2 size = vec2(textureSize(tex, 0).xy);
    vec2 dx = dFdx(st), dy = dFdy(st);
    vec2 lo = st_box.xy, hi = st_box.zw;
    // a texture repeated over the primitive (its rectangle leaves the texture): the place in
    // the texture, inside the whole texture
    vec2 ts = vec2(tex_size);
    bvec2 wrap = bvec2(clamp_st.x == 0 && (lo.x < 0.0 || hi.x > ts.x), clamp_st.y == 0 && (lo.y < 0.0 || hi.y > ts.y));
    vec2 q = st;
    if (wrap.x) { lo.x = 0.0; hi.x = ts.x; q.x = mod(st.x, ts.x); }
    if (wrap.y) { lo.y = 0.0; hi.y = ts.y; q.y = mod(st.y, ts.y); }
    // clamped (or a rectangle inside the texture): never past the texture's edge
    lo = clamp(lo, vec2(0.0), ts);
    hi = clamp(hi, lo, ts);
    float lod = log2(max(max(length(dx), length(dy)), 1e-8));
    float top = max(log2(max(min(hi.x - lo.x, hi.y - lo.y), 1.0)) - 2.0, 0.0);
    if (lod > top) {
        float k = exp2(top - lod);
        dx *= k;
        dy *= k;
        lod = top;
    }
    vec2 m = vec2(0.5 * exp2(max(lod, 0.0)));
    vec2 p = clamp(q, lo + min(m, (hi - lo) * 0.5), hi - min(m, (hi - lo) * 0.5));
    vec3 uv = vec3(p / size, float(layer));
    vec4 c = textureGrad(tex, uv, dx / size, dy / size);
    vec4 a = textureGrad(tex_a, uv, dx / size, dy / size);
    float ta = c.a > 0.0 ? a.r / c.a : 0.0;
    if (hd_layer >= 0) c = textureGrad(tex_hd, vec3(uv.xy, float(hd_layer)), dx / size, dy / size);
    if (chroma == 0) {
        cover = 1.0;
        return vec4(c.rgb, a.r + a.g);
    }
    cover = c.a;
    return c.a > 0.0 ? vec4(c.rgb / c.a, ta) : vec4(0.0);
}

bool keyed(vec3 c) { return chroma != 0 && all(lessThan(abs(c - key), vec3(0.5 / 255.0))); }

// the texture colour and how much of it is not keyed out
vec4 sample_tex(out float cover) {
    if (hd_layer >= 0 || linear_filter != 0 && mipmaps != 0) return sample_mip(cover);
    if (linear_filter == 0) {
        vec4 t = texel(ivec2(floor(st)));
        cover = keyed(t.rgb) ? 0.0 : 1.0;
        return t;
    }
    vec2 p = st - 0.5;
    ivec2 i = ivec2(floor(p));
    vec2 f = p - floor(p);
    vec4 sum = vec4(0.0);
    float wsum = 0.0;
    for (int k = 0; k < 4; k++) {
        ivec2 o = ivec2(k & 1, k >> 1);
        vec4 t = texel(i + o);
        float w = (o.x == 1 ? f.x : 1.0 - f.x) * (o.y == 1 ? f.y : 1.0 - f.y);
        if (keyed(t.rgb)) w = 0.0;
        sum += t * w;
        wsum += w;
    }
    cover = wsum;
    return wsum > 0.0 ? sum / wsum : vec4(0.0);
}

vec3 factor3(int f, vec3 local, float local_a, float other_a, float tex_a) {
    if (f == 0) return vec3(0.0);
    if (f == 1) return local;
    if (f == 2) return vec3(other_a);
    if (f == 3) return vec3(local_a);
    if (f == 4) return vec3(tex_a);
    if (f == 8) return vec3(1.0);
    if (f == 9) return 1.0 - local;
    if (f == 10) return vec3(1.0 - other_a);
    if (f == 11) return vec3(1.0 - local_a);
    if (f == 12) return vec3(1.0 - tex_a);
    return vec3(1.0);
}

vec3 combine3(int fn_, vec3 f, vec3 local, float local_a, vec3 other) {
    if (fn_ == 0) return vec3(0.0);
    if (fn_ == 1) return local;
    if (fn_ == 2) return vec3(local_a);
    if (fn_ == 3) return f * other;
    if (fn_ == 4) return f * other + local;
    if (fn_ == 5) return f * other + local_a;
    if (fn_ == 6) return f * (other - local);
    if (fn_ == 7) return f * (other - local) + local;
    if (fn_ == 8) return f * (other - local) + local_a;
    if (fn_ == 9) return -f * local + local;
    if (fn_ == 16) return -f * local + local_a;
    return local;
}

void main() {
    cc = bytes4(vmode.x);
    ac = bytes4(vmode.y);
    cc_inv = int(vmode.z & 1u);
    ac_inv = int((vmode.z >> 1) & 1u);
    use_tex = int((vmode.z >> 2) & 1u);
    bool tex_alpha_zero = ((vmode.z >> 3) & 1u) != 0u;
    layer = int((vmode.z >> 8) & 4095u);
    hd_layer = int(vmode.z >> 20) - 1;
    tex_size = ivec2(vmode.w & 0x3ffu, (vmode.w >> 10) & 0x3ffu);
    st = stq.z != 0.0 ? stq.xy / stq.z : stq.xy;
    // the depth compared: z-buffer ooz / 65535 (larger nearer); w-buffer 1 / (w + 1) (larger
    // nearer, exact far away; the depth functions are turned around for it, see `flush`)
    if (depth_mode == 1) gl_FragDepth = clamp((depth.x + depth_bias) / 65535.0, 0.0, 1.0);
    else if (depth_mode == 2) gl_FragDepth = 1.0 / (1.0 / max(depth.y, 1e-30) + depth_bias + 1.0);
    else gl_FragDepth = 0.5;
    float cover = 1.0;
    vec4 t = use_tex != 0 ? sample_tex(cover) : vec4(1.0);
    if (tex_alpha_zero) t.a = 0.0;
    // alpha first: colour factors may use it
    float a_local = ac.z == 0 ? vcol.a : konst.a;
    float a_other = ac.w == 0 ? vcol.a : ac.w == 1 ? t.a : konst.a;
    float af = factor3(ac.y, vec3(a_local), a_local, a_other, t.a).x;
    float a = clamp(combine3(ac.x, vec3(af), vec3(a_local), a_local, vec3(a_other)).x, 0.0, 1.0);
    if (ac_inv != 0) a = 1.0 - a;
    vec3 c_local = cc.z == 0 ? vcol.rgb : konst.rgb;
    vec3 c_other = cc.w == 0 ? vcol.rgb : cc.w == 1 ? t.rgb : konst.rgb;
    // the chroma key compares the colour unit's "other" input
    if (cc.w != 1 && keyed(c_other)) discard;
    if (cc.w != 1) cover = 1.0;
    vec3 cf = factor3(cc.y, c_local, a_local, a_other, t.a);
    vec3 c = clamp(combine3(cc.x, cf, c_local, a_local, c_other), 0.0, 1.0);
    if (cc_inv != 0) c = 1.0 - c;
    // the alpha test: GR_CMP_* function, reference value
    int ai = int(round(a * 255.0)), ar = int(vmode.w >> 24);
    int f = int((vmode.w >> 20) & 7u);
    bool pass = f == 7 || (f == 1 && ai < ar) || (f == 2 && ai == ar) || (f == 3 && ai <= ar)
        || (f == 4 && ai > ar) || (f == 5 && ai != ar) || (f == 6 && ai >= ar);
    if (!pass) discard;
    if (edge == 0 && cover < 0.5) discard;
    if (edge == 1) a = cover;
    if (edge == 2) a *= cover;
    if (edge == 3) c *= cover;
    if (cover <= 0.0) discard;
    // fog after the combine units: the factor is the iterated alpha
    if ((fog_mode & 0xff) == 1) {
        float f = clamp(vcol.a, 0.0, 1.0);
        if ((fog_mode & 0x100) != 0) c = (1.0 - f) * c;
        else if ((fog_mode & 0x200) != 0) c = f * fog_color;
        else c = mix(c, fog_color, f);
    }
    color = vec4(c, a);
}
"#;

/// LFB pixels drawn over the picture: a rectangle (GL coordinates x0 y0 x1 y1) of a texture,
/// its transparent texels left out.
const BLIT_VS: &str = r#"
uniform vec4 rect;
out vec2 uv;
void main() {
    vec2 c = vec2(gl_VertexID & 1, gl_VertexID >> 1);
    uv = vec2(c.x, 1.0 - c.y);
    gl_Position = vec4(mix(rect.xy, rect.zw, c), 0.0, 1.0);
}
"#;

const BLIT_FS: &str = r#"
uniform sampler2D img;
in vec2 uv;
out vec4 color;
void main() {
    color = texture(img, uv);
    if (color.a == 0.0) discard;
}
"#;

/// Words per batched vertex: x y, r g b a, sow tow oow, constant r g b a, the mode (4 words),
/// the texture rectangle s0 t0 s1 t1, ooz oow.
const VERTEX: usize = 23;

/// The shaders' uniforms, looked up once.
struct Uniforms {
    space: Option<glow::UniformLocation>,
    tex: Option<glow::UniformLocation>,
    tex_a: Option<glow::UniformLocation>,
    tex_hd: Option<glow::UniformLocation>,
    mipmaps: Option<glow::UniformLocation>,
    clamp_st: Option<glow::UniformLocation>,
    chroma: Option<glow::UniformLocation>,
    key: Option<glow::UniformLocation>,
    linear_filter: Option<glow::UniformLocation>,
    edge: Option<glow::UniformLocation>,
    depth_mode: Option<glow::UniformLocation>,
    depth_bias: Option<glow::UniformLocation>,
    fog_mode: Option<glow::UniformLocation>,
    fog_color: Option<glow::UniformLocation>,
}

impl Uniforms {
    fn new(gl: &glow::Context, prog: glow::Program) -> Uniforms {
        let u = |n: &str| unsafe { gl.get_uniform_location(prog, n) };
        Uniforms {
            space: u("space"),
            tex: u("tex"),
            tex_a: u("tex_a"),
            tex_hd: u("tex_hd"),
            mipmaps: u("mipmaps"),
            clamp_st: u("clamp_st"),
            chroma: u("chroma"),
            key: u("key"),
            linear_filter: u("linear_filter"),
            edge: u("edge"),
            depth_mode: u("depth_mode"),
            depth_bias: u("depth_bias"),
            fog_mode: u("fog_mode"),
            fog_color: u("fog_color"),
        }
    }
}

/// The TMU addresses of the textures downloaded (`savestate`'s fingerprint).
pub fn texture_addresses() -> Vec<u32> {
    with(|g, _| g.textures.keys().copied().collect()).unwrap_or_default()
}

/// The aspect drawn for a window of aspect `a` (None: Glide closes): the game's choice.
fn aspect(a: Option<f32>) -> f32 {
    crate::game::game().glide().map_or(4.0 / 3.0, |x| x.aspect(a))
}

fn with<R>(f: impl FnOnce(&mut Glide, &glow::Context) -> R) -> Option<R> {
    // the context first: getting one may wait for the window's thread, which may call Glide
    if !super::window::gl_here() {
        return None;
    }
    let mut g = GLIDE.lock().unwrap_or_else(|e| e.into_inner());
    let g = g.as_mut()?;
    super::window::with_gl(|gl| {
        let r = f(g, gl);
        // while another thread draws too (within the last two pictures), the next call may be
        // its context: it sees this one's work once it is flushed
        let me = thread_id();
        let swaps = g.swaps;
        let mut other = false;
        let mut known = false;
        for d in g.drawers.iter_mut() {
            if d.0 == me {
                d.1 = swaps;
                known = true;
            } else if d.1 + 2 >= swaps {
                other = true;
            }
        }
        if !known {
            g.drawers.push((me, swaps));
        }
        if other {
            unsafe { gl.flush() };
        }
        r
    })
}

fn f32_arg(c: &Cpu, n: u32) -> f32 {
    f32::from_bits(arg(c, n))
}

/// Size of a texture level: LOD 0 is 256, 8 is 1; aspect 0..6 is 8:1 .. 1:8.
/// Pixels of `ch` bytes, w x h, at half the size (a box of 2x2, or 2 where a side is 1).
fn half(px: &[u8], w: usize, h: usize, ch: usize) -> (Vec<u8>, usize, usize) {
    let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));
    let (sx, sy) = (if w > 1 { 2 } else { 1 }, if h > 1 { 2 } else { 1 });
    let mut out = vec![0u8; hw * hh * ch];
    for y in 0..hh {
        for x in 0..hw {
            for c in 0..ch {
                let mut sum = 0u32;
                for dy in 0..sy {
                    for dx in 0..sx {
                        sum += px[((y * sy + dy) * w + x * sx + dx) * ch + c] as u32;
                    }
                }
                let n = (sx * sy) as u32;
                out[(y * hw + x) * ch + c] = ((sum + n / 2) / n) as u8;
            }
        }
    }
    (out, hw, hh)
}

/// A texture array of `n` layers of 256x256 with mipmaps, filtered trilinear, clamped.
unsafe fn layer_array(gl: &glow::Context, format: u32, n: u32) -> glow::Texture {
    unsafe {
        let t = gl.create_texture().expect("GL texture");
        gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(t));
        gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        for p in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
            gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, p, glow::CLAMP_TO_EDGE as i32);
        }
        gl.tex_storage_3d(glow::TEXTURE_2D_ARRAY, LEVELS, format, 256, 256, n as i32);
        t
    }
}

fn lod_size(lod: u32, aspect: u32) -> (u32, u32) {
    let l = 256 >> lod.min(8);
    match aspect {
        0..=3 => (l, (l >> (3 - aspect)).max(1)),
        _ => ((l >> (aspect - 3)).max(1), l),
    }
}

/// Texels of a format as RGBA bytes (alpha 255 where the format has none).
fn convert(format: u32, data: u32, n: usize) -> Vec<u8> {
    let m = mem();
    let e5 = |v: u32| ((v << 3) | (v >> 2)) as u8;
    let e6 = |v: u32| ((v << 2) | (v >> 4)) as u8;
    let e4 = |v: u32| (v * 17) as u8;
    let mut out = Vec::with_capacity(n * 4);
    for k in 0..n as u32 {
        let px: [u8; 4] = match format {
            0xb => {
                let v = m.rd16(data + 2 * k) as u32;
                [e5(v >> 10 & 31), e5(v >> 5 & 31), e5(v & 31), if v & 0x8000 != 0 { 255 } else { 0 }]
            }
            0xa => {
                let v = m.rd16(data + 2 * k) as u32;
                [e5(v >> 11), e6(v >> 5 & 63), e5(v & 31), 255]
            }
            0xc => {
                let v = m.rd16(data + 2 * k) as u32;
                [e4(v >> 8 & 15), e4(v >> 4 & 15), e4(v & 15), e4(v >> 12)]
            }
            0xd => {
                let v = m.rd16(data + 2 * k);
                let i = v as u8;
                [i, i, i, (v >> 8) as u8]
            }
            0 => {
                let v = m.rd8(data + k) as u32;
                [(v >> 5) as u8 * 36, (v >> 2 & 7) as u8 * 36, (v & 3) as u8 * 85, 255]
            }
            2 => [255, 255, 255, m.rd8(data + k)],
            3 => {
                let i = m.rd8(data + k);
                [i, i, i, 255]
            }
            _ => [255, 0, 255, 255],
        };
        out.extend(px);
    }
    out
}

/// 0xRRGGBB as a 565 pixel.
fn to_565(c: u32) -> u16 {
    ((c >> 8 & 0xf800) | (c >> 5 & 0x07e0) | (c >> 3 & 0x1f)) as u16
}

/// A 16-bit LFB pixel (GR_LFBWRITEMODE_565 0, 555 1, 1555 2) as opaque RGBA.
fn pixel16(v: u16, mode: u32) -> [u8; 4] {
    let v = v as u32;
    let e5 = |x: u32| ((x << 3) | (x >> 2)) as u8;
    let e6 = |x: u32| ((x << 2) | (x >> 4)) as u8;
    match mode {
        1 | 2 => [e5(v >> 10 & 31), e5(v >> 5 & 31), e5(v & 31), 255],
        _ => [e5(v >> 11), e6(v >> 5 & 63), e5(v & 31), 255],
    }
}

fn bytes_per_texel(format: u32) -> u32 {
    if format >= 8 { 2 } else { 1 }
}

impl Hd {
    /// The array for replacements, when there are any.
    fn open(gl: &glow::Context) -> Option<Hd> {
        let scale = super::hd::scale();
        if scale == 0 {
            return None;
        }
        let size = 256 * scale as i32;
        unsafe {
            let array = gl.create_texture().expect("GL texture");
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(array));
            gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            for p in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, p, glow::CLAMP_TO_EDGE as i32);
            }
            gl.tex_storage_3d(glow::TEXTURE_2D_ARRAY, LEVELS + scale.trailing_zeros() as i32, glow::RGBA8, size, size, HD_LAYERS as i32);
            Some(Hd { array, scale, stale: false })
        }
    }

    /// Rows `r0..=r1` of the original texture (`w` wide, in `layer` of `array`) scaled up into
    /// the replacement's `hd_layer`.
    fn scale_up(&mut self, gl: &glow::Context, [read, draw]: [glow::Framebuffer; 2], array: glow::Texture, layer: u32, hd_layer: u32, w: u32, (r0, r1): (u32, u32)) {
        let k = self.scale as i32;
        let (w, r0, r1) = (w as i32, r0 as i32, r1 as i32 + 1);
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(read));
            gl.framebuffer_texture_layer(glow::READ_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, Some(array), 0, layer as i32);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(draw));
            gl.framebuffer_texture_layer(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, Some(self.array), 0, hd_layer as i32);
            gl.blit_framebuffer(0, r0, w, r1, 0, r0 * k, w * k, r1 * k, glow::COLOR_BUFFER_BIT, glow::LINEAR);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        self.stale = true;
    }

    /// A replacement (straight alpha) into rows from `r0` (of the original) of `hd_layer`.
    fn put(&mut self, gl: &glow::Context, hd_layer: u32, r0: u32, img: &super::hd::Image) {
        let px: Vec<u8> = img
            .rgba
            .chunks(4)
            .flat_map(|p| {
                let a = p[3] as u32;
                let m = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                [m(p[0]), m(p[1]), m(p[2]), p[3]]
            })
            .collect();
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(self.array));
            let y = (r0 * self.scale) as i32;
            gl.tex_sub_image_3d(glow::TEXTURE_2D_ARRAY, 0, 0, y, hd_layer as i32, img.w as i32, img.h as i32, 1, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(&px)));
        }
        self.stale = true;
    }
}

impl Glide {
    fn open(gl: &glow::Context, w: u32, h: u32) -> Glide {
        let target = (w, w * 3 / 4);
        unsafe {
            let color = texture(gl, true);
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, target.0 as i32, target.1 as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            let prog = compile(gl, VS, FS);
            let vbo = gl.create_buffer().expect("GL buffer");
            let aniso_max = if gl.supported_extensions().iter().any(|e| e.ends_with("texture_filter_anisotropic")) {
                gl.get_parameter_f32(glow::MAX_TEXTURE_MAX_ANISOTROPY_EXT)
            } else {
                1.0
            };
            let array = layer_array(gl, glow::RGBA8, LAYERS);
            let alpha = layer_array(gl, glow::RG8, LAYERS);
            let max_layers = (gl.get_parameter_i32(glow::MAX_ARRAY_TEXTURE_LAYERS).max(LAYERS as i32) as u32).min(MAX_LAYERS);
            let depth_rb = gl.create_renderbuffer().expect("GL renderbuffer");
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth_rb));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT32F, target.0 as i32, target.1 as i32);
            let blit = (compile(gl, BLIT_VS, BLIT_FS), texture(gl, false));
            Glide {
                res: (w, h),
                target,
                space: (w, h),
                view: View::Full,
                color,
                ms: None,
                made: 0,
                ms_max: gl.get_parameter_i32(glow::MAX_SAMPLES).max(0) as u32,
                prog,
                vbo,
                locals: HashMap::new(),
                drawers: Vec::new(),
                swaps: 0,
                array,
                alpha,
                layers: LAYERS,
                max_layers,
                hd: Hd::open(gl),
                aniso: 0.0,
                aniso_max,
                textures: BTreeMap::new(),
                downloads: 0,
                cc: [1, 0, 0, 0, 0],
                ac: [1, 0, 0, 0, 0],
                constant: 0xffff_ffff,
                tex: None,
                tex_alpha_zero: false,
                alpha_test: (7, 0),
                uniforms: Uniforms::new(gl, prog),
                state: State {
                    blend: (4, 0),
                    chroma: false,
                    key: 0,
                    linear: false,
                    clamp: (true, true),
                    clip: [0, 0, w as i32, h as i32],
                    depth_mode: 0,
                    depth_func: 1,
                    depth_mask: true,
                    depth_bias: 0,
                    fog: 0,
                    fog_color: 0,
                    color_mask: (true, true),
                },
                batch: Vec::new(),
                batch_mode: glow::TRIANGLES,
                gamma: 1.0,
                depth_rb,
                cull: 0,
                lower_left: false,
                saved: HashMap::new(),
                lfb: None,
                blit,
            }
        }
    }

    /// This thread's own GL objects.
    fn local(&mut self, gl: &glow::Context) -> &mut Local {
        let vbo = self.vbo;
        self.locals.entry(thread_id()).or_insert_with(|| Local::new(gl, vbo))
    }

    /// This thread's framebuffer drawing into `color`.
    fn color_fbo(&mut self, gl: &glow::Context) -> glow::Framebuffer {
        let (color, depth) = (self.color, self.depth_rb);
        *self.local(gl).fbos.entry((true, color.0.get() as u64)).or_insert_with(|| unsafe {
            let f = gl.create_framebuffer().expect("GL framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(color), 0);
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, Some(depth));
            f
        })
    }

    /// This thread's framebuffer drawing into the multisampled renderbuffer, if there is one.
    fn ms_fbo(&mut self, gl: &glow::Context) -> Option<glow::Framebuffer> {
        let (rb, _, n, depth) = self.ms?;
        Some(*self.local(gl).fbos.entry((false, n)).or_insert_with(|| unsafe {
            let f = gl.create_framebuffer().expect("GL framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::RENDERBUFFER, Some(rb));
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, Some(depth));
            f
        }))
    }

    /// The view's rectangle in the target (x, y from the top, w, h) and the columns it is cut to.
    fn view_rect(&self) -> ([f32; 4], (f32, f32)) {
        let (tw, th) = (self.target.0 as f32, self.target.1 as f32);
        let w43 = (th * 4.0 / 3.0).min(tw);
        match self.view {
            View::Full => ([0.0, 0.0, tw, th], (0.0, tw)),
            View::Hud(at) => ([(tw - w43) * at, 0.0, w43, th], (0.0, tw)),
            View::Half(false) => ([tw / 4.0 - w43 / 4.0, 0.0, w43, th], (0.0, tw / 2.0)),
            View::Half(true) => ([tw * 0.75 - w43 * 0.75, 0.0, w43, th], (tw / 2.0, tw)),
        }
    }

    /// GL's viewport for the view, and a clip rectangle (x0 y0 x1 y1 in screen pixels) as
    /// its scissor box.
    fn scissor(&self, gl: &glow::Context, c: [i32; 4]) {
        let ([vx, vy, vw, vh], (cut0, cut1)) = self.view_rect();
        let th = self.target.1 as f32;
        let (sx, sy) = (vw / self.res.0 as f32, vh / self.res.1 as f32);
        let x0 = (vx + c[0] as f32 * sx).max(cut0).round() as i32;
        let x1 = (vx + c[2] as f32 * sx).min(cut1).round() as i32;
        let (y0, y1) = ((vy + c[1] as f32 * sy).round() as i32, (vy + c[3] as f32 * sy).round() as i32);
        unsafe {
            gl.viewport(vx.round() as i32, (th - vy - vh).round() as i32, vw.round() as i32, vh.round() as i32);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(x0, th as i32 - y1, (x1 - x0).max(0), (y1 - y0).max(0));
        }
    }

    /// Size the target (between frames): as tall as the render size `h` (the window's or the
    /// chosen resolution: high resolution), as wide as the aspect `a` makes it.
    fn resize(&mut self, gl: &glow::Context, a: f32, h: u32) {
        let h = h.clamp(240, 4320);
        let w = (h as f32 * a).round() as u32;
        if (w, h) == self.target {
            // the samples may have changed in Preferences
            self.multisample(gl, false);
            return;
        }
        self.target = (w, h);
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.color));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w as i32, h as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(self.depth_rb));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT32F, w as i32, h as i32);
            let fbo = self.color_fbo(gl);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.depth_mask(true);
            gl.clear_depth_f32(0.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        }
        self.multisample(gl, true);
    }

    /// The multisampled framebuffer for the chosen samples and the target's size (`resized`:
    /// the target changed).
    fn multisample(&mut self, gl: &glow::Context, resized: bool) {
        let samples = MSAA.load(Ordering::Relaxed).min(self.ms_max);
        let samples = if samples < 2 { 0 } else { samples };
        if !resized && self.ms.map_or(0, |m| m.1) == samples {
            return;
        }
        unsafe {
            if let Some((rb, _, n, depth)) = self.ms.take() {
                // this thread's framebuffer for it (other threads' stay unused)
                if let Some(f) = self.local(gl).fbos.remove(&(false, n)) {
                    gl.delete_framebuffer(f);
                }
                gl.delete_renderbuffer(rb);
                gl.delete_renderbuffer(depth);
            }
            if samples == 0 {
                return;
            }
            let (w, h) = (self.target.0 as i32, self.target.1 as i32);
            let rb = gl.create_renderbuffer().expect("GL renderbuffer");
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rb));
            gl.renderbuffer_storage_multisample(glow::RENDERBUFFER, samples as i32, glow::RGBA8, w, h);
            let depth = gl.create_renderbuffer().expect("GL renderbuffer");
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
            gl.renderbuffer_storage_multisample(glow::RENDERBUFFER, samples as i32, glow::DEPTH_COMPONENT32F, w, h);
            self.made += 1;
            self.ms = Some((rb, samples, self.made, depth));
            let fbo = self.ms_fbo(gl).unwrap();
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.depth_mask(true);
            gl.clear_depth_f32(0.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        }
    }

    /// The framebuffer drawn into.
    fn draw_fbo(&mut self, gl: &glow::Context) -> glow::Framebuffer {
        match self.ms_fbo(gl) {
            Some(f) => f,
            None => self.color_fbo(gl),
        }
    }

    /// The multisampled picture into the texture shown.
    fn resolve(&mut self, gl: &glow::Context) {
        if let Some(fbo) = self.ms_fbo(gl) {
            let (w, h) = (self.target.0 as i32, self.target.1 as i32);
            let color = self.color_fbo(gl);
            unsafe {
                gl.disable(glow::SCISSOR_TEST);
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(color));
                gl.blit_framebuffer(0, 0, w, h, 0, 0, w, h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            }
        }
    }

    /// Draw what is batched with the current state.
    fn flush(&mut self, gl: &glow::Context) {
        if self.batch.is_empty() {
            return;
        }
        let s = self.state;
        let fbo = self.draw_fbo(gl);
        let vao = self.local(gl).vao;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            self.scissor(gl, s.clip);
            gl.use_program(Some(self.prog));
            gl.bind_vertex_array(Some(vao));
            let u = &self.uniforms;
            gl.uniform_2_f32(u.space.as_ref(), self.space.0 as f32, self.space.1 as f32);
            let filter = FILTER.load(Ordering::Relaxed);
            let mipmaps = filter != 0 && s.key & 0xff_ffff == 0;
            gl.uniform_1_i32(u.mipmaps.as_ref(), mipmaps as i32);
            gl.uniform_1_i32(u.tex_a.as_ref(), 1);
            gl.uniform_1_i32(u.tex.as_ref(), 0);
            gl.uniform_1_i32(u.tex_hd.as_ref(), 2);
            let aniso = (filter.max(1) as f32).min(self.aniso_max);
            if let Some(hd) = &mut self.hd {
                gl.active_texture(glow::TEXTURE2);
                gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(hd.array));
                if hd.stale {
                    gl.generate_mipmap(glow::TEXTURE_2D_ARRAY);
                    hd.stale = false;
                }
                if aniso != self.aniso && self.aniso_max > 1.0 {
                    gl.tex_parameter_f32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAX_ANISOTROPY_EXT, aniso);
                }
            }
            for (k, t) in [(glow::TEXTURE1, self.alpha), (glow::TEXTURE0, self.array)] {
                gl.active_texture(k);
                gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(t));
                if aniso != self.aniso && self.aniso_max > 1.0 {
                    gl.tex_parameter_f32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAX_ANISOTROPY_EXT, aniso);
                }
            }
            self.aniso = aniso;
            gl.uniform_2_i32(u.clamp_st.as_ref(), s.clamp.0 as i32, s.clamp.1 as i32);
            gl.uniform_1_i32(u.chroma.as_ref(), s.chroma as i32);
            let key = s.key;
            gl.uniform_3_f32(u.key.as_ref(), (key >> 16 & 0xff) as f32 / 255.0, (key >> 8 & 0xff) as f32 / 255.0, (key & 0xff) as f32 / 255.0);
            gl.uniform_1_i32(u.linear_filter.as_ref(), s.linear as i32);
            // blending: Glide factors 0 zero, 1 src alpha, 2 src/dst colour, 3 dst alpha, 4 one,
            // 5 1 - src alpha, 6 1 - src/dst colour, 7 1 - dst alpha
            let factor = |f: u32, dst: bool| match f {
                0 => glow::ZERO,
                1 => glow::SRC_ALPHA,
                2 => if dst { glow::SRC_COLOR } else { glow::DST_COLOR },
                3 => glow::DST_ALPHA,
                5 => glow::ONE_MINUS_SRC_ALPHA,
                6 => if dst { glow::ONE_MINUS_SRC_COLOR } else { glow::ONE_MINUS_DST_COLOR },
                7 => glow::ONE_MINUS_DST_ALPHA,
                _ => glow::ONE,
            };
            let (src, dst) = (factor(s.blend.0, false), factor(s.blend.1, true));
            let opaque = src == glow::ONE && dst == glow::ZERO;
            let soft = s.chroma && s.linear;
            // the soft chroma key edge: 0 cut at half, 1 alpha = cover (blending turned on for
            // it), 2 alpha *= cover, 3 rgb *= cover (additive). Untextured vertices have a
            // cover of 1, which leaves them as they are.
            let edge = if !soft {
                0
            } else if opaque {
                1
            } else if src == glow::SRC_ALPHA || dst == glow::ONE_MINUS_SRC_ALPHA {
                2
            } else if src == glow::ONE && dst == glow::ONE {
                3
            } else {
                0
            };
            gl.uniform_1_i32(u.edge.as_ref(), edge);
            if opaque && edge != 1 {
                gl.disable(glow::BLEND);
            } else {
                gl.enable(glow::BLEND);
                if edge == 1 {
                    gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                } else {
                    gl.blend_func(src, dst);
                }
            }
            // the depth buffer: z (1, 3) or w (2, 4); compared values grow towards the viewer
            // for z, and the stored w values too (1 / (w + 1)): Glide's functions on w turn round
            let depth = match s.depth_mode {
                1 | 3 => 1,
                2 | 4 => 2,
                _ => 0,
            };
            gl.uniform_1_i32(u.depth_mode.as_ref(), depth);
            gl.uniform_1_f32(u.depth_bias.as_ref(), s.depth_bias as f32);
            if depth == 0 {
                gl.disable(glow::DEPTH_TEST);
            } else {
                gl.enable(glow::DEPTH_TEST);
                let w = depth == 2;
                gl.depth_func(match s.depth_func & 7 {
                    0 => glow::NEVER,
                    1 => if w { glow::GREATER } else { glow::LESS },
                    2 => glow::EQUAL,
                    3 => if w { glow::GEQUAL } else { glow::LEQUAL },
                    4 => if w { glow::LESS } else { glow::GREATER },
                    5 => glow::NOTEQUAL,
                    6 => if w { glow::LEQUAL } else { glow::GEQUAL },
                    _ => glow::ALWAYS,
                });
            }
            gl.depth_mask(depth != 0 && s.depth_mask);
            gl.color_mask(s.color_mask.0, s.color_mask.0, s.color_mask.0, s.color_mask.1);
            gl.uniform_1_i32(u.fog_mode.as_ref(), s.fog as i32);
            let fc = s.fog_color;
            gl.uniform_3_f32(u.fog_color.as_ref(), (fc >> 16 & 0xff) as f32 / 255.0, (fc >> 8 & 0xff) as f32 / 255.0, (fc & 0xff) as f32 / 255.0);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            let bytes = std::slice::from_raw_parts(self.batch.as_ptr() as *const u8, self.batch.len() * 4);
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
            gl.draw_arrays(self.batch_mode, 0, (self.batch.len() / VERTEX) as i32);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
            gl.depth_mask(true);
            gl.color_mask(true, true, true, true);
        }
        self.batch.clear();
    }

    /// Change the state, drawing what was batched with the old one first.
    fn set(&mut self, gl: &glow::Context, f: impl FnOnce(&mut State)) {
        let mut s = self.state;
        f(&mut s);
        if s != self.state {
            self.flush(gl);
            self.state = s;
        }
    }

    /// Add vertices (GrVertex at guest addresses) as primitives of `mode`.
    fn add(&mut self, gl: &glow::Context, mode: u32, verts: &[u32]) {
        let m = mem();
        if let (1 | 2, &[a, b, c]) = (self.cull, verts) {
            // Glide's signed area (in its own y direction): negative or positive ones are culled
            let p = |v: u32| (f32::from_bits(m.rd32(v)), f32::from_bits(m.rd32(v + 4)));
            let (a, b, c) = (p(a), p(b), p(c));
            let area = (a.0 - b.0) * (b.1 - c.1) - (b.0 - c.0) * (a.1 - b.1);
            if self.cull == 1 && area < 0.0 || self.cull == 2 && area > 0.0 {
                return;
            }
        }
        if mode != self.batch_mode {
            self.flush(gl);
            self.batch_mode = mode;
        }
        let k = self.constant;
        let ch = |sh: u32| ((k >> sh & 0xff) as f32 / 255.0).to_bits();
        let konst = [ch(16), ch(8), ch(0), ch(24)];
        // the combine units a byte each, then invert bits, texturing, the layer (12 bits: up to
        // MAX_LAYERS) and the replacement's layer + 1, its size
        let bytes = |u: [u32; 5]| u[0] | u[1] << 8 | u[2] << 16 | u[3] << 24;
        if let Some(a) = self.tex {
            self.commit(gl, a);
        }
        let tex = self.tex.and_then(|a| self.textures.get(&a));
        let mode = [
            bytes(self.cc),
            bytes(self.ac),
            (self.cc[4] & 1) | (self.ac[4] & 1) << 1 | (tex.is_some() as u32) << 2 | (self.tex_alpha_zero as u32) << 3 | tex.map_or(0, |t| t.layer << 8 | t.hd.map_or(0, |l| l + 1) << 20),
            tex.map_or(0, |t| t.w | t.h << 10) | self.alpha_test.0 << 20 | self.alpha_test.1 << 24,
        ];
        // x y z r g b ooz a oow, then tmu 0: sow tow oow. Glide's s and t go 0..256 over the
        // texture's longer side whatever its size: in texels times its size / 256
        let k = tex.map_or(1.0, |t| t.w.max(t.h) as f32 / 256.0);
        let sow = |v: u32| (f32::from_bits(m.rd32(v + 0x24)) * k, f32::from_bits(m.rd32(v + 0x28)) * k);
        // tmu 0's oow, else the vertex's (games that leave it 0: Recoil)
        let q = |v: u32| match f32::from_bits(m.rd32(v + 0x2c)) {
            0.0 => match f32::from_bits(m.rd32(v + 0x20)) {
                0.0 => 1.0,
                q => q,
            },
            q => q,
        };
        let st = |v: u32| {
            let (s, t) = sow(v);
            (s / q(v), t / q(v))
        };
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for &v in verts {
            let (s, t) = st(v);
            b = [b[0].min(s), b[1].min(t), b[2].max(s), b[3].max(t)];
        }
        let b = b.map(f32::to_bits);
        // with the origin at the lower left, y counts up from the bottom
        let flip = self.lower_left.then_some(self.space.1 as f32);
        let xy = |v: u32| {
            let y = f32::from_bits(m.rd32(v + 4));
            (f32::from_bits(m.rd32(v)), flip.map_or(y, |h| h - y))
        };
        // a line: a quad as wide as the game's line at its resolution
        let quad = if let [v0, v1] = *verts { Some(self.line_offsets(xy(v0), xy(v1))) } else { None };
        let mut put = |v: u32, (x, y): (f32, f32)| {
            // tmu 0's sow tow oow, interpolated linearly on the screen and divided per pixel
            let q = q(v);
            self.batch.extend([x.to_bits(), y.to_bits(), m.rd32(v + 0xc), m.rd32(v + 0x10), m.rd32(v + 0x14), m.rd32(v + 0x1c)]);
            let (s, t) = sow(v);
            self.batch.extend([s.to_bits(), t.to_bits(), q.to_bits()]);
            self.batch.extend(konst);
            self.batch.extend(mode);
            self.batch.extend(b);
            // ooz, oow for the depth buffer
            self.batch.extend([m.rd32(v + 0x18), m.rd32(v + 0x20)]);
        };
        if let (Some([n0, n1, n2, n3]), &[v0, v1]) = (quad, verts) {
            put(v0, n0);
            put(v0, n1);
            put(v1, n2);
            put(v1, n2);
            put(v0, n1);
            put(v1, n3);
        } else {
            for &v in verts {
                put(v, xy(v));
            }
        }
    }

    /// The corners of a line from `a` to `b` (in the vertices' space) drawn as a quad: a pixel
    /// wide at 720 lines (the laser sight: 1 pixel stayed hair-thin at high resolution), at
    /// least one; past the ends by half that, as GL draws lines.
    fn line_offsets(&self, a: (f32, f32), b: (f32, f32)) -> [(f32, f32); 4] {
        let ([_, _, vw, vh], _) = self.view_rect();
        // target pixels per unit of the vertices' space
        let (kx, ky) = (vw / self.space.0 as f32, vh / self.space.1 as f32);
        let width = (self.target.1 as f32 / 720.0).round().max(1.0);
        let (dx, dy) = ((b.0 - a.0) * kx, (b.1 - a.1) * ky);
        let len = (dx * dx + dy * dy).sqrt();
        let (ux, uy) = if len > 0.0 { (dx / len, dy / len) } else { (1.0, 0.0) };
        let h = width / 2.0;
        // along (u) and across (n = u turned) in pixels, back to the space's units
        let p = |o: (f32, f32), along: f32, across: f32| (o.0 + (ux * along - uy * across) / kx, o.1 + (uy * along + ux * across) / ky);
        [p(a, -h, -h), p(a, -h, h), p(b, h, -h), p(b, h, h)]
    }

    /// The picture so far at the Glide screen's size, rows from the top, as 0xRRGGBB.
    fn read_screen(&mut self, gl: &glow::Context) -> Vec<u32> {
        self.flush(gl);
        self.resolve(gl);
        let (tw, th) = self.target;
        let mut px = vec![0u8; (tw * th * 4) as usize];
        let fbo = self.color_fbo(gl);
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.read_pixels(0, 0, tw as i32, th as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut px)));
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        // nearest target pixel of each screen pixel, in the view's rectangle (GL rows from
        // the bottom)
        let ([vx, vy, vw, vh], _) = self.view_rect();
        let (w, h) = self.res;
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let ty = (vy + (y as f32 + 0.5) * vh / h as f32) as u32;
            let row = th - 1 - ty.min(th - 1);
            for x in 0..w {
                let tx = ((vx + (x as f32 + 0.5) * vw / w as f32) as u32).min(tw - 1);
                let p = ((row * tw + tx) * 4) as usize;
                out.push((px[p] as u32) << 16 | (px[p + 1] as u32) << 8 | px[p + 2] as u32);
            }
        }
        out
    }

    /// RGBA pixels (`w` x `h`, rows from the top) over the picture at screen position `x`, `y`
    /// (from the top); transparent ones are left out.
    fn draw_pixels(&mut self, gl: &glow::Context, (x, y): (i32, i32), (w, h): (u32, u32), rgba: &[u8]) {
        self.flush(gl);
        let fbo = self.draw_fbo(gl);
        let empty = self.local(gl).empty;
        let (sw, sh) = (self.res.0 as f32, self.res.1 as f32);
        let (prog, tex) = self.blit;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            self.scissor(gl, [0, 0, self.res.0 as i32, self.res.1 as i32]);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w as i32, h as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(rgba)));
            gl.use_program(Some(prog));
            gl.uniform_1_i32(gl.get_uniform_location(prog, "img").as_ref(), 0);
            let nx = |v: f32| v / sw * 2.0 - 1.0;
            let ny = |v: f32| 1.0 - v / sh * 2.0;
            let r = [nx(x as f32), ny((y + h as i32) as f32), nx((x + w as i32) as f32), ny(y as f32)];
            gl.uniform_4_f32(gl.get_uniform_location(prog, "rect").as_ref(), r[0], r[1], r[2], r[3]);
            gl.bind_vertex_array(Some(empty));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.disable(glow::SCISSOR_TEST);
        }
    }

    /// grLfbLock: the guest buffer of the Glide screen (read: the picture so far; write:
    /// `LFB_UNTOUCHED`, drawn where changed at grLfbUnlock).
    fn lfb_lock(&mut self, gl: &glow::Context, write: bool, mode: u32, lower: bool) -> u32 {
        static BUF: AtomicU32 = AtomicU32::new(0);
        let (w, h) = self.res;
        let mut buf = BUF.load(Ordering::Relaxed);
        if buf == 0 {
            // the largest Glide screen
            buf = super::alloc(LFB_STRIDE * 1200);
            BUF.store(buf, Ordering::Relaxed);
        }
        let m = mem();
        if write {
            for y in 0..h {
                for x in 0..w {
                    m.wr16(buf + y * LFB_STRIDE + 2 * x, LFB_UNTOUCHED);
                }
            }
        } else {
            let px = self.read_screen(gl);
            for y in 0..h {
                let row = if lower { h - 1 - y } else { y };
                for x in 0..w {
                    m.wr16(buf + row * LFB_STRIDE + 2 * x, to_565(px[(y * w + x) as usize]));
                }
            }
        }
        self.lfb = Some(Lfb { buf, write, mode, lower });
        buf
    }

    fn lfb_unlock(&mut self, gl: &glow::Context) {
        let Some(l) = self.lfb.take() else { return };
        if !l.write {
            return;
        }
        let (w, h) = self.res;
        let m = mem();
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let mut any = false;
        for y in 0..h {
            let row = if l.lower { h - 1 - y } else { y };
            for x in 0..w {
                let v = m.rd16(l.buf + row * LFB_STRIDE + 2 * x);
                if v != LFB_UNTOUCHED {
                    let p = ((y * w + x) * 4) as usize;
                    rgba[p..p + 4].copy_from_slice(&pixel16(v, l.mode));
                    any = true;
                }
            }
        }
        if any {
            self.draw_pixels(gl, (0, 0), (w, h), &rgba);
        }
    }

    /// Texels into the texture at TMU address `addr` (rows `rows` of its top level, or all).
    fn download(&mut self, gl: &glow::Context, addr: u32, lod: u32, aspect: u32, format: u32, data: u32, rows: Option<(u32, u32)>) {
        let (w, h) = lod_size(lod, aspect);
        let bpt = bytes_per_texel(format);
        let bytes = w * h * bpt;
        self.downloads += 1;
        let stamp = self.downloads;
        let (r0, r1) = rows.unwrap_or((0, h - 1));
        let r1 = r1.min(h - 1);
        // the same texels again: nothing to do (and no flush)
        let src: Option<&[u8]> = (r0 <= r1).then(|| &*mem().slice(data, ((r1 - r0 + 1) * w * bpt) as usize));
        let at = (r0 * w * bpt) as usize;
        if let (Some(t), Some(src)) = (self.textures.get_mut(&addr), src) {
            let known = t.known.get(r0 as usize..=r1 as usize).is_some_and(|k| k.iter().all(|&k| k));
            if (t.w, t.h, t.bytes, t.format) == (w, h, bytes, format) && known && t.raw[at..at + src.len()] == src[..] {
                t.stamp = stamp;
                return;
            }
        }
        match self.textures.get_mut(&addr) {
            Some(t) if (t.w, t.h, t.bytes) == (w, h, bytes) => {
                t.stamp = stamp;
                if t.format != format {
                    t.known.fill(false);
                    t.format = format;
                }
                t.layer
            }
            _ => {
                // what it overwrites in TMU memory is gone; then a free layer, or the one
                // downloaded longest ago
                self.textures.retain(|&a, t| a + t.bytes <= addr || addr + bytes <= a);
                if self.textures.len() as u32 >= self.layers && self.layers < self.max_layers {
                    self.grow(gl);
                }
                let layer = (0..self.layers).find(|l| self.textures.values().all(|t| t.layer != *l)).unwrap_or_else(|| {
                    let (&a, t) = self.textures.iter().min_by_key(|t| t.1.stamp).unwrap();
                    let l = t.layer;
                    self.textures.remove(&a);
                    l
                });
                self.textures.insert(addr, Texture { layer, w, h, bytes, stamp, hd: None, raw: vec![0; bytes as usize], known: vec![false; h as usize], format, staged: vec![0; (w * h * 4) as usize], staged_alpha: vec![0; (w * h * 2) as usize], pending: None });
                layer
            }
        };
        let Some(src) = src else { return };
        {
            let t = self.textures.get_mut(&addr).unwrap();
            t.raw[at..at + src.len()].copy_from_slice(src);
            t.known[r0 as usize..=r1 as usize].fill(true);
        }
        let n = ((r1 - r0 + 1) * w) as usize;
        let mut px = convert(format, data, n);
        let key = super::hd::hash(format, w, r1 - r0 + 1, mem().slice(data, n * bytes_per_texel(format) as usize));
        if let Some(x) = crate::game::game().glide() {
            x.recolour(key, &mut px);
        }
        // as a replacement has it: alpha 0 where keyed out
        let original: Vec<u8> = px.chunks(4).flat_map(|p| [p[0], p[1], p[2], if p[..3] == [0, 0, 0] { 0 } else { 255 }]).collect();
        super::hd::dump(key, w, r1 - r0 + 1, &original);
        let t = self.textures.get_mut(&addr).unwrap();
        let at = (r0 * w) as usize;
        let (rgb, alpha) = (&mut t.staged[at * 4..], &mut t.staged_alpha[at * 2..]);
        // premultiplied by coverage: a black texel is keyed out
        for (k, p) in px.chunks_exact(4).enumerate() {
            let covered = p[..3] != [0, 0, 0];
            rgb[4 * k..4 * k + 4].copy_from_slice(&if covered { [p[0], p[1], p[2], 255] } else { [0; 4] });
            alpha[2 * k..2 * k + 2].copy_from_slice(&if covered { [p[3], 0] } else { [0, p[3]] });
        }
        t.pending = Some(t.pending.map_or((r0, r1), |(a, b)| (a.min(r0), b.max(r1))));
        // the replacements read the original's layer: up at once
        if self.hd.is_some() {
            self.flush(gl);
            self.commit(gl, addr);
            self.replace(gl, addr, key, (r0, r1), &original);
        }
    }

    /// Twice the layers (all textures in use, TMU memory not full): new arrays, every texture
    /// uploaded again from its staged texels at its next draw.
    fn grow(&mut self, gl: &glow::Context) {
        self.flush(gl);
        let n = (self.layers * 2).min(self.max_layers);
        log!("glide: texture array grows to {n} layers");
        unsafe {
            gl.delete_texture(self.array);
            gl.delete_texture(self.alpha);
            self.array = layer_array(gl, glow::RGBA8, n);
            self.alpha = layer_array(gl, glow::RG8, n);
        }
        self.layers = n;
        // the filter is set on the new arrays at the next draw
        self.aniso = 0.0;
        for t in self.textures.values_mut() {
            t.pending = Some((0, t.h - 1));
        }
    }

    /// Upload the rows of the texture at `addr` downloaded since its last upload into its
    /// layer, what was batched before drawn first (with the old texels).
    fn commit(&mut self, gl: &glow::Context, addr: u32) {
        let Some(t) = self.textures.get_mut(&addr) else { return };
        let Some((r0, r1)) = t.pending else { return };
        if !self.batch.is_empty() {
            self.flush(gl);
        }
        let t = self.textures.get_mut(&addr).unwrap();
        t.pending = None;
        let (w, h, layer) = (t.w as usize, t.h as usize, t.layer);
        let rows = r0 as usize * w..(r1 as usize + 1) * w;
        unsafe {
            for (tex, format, px) in [(self.array, glow::RGBA, &t.staged[rows.start * 4..rows.end * 4]), (self.alpha, glow::RG, &t.staged_alpha[rows.start * 2..rows.end * 2])] {
                gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(tex));
                gl.tex_sub_image_3d(
                    glow::TEXTURE_2D_ARRAY,
                    0,
                    0,
                    r0 as i32,
                    layer as i32,
                    w as i32,
                    (r1 - r0 + 1) as i32,
                    1,
                    format,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(px)),
                );
            }
            // its mipmaps, from the whole texture (the GPU's generate_mipmap would build every
            // layer's: with hundreds of textures downloaded at a mission's start that took the
            // driver down)
            for (tex, format, ch, px) in [(self.array, glow::RGBA, 4, &t.staged), (self.alpha, glow::RG, 2, &t.staged_alpha)] {
                gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(tex));
                let (mut px, mut lw, mut lh) = (px[..w * h * ch].to_vec(), w, h);
                for level in 1..LEVELS {
                    if lw == 1 && lh == 1 {
                        break;
                    }
                    (px, lw, lh) = half(&px, lw, lh, ch);
                    gl.tex_sub_image_3d(
                        glow::TEXTURE_2D_ARRAY,
                        level,
                        0,
                        0,
                        layer as i32,
                        lw as i32,
                        lh as i32,
                        1,
                        format,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(&px)),
                    );
                }
            }
        }
    }

    /// After a download of rows `r0..=r1` (known by `key`) into the texture at `addr`: its
    /// replacement into the replacements' layer, or the original scaled up when other parts of
    /// the page are replaced. A whole page without a replacement gives its layer up.
    fn replace(&mut self, gl: &glow::Context, addr: u32, key: u64, rows: (u32, u32), original: &[u8]) {
        if self.hd.is_none() {
            return;
        }
        let fbos = self.local(gl).copy;
        let Some(hd) = &mut self.hd else { return };
        let t = &self.textures[&addr];
        let (layer, w, h) = (t.layer, t.w, t.h);
        let img = super::hd::find(key, w, rows.1 - rows.0 + 1, original);
        let whole = rows == (0, h - 1);
        let hd_layer = match (t.hd, &img) {
            (Some(l), Some(_)) => l,
            (Some(l), None) if !whole => l,
            (Some(_), None) => {
                self.textures.get_mut(&addr).unwrap().hd = None;
                return;
            }
            (None, None) => return,
            (None, Some(_)) => {
                // a free layer, or the one of the texture downloaded longest ago
                let l = (0..HD_LAYERS).find(|l| self.textures.values().all(|t| t.hd != Some(*l))).unwrap_or_else(|| {
                    let t = self.textures.values_mut().filter(|t| t.hd.is_some()).min_by_key(|t| t.stamp).unwrap();
                    t.hd.take().unwrap()
                });
                self.textures.get_mut(&addr).unwrap().hd = Some(l);
                // the rest of the page as it is
                if !whole {
                    hd.scale_up(gl, fbos, self.array, layer, l, w, (0, h - 1));
                }
                l
            }
        };
        match img {
            Some(img) => hd.put(gl, hd_layer, rows.0, &img),
            None => hd.scale_up(gl, fbos, self.array, layer, hd_layer, w, rows),
        }
    }
}

// ---- the Glide functions ---------------------------------------------------------------------

fn ret1(_: &mut Cpu) -> u32 {
    1
}
fn ret0(_: &mut Cpu) -> u32 {
    0
}

fn sst_query_boards(c: &mut Cpu) -> u32 {
    mem().wr32(arg(c, 0), 1);
    1
}

fn sst_query_hardware(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    let m = mem();
    m.slice(p, 0x40).fill(0);
    // one Voodoo with 6 MB frame buffer memory (the game offers up to 1280x960 for it)
    for (off, v) in [(0, 1), (4, 0), (8, 6), (0xc, 2), (0x10, 1), (0x14, 0), (0x18, 1), (0x1c, TMU_RAM >> 20)] {
        m.wr32(p + off, v);
    }
    1
}

fn sst_win_open(c: &mut Cpu) -> u32 {
    let res = arg(c, 1);
    let (w, h) = match res {
        0 => (320, 200),
        1 => (320, 240),
        2 => (400, 256),
        3 => (512, 384),
        4 => (640, 200),
        5 => (640, 350),
        6 => (640, 400),
        7 => (640, 480),
        8 => (800, 600),
        9 => (960, 720),
        0xa => (856, 480),
        0xb => (512, 256),
        0xc => (1024, 768),
        0xd => (1280, 1024),
        0xe => (1600, 1200),
        _ => (640, 480),
    };
    log!("glide: grSstWinOpen {w}x{h}");
    super::window::set_screen(Some((w, h)));
    if !super::window::gl_here() {
        return 0;
    }
    let mut g = super::window::with_gl(|gl| {
        let mut g = Glide::open(gl, w, h);
        let fbo = g.color_fbo(gl);
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        g
    });
    if let Some(g) = &mut g {
        let (ww, wh) = super::display::render_size();
        let a = aspect(Some(ww as f32 / wh.max(1) as f32));
        super::window::with_gl(|gl| g.resize(gl, a, wh));
    }
    let ok = g.is_some();
    *GLIDE.lock().unwrap_or_else(|e| e.into_inner()) = g;
    ok as u32
}

fn sst_win_close(_: &mut Cpu) -> u32 {
    super::window::set_screen(None);
    with(|g, gl| unsafe {
        // this thread's objects; other threads' are left (their contexts are not current here)
        if let Some(l) = g.locals.remove(&thread_id()) {
            l.delete(gl);
        }
        if let Some((rb, _, _, depth)) = g.ms.take() {
            gl.delete_renderbuffer(rb);
            gl.delete_renderbuffer(depth);
        }
        gl.delete_renderbuffer(g.depth_rb);
        gl.delete_program(g.blit.0);
        gl.delete_texture(g.blit.1);
        gl.delete_texture(g.color);
        gl.delete_texture(g.array);
        gl.delete_texture(g.alpha);
    });
    *GLIDE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    aspect(None);
    1
}


fn buffer_clear(c: &mut Cpu) -> u32 {
    let (color, depth) = (arg(c, 0), arg(c, 2) & 0xffff);
    with(|g, gl| {
        g.flush(gl);
        let fbo = g.draw_fbo(gl);
        let s = g.state;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            g.scissor(gl, s.clip);
            let ch = |sh: u32| (color >> sh & 0xff) as f32 / 255.0;
            gl.clear_color(ch(16), ch(8), ch(0), 1.0);
            gl.color_mask(s.color_mask.0, s.color_mask.0, s.color_mask.0, s.color_mask.1);
            let mut bits = glow::COLOR_BUFFER_BIT;
            // the depth buffer when it is on and written: z as it is, w turned round (0xffff
            // the farthest)
            if s.depth_mode != 0 && s.depth_mask {
                let d = depth as f32 / 65535.0;
                gl.clear_depth_f32(if matches!(s.depth_mode, 2 | 4) { 1.0 - d } else { d });
                gl.depth_mask(true);
                bits |= glow::DEPTH_BUFFER_BIT;
            }
            gl.clear(bits);
            gl.color_mask(true, true, true, true);
            gl.disable(glow::SCISSOR_TEST);
        }
    });
    0
}

fn cull_mode(c: &mut Cpu) -> u32 {
    let mode = arg(c, 0);
    with(|g, _| g.cull = mode);
    0
}

/// grSstOrigin(GR_ORIGIN_UPPER_LEFT 0, LOWER_LEFT 1).
fn sst_origin(c: &mut Cpu) -> u32 {
    let lower = arg(c, 0) == 1;
    with(|g, _| g.lower_left = lower);
    0
}

fn depth_buffer_mode(c: &mut Cpu) -> u32 {
    let v = arg(c, 0);
    with(|g, gl| g.set(gl, |s| s.depth_mode = v));
    0
}

fn depth_buffer_function(c: &mut Cpu) -> u32 {
    let v = arg(c, 0);
    with(|g, gl| g.set(gl, |s| s.depth_func = v));
    0
}

fn depth_mask(c: &mut Cpu) -> u32 {
    let v = arg(c, 0) != 0;
    with(|g, gl| g.set(gl, |s| s.depth_mask = v));
    0
}

fn depth_bias_level(c: &mut Cpu) -> u32 {
    let v = arg(c, 0) as i16 as i32;
    with(|g, gl| g.set(gl, |s| s.depth_bias = v));
    0
}

fn fog_mode(c: &mut Cpu) -> u32 {
    let v = arg(c, 0);
    if v & 0xff > 1 {
        log!("warn: grFogMode({v:#x}): only iterated alpha fog is done");
    }
    with(|g, gl| g.set(gl, |s| s.fog = v));
    0
}

fn fog_color_value(c: &mut Cpu) -> u32 {
    let v = arg(c, 0);
    with(|g, gl| g.set(gl, |s| s.fog_color = v));
    0
}

fn color_mask(c: &mut Cpu) -> u32 {
    let v = (arg(c, 0) != 0, arg(c, 1) != 0);
    with(|g, gl| g.set(gl, |s| s.color_mask = v));
    0
}

/// grGlideGetState(GrState*): our state, kept by the address of the game's GrState.
fn glide_get_state(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    with(|g, _| {
        let s = Saved { state: g.state, cc: g.cc, ac: g.ac, constant: g.constant, tex: g.tex, tex_alpha_zero: g.tex_alpha_zero, alpha_test: g.alpha_test, cull: g.cull, lower_left: g.lower_left };
        g.saved.insert(p, s);
    });
    0
}

fn glide_set_state(c: &mut Cpu) -> u32 {
    let p = arg(c, 0);
    with(|g, gl| {
        let Some(s) = g.saved.get(&p).copied() else { return };
        g.set(gl, |st| *st = s.state);
        (g.cc, g.ac, g.constant, g.tex, g.tex_alpha_zero, g.alpha_test, g.cull, g.lower_left) = (s.cc, s.ac, s.constant, s.tex, s.tex_alpha_zero, s.alpha_test, s.cull, s.lower_left);
    });
    0
}

/// grLfbLock(type, buffer, write mode, origin, pixel pipeline, GrLfbInfo*): type bit 0 is
/// write. GrLfbInfo: size, pointer, stride, write mode, origin.
fn lfb_lock(c: &mut Cpu) -> u32 {
    let (ty, mode, origin, info) = (arg(c, 0), arg(c, 2), arg(c, 3), arg(c, 5));
    let write = ty & 1 != 0;
    let mode = if mode == 0xff { 0 } else { mode };
    if write && mode > 2 {
        log!("warn: grLfbLock write mode {mode:#x} not supported");
    }
    let lower = origin == 1;
    let Some(buf) = with(|g, gl| g.lfb_lock(gl, write, mode, lower)) else { return 0 };
    let m = mem();
    m.wr32(info + 4, buf);
    m.wr32(info + 8, LFB_STRIDE);
    m.wr32(info + 12, mode);
    m.wr32(info + 16, origin);
    1
}

fn lfb_unlock(_: &mut Cpu) -> u32 {
    with(|g, gl| g.lfb_unlock(gl));
    1
}

/// grLfbWriteRegion(buffer, x, y, source format, w, h, stride, data): source formats 565 (0),
/// 555 (1), 1555 (2), 888 (4), 8888 (5).
fn lfb_write_region(c: &mut Cpu) -> u32 {
    let (x, y, fmt, w, h, stride, data) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 6), arg(c, 7));
    let m = mem();
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for r in 0..h {
        let row = data + r * stride;
        for k in 0..w {
            rgba.extend(match fmt {
                0..=2 => pixel16(m.rd16(row + 2 * k), fmt),
                4 | 5 => {
                    let v = m.rd32(row + 4 * k);
                    [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]
                }
                _ => [255, 0, 255, 255],
            });
        }
    }
    with(|g, gl| {
        let y = if g.lower_left { g.res.1 as i32 - (y + h) as i32 } else { y as i32 };
        let rgba = if g.lower_left { rgba.chunks((w * 4) as usize).rev().flatten().copied().collect() } else { rgba };
        g.draw_pixels(gl, (x as i32, y), (w, h), &rgba);
    });
    1
}

/// grLfbReadRegion(buffer, x, y, w, h, stride, data): the picture so far as 565.
fn lfb_read_region(c: &mut Cpu) -> u32 {
    let (x, y, w, h, stride, data) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 6));
    with(|g, gl| {
        let px = g.read_screen(gl);
        let (sw, sh) = g.res;
        let m = mem();
        for r in 0..h {
            let sy = if g.lower_left { sh.saturating_sub(1 + y + r) } else { y + r };
            for k in 0..w {
                let (sx, sy) = ((x + k).min(sw - 1), sy.min(sh - 1));
                m.wr16(data + r * stride + 2 * k, to_565(px[(sy * sw + sx) as usize]));
            }
        }
    })
    .is_some() as u32
}

/// grDrawPolygonVertexList(n, GrVertex[n]): a convex polygon, as a fan of triangles.
fn draw_polygon_vertex_list(c: &mut Cpu) -> u32 {
    let (n, list) = (arg(c, 0), arg(c, 1));
    // GrVertex: x y z r g b ooz a oow, two TMUs' sow tow oow
    let v = |k: u32| list + 0x3c * k;
    with(|g, gl| {
        for k in 1..n.saturating_sub(1) {
            g.add(gl, glow::TRIANGLES, &[v(0), v(k), v(k + 1)]);
        }
    });
    0
}

fn buffer_swap(_: &mut Cpu) -> u32 {
    super::sample::start();
    let shown = with(|g, gl| {
        g.flush(gl);
        g.resolve(gl);
        g.swaps += 1;
        (g.color, g.target, crate::game::game().glide().map_or(g.gamma, |x| x.gamma(g.gamma)))
    });
    if let Some((tex, (w, h), gamma)) = shown {
        super::window::show_texture(tex, w, h, gamma);
        if let Some(x) = crate::game::game().glide() {
            x.frame_shown();
        }
        // the next frame follows the window
        let (ww, wh) = super::display::render_size();
        let a = aspect(Some(ww as f32 / wh.max(1) as f32));
        with(|g, gl| g.resize(gl, a, wh));
    }
    0
}

/// The coordinate space of the vertices that follow (None: screen pixels).
pub fn set_space(space: Option<(u32, u32)>) {
    with(|g, gl| {
        let s = space.unwrap_or(g.res);
        if s != g.space {
            g.flush(gl);
            g.space = s;
        }
    });
}

/// Where the vertices that follow go in the frame.
pub fn set_view(view: View) {
    with(|g, gl| {
        if view != g.view {
            g.flush(gl);
            g.view = view;
        }
    });
}

fn draw_triangle(c: &mut Cpu) -> u32 {
    let v = [arg(c, 0), arg(c, 1), arg(c, 2)];
    with(|g, gl| g.add(gl, glow::TRIANGLES, &v));
    0
}

fn draw_line(c: &mut Cpu) -> u32 {
    let v = [arg(c, 0), arg(c, 1)];
    with(|g, gl| g.add(gl, glow::TRIANGLES, &v));
    0
}

fn draw_point(c: &mut Cpu) -> u32 {
    let v = [arg(c, 0)];
    with(|g, gl| g.add(gl, glow::POINTS, &v));
    0
}

fn clip_window(c: &mut Cpu) -> u32 {
    let r = [arg(c, 0) as i32, arg(c, 1) as i32, arg(c, 2) as i32, arg(c, 3) as i32];
    with(|g, gl| {
        // kept from the top (with the origin at the lower left, y counts up)
        let h = g.res.1 as i32;
        let r = if g.lower_left { [r[0], h - r[3], r[2], h - r[1]] } else { r };
        g.set(gl, |s| s.clip = r)
    });
    0
}

fn combine(c: &Cpu) -> [u32; 5] {
    [arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4)]
}

fn color_combine(c: &mut Cpu) -> u32 {
    let v = combine(c);
    with(|g, _| g.cc = v);
    0
}

fn alpha_combine(c: &mut Cpu) -> u32 {
    let v = combine(c);
    with(|g, _| g.ac = v);
    0
}

fn tex_combine(c: &mut Cpu) -> u32 {
    // grTexCombine(tmu, rgb function, factor, alpha function, factor, rgb invert, alpha invert):
    // one TMU, so LOCAL passes the texture through and ZERO gives 0 (Recoil: rgb LOCAL, alpha
    // ZERO, for every texture). Other functions need a second TMU: taken as LOCAL.
    let (rgb, alpha) = (arg(c, 1), arg(c, 3));
    if rgb != 1 || alpha > 1 {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| log!("warn: grTexCombine({rgb}, {alpha}) taken as LOCAL"));
    }
    with(|g, _| g.tex_alpha_zero = alpha == 0);
    0
}

fn constant_color(c: &mut Cpu) -> u32 {
    let v = arg(c, 0);
    with(|g, _| g.constant = v);
    0
}

fn alpha_blend(c: &mut Cpu) -> u32 {
    let b = (arg(c, 0), arg(c, 1));
    with(|g, gl| g.set(gl, |s| s.blend = b));
    0
}

/// grAlphaTestFunction: no flush, the vertices carry it.
fn alpha_test_function(c: &mut Cpu) -> u32 {
    let f = arg(c, 0) & 7;
    with(|g, _| g.alpha_test.0 = f);
    0
}

fn alpha_test_reference(c: &mut Cpu) -> u32 {
    let v = arg(c, 0) & 0xff;
    with(|g, _| g.alpha_test.1 = v);
    0
}

fn chromakey_mode(c: &mut Cpu) -> u32 {
    let on = arg(c, 0) != 0;
    with(|g, gl| g.set(gl, |s| s.chroma = on));
    0
}

fn chromakey_value(c: &mut Cpu) -> u32 {
    let k = arg(c, 0);
    with(|g, gl| g.set(gl, |s| s.key = k));
    0
}

fn tex_filter_mode(c: &mut Cpu) -> u32 {
    let linear = arg(c, 2) != 0;
    with(|g, gl| g.set(gl, |s| s.linear = linear));
    0
}

fn tex_clamp_mode(c: &mut Cpu) -> u32 {
    let cl = (arg(c, 1) != 0, arg(c, 2) != 0);
    with(|g, gl| g.set(gl, |s| s.clamp = cl));
    0
}

fn gamma_value(c: &mut Cpu) -> u32 {
    set_gamma(f32_arg(c, 0));
    0
}

/// grGammaCorrectionValue; Preferences' OK too (the game sets it only when Glide starts).
pub fn set_gamma(v: f32) {
    with(|g, _| g.gamma = if v > 0.0 { v } else { 1.0 });
}

fn tex_min_address(_: &mut Cpu) -> u32 {
    0
}

fn tex_max_address(_: &mut Cpu) -> u32 {
    // the last start address a 256x256 16-bit texture fits at
    TMU_RAM - 0x2_0000
}

/// GrTexInfo: smallLod, largeLod, aspect, format, data
fn tex_info(p: u32) -> (u32, u32, u32, u32, u32) {
    let m = mem();
    (m.rd32(p), m.rd32(p + 4), m.rd32(p + 8), m.rd32(p + 12), m.rd32(p + 16))
}

fn tex_mem_required(info: (u32, u32, u32, u32, u32)) -> u32 {
    let (small, large, aspect, format, _) = info;
    (large..=small.max(large)).map(|l| {
        let (w, h) = lod_size(l, aspect);
        w * h * bytes_per_texel(format)
    }).sum::<u32>().next_multiple_of(8)
}

fn tex_texture_mem_required(c: &mut Cpu) -> u32 {
    tex_mem_required(tex_info(arg(c, 1)))
}

fn tex_calc_mem_required(c: &mut Cpu) -> u32 {
    tex_mem_required((arg(c, 0), arg(c, 1), arg(c, 2), arg(c, 3), 0))
}

fn tex_download_mipmap(c: &mut Cpu) -> u32 {
    let (addr, info) = (arg(c, 1), tex_info(arg(c, 3)));
    let (_, large, aspect, format, data) = info;
    with(|g, gl| g.download(gl, addr, large, aspect, format, data, None));
    0
}

fn tex_download_mipmap_level(c: &mut Cpu) -> u32 {
    let (addr, this, large, aspect, format, data) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 7));
    if this == large {
        with(|g, gl| g.download(gl, addr, this, aspect, format, data, None));
    }
    0
}

fn tex_download_mipmap_level_partial(c: &mut Cpu) -> u32 {
    let (addr, this, large, aspect, format, data, r0, r1) = (arg(c, 1), arg(c, 2), arg(c, 3), arg(c, 4), arg(c, 5), arg(c, 7), arg(c, 8), arg(c, 9));
    if this == large {
        // `data` points at the first row downloaded
        with(|g, gl| g.download(gl, addr, this, aspect, format, data, Some((r0, r1))));
    }
    0
}

fn tex_source(c: &mut Cpu) -> u32 {
    let addr = arg(c, 1);
    with(|g, _| g.tex = Some(addr));
    0
}

fn sst_screen_width(c: &mut Cpu) -> u32 {
    let w = with(|g, _| g.res.0).unwrap_or(640);
    c.r[EDX] = 0;
    w
}

fn sst_screen_height(_: &mut Cpu) -> u32 {
    with(|g, _| g.res.1).unwrap_or(480)
}

/// The Glide functions we implement; anything else the game asks for gets `not_implemented`.
const FUNCTIONS: &[(&str, fn(&mut Cpu) -> u32)] = &[
    ("grGlideInit", ret0),
    ("grGlideShutdown", ret0),
    ("grSstQueryBoards", sst_query_boards),
    ("grSstQueryHardware", sst_query_hardware),
    ("grSstSelect", ret0),
    ("grSstWinOpen", sst_win_open),
    ("grSstWinClose", sst_win_close),
    ("grSstIdle", ret0),
    ("grSstControl", ret1),
    ("grSstScreenWidth", sst_screen_width),
    ("grSstScreenHeight", sst_screen_height),
    ("grBufferClear", buffer_clear),
    ("grBufferSwap", buffer_swap),
    ("grDrawTriangle", draw_triangle),
    ("guDrawTriangleWithClip", draw_triangle),
    ("grDrawLine", draw_line),
    ("grDrawPoint", draw_point),
    ("grClipWindow", clip_window),
    ("grColorCombine", color_combine),
    ("grAlphaCombine", alpha_combine),
    ("grTexCombine", tex_combine),
    ("grConstantColorValue", constant_color),
    ("grAlphaBlendFunction", alpha_blend),
    ("grAlphaTestFunction", alpha_test_function),
    ("grAlphaTestReferenceValue", alpha_test_reference),
    ("grChromakeyMode", chromakey_mode),
    ("grChromakeyValue", chromakey_value),
    ("grTexFilterMode", tex_filter_mode),
    ("grTexClampMode", tex_clamp_mode),
    ("grTexMipMapMode", ret0),
    ("grDepthBufferMode", depth_buffer_mode),
    ("grDepthBufferFunction", depth_buffer_function),
    ("grDepthMask", depth_mask),
    ("grDepthBiasLevel", depth_bias_level),
    ("grCullMode", cull_mode),
    ("grSstOrigin", sst_origin),
    ("grFogMode", fog_mode),
    ("grFogColorValue", fog_color_value),
    ("grColorMask", color_mask),
    ("grRenderBuffer", ret0),
    ("grGlideGetState", glide_get_state),
    ("grGlideSetState", glide_set_state),
    ("grDrawPolygonVertexList", draw_polygon_vertex_list),
    ("grLfbLock", lfb_lock),
    ("grLfbUnlock", lfb_unlock),
    ("grLfbWriteRegion", lfb_write_region),
    ("grLfbReadRegion", lfb_read_region),
    ("grDitherMode", ret0),
    ("grGammaCorrectionValue", gamma_value),
    ("grTexMinAddress", tex_min_address),
    ("grTexMaxAddress", tex_max_address),
    ("grTexTextureMemRequired", tex_texture_mem_required),
    ("grTexCalcMemRequired", tex_calc_mem_required),
    ("grTexDownloadMipMap", tex_download_mipmap),
    ("grTexDownloadMipMapLevel", tex_download_mipmap_level),
    ("grTexDownloadMipMapLevelPartial", tex_download_mipmap_level_partial),
    ("grTexSource", tex_source),
];

/// GetProcAddress on glide2x.dll: a thunk for the name (`_grName@argbytes`).
pub fn proc_address(name: &str) -> u32 {
    let plain = name.trim_start_matches('_');
    let (base, bytes) = plain.split_once('@').map_or((plain, 0), |(b, n)| (b, n.parse().unwrap_or(0)));
    let name = format!("glide2x!{base}");
    match FUNCTIONS.iter().find(|e| e.0 == base) {
        Some(&(_, f)) => thunk(&name, (bytes / 4) as i32, f),
        None => thunk_not_implemented(&name, (bytes / 4) as i32),
    }
}

#[cfg(test)]
mod tests {
    use super::half;

    #[test]
    fn half_averages_boxes() {
        // 2x2, one byte a pixel: one pixel, the average (rounded)
        assert_eq!(half(&[0, 10, 20, 31], 2, 2, 1), (vec![15], 1, 1));
        // 4x1: a side of 1 stays 1, pairs averaged
        assert_eq!(half(&[0, 2, 4, 8], 4, 1, 1), (vec![1, 6], 2, 1));
        // two bytes a pixel: each averaged on its own
        assert_eq!(half(&[0, 100, 2, 100, 4, 100, 6, 100], 2, 2, 2), (vec![3, 100], 1, 1));
    }
}
