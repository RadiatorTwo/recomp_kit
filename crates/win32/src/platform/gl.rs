//! OpenGL: the context of the game window and what it shows. A shown picture is either a 2D
//! frame (DirectDraw: menus, the software renderer) uploaded as a texture, or a texture
//! something else rendered (the Glide scene); a video and the dialog overlay go on top.

use glow::HasContext;
use glutin::context::{ContextApi, ContextAttributesBuilder, GlProfile, NotCurrentContext, NotCurrentGlContext, PossiblyCurrentContext, Version};
use glutin::config::GlConfig;
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SwapInterval, WindowSurface};
use glutin_winit::GlWindow;
use raw_window_handle::HasWindowHandle;
use std::num::NonZeroU32;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

/// GLSL for desktop OpenGL 3.3 core.
pub const GLSL: &str = "#version 330 core\n";

pub struct Gl {
    pub gl: glow::Context,
    /// boxed: the present thread's shared context draws to it too (`share`)
    surface: Box<Surface<WindowSurface>>,
    context: PossiblyCurrentContext,
    config: glutin::config::Config,
    raw: Option<raw_window_handle::RawWindowHandle>,
    blit: glow::Program,
    vao: glow::VertexArray,
    frame: glow::Texture,
    overlay: glow::Texture,
    has_overlay: bool,
    video: glow::Texture,
    /// where the video is shown: x, y, width, height as parts of the 4:3 picture
    video_size: Option<(u32, u32)>,
    /// what is shown: a texture, its size, whether its pixels are 0RGB/ARGB words (2D frames:
    /// the bytes are B G R A), and the gamma to show it with
    pub shown: (glow::Texture, u32, u32, bool, f32),
    /// the swap interval set: 1 (vsync) or 0
    vsync: bool,
    /// the framebuffers to copy pictures for the present thread through (`stage`)
    pub copy_fbo: [glow::Framebuffer; 2],
}

thread_local! {
    static UNIFORMS: std::cell::RefCell<std::collections::HashMap<(glow::Program, &'static str), Option<glow::UniformLocation>>> = Default::default();
}

/// A uniform's location in program `p`, asked of the driver once (asking for each in every
/// picture cost the effects' passes ~0.2 ms).
pub fn uniform(gl: &glow::Context, p: glow::Program, name: &'static str) -> Option<glow::UniformLocation> {
    UNIFORMS.with_borrow_mut(|u| u.entry((p, name)).or_insert_with(|| unsafe { gl.get_uniform_location(p, name) }).clone())
}

/// Deletes program `p` and forgets its uniforms (a new program may get its number).
pub fn delete_program(gl: &glow::Context, p: glow::Program) {
    UNIFORMS.with_borrow_mut(|u| u.retain(|k, _| k.0 != p));
    unsafe { gl.delete_program(p) };
}

pub fn compile(gl: &glow::Context, vs: &str, fs: &str) -> glow::Program {
    unsafe {
        let p = gl.create_program().expect("GL program");
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let s = gl.create_shader(kind).expect("GL shader");
            gl.shader_source(s, &format!("{GLSL}{src}"));
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                panic!("shader: {}", gl.get_shader_info_log(s));
            }
            gl.attach_shader(p, s);
        }
        gl.link_program(p);
        if !gl.get_program_link_status(p) {
            panic!("shader program: {}", gl.get_program_info_log(p));
        }
        p
    }
}

/// A texture with nearest or linear filtering, clamped.
pub fn texture(gl: &glow::Context, linear: bool) -> glow::Texture {
    unsafe {
        let t = gl.create_texture().expect("GL texture");
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        let f = if linear { glow::LINEAR } else { glow::NEAREST } as i32;
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, f);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, f);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        t
    }
}

/// Upload 32-bit words (as their little-endian bytes, RGBA order) to a texture.
pub fn upload(gl: &glow::Context, t: glow::Texture, px: &[u32], w: u32, h: u32) {
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        let bytes = std::slice::from_raw_parts(px.as_ptr() as *const u8, px.len() * 4);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(bytes)),
        );
    }
}

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
uniform sampler2D tex;
uniform int words;
uniform int flip;
uniform float gamma;
in vec2 uv;
out vec4 color;
void main() {
    vec4 c = texture(tex, flip != 0 ? vec2(uv.x, 1.0 - uv.y) : uv);
    // 2D frames hold 0RGB/ARGB words: their bytes are B G R A
    if (words != 0) c = c.bgra;
    color = vec4(pow(c.rgb, vec3(1.0 / gamma)), c.a);
}
"#;

impl Gl {
    /// Create the window with an OpenGL 3.3 context.
    pub fn new(el: &ActiveEventLoop, attrs: WindowAttributes) -> (Window, Gl) {
        let template = glutin::config::ConfigTemplateBuilder::new().with_alpha_size(8);
        let (win, config) = glutin_winit::DisplayBuilder::new()
            .with_window_attributes(Some(attrs))
            .build(el, template, |configs| configs.reduce(|a, b| if b.num_samples() < a.num_samples() { b } else { a }).unwrap())
            .expect("cannot create an OpenGL window");
        let win = win.expect("no window");
        let display = config.display();
        let handle = win.window_handle().ok().map(|h| h.as_raw());
        let attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
            .with_profile(GlProfile::Core)
            .build(handle);
        let context = unsafe { display.create_context(&config, &attrs) }.expect("cannot create an OpenGL 3.3 context");
        let sattrs = win.build_surface_attributes(Default::default()).expect("surface attributes");
        let surface = Box::new(unsafe { display.create_window_surface(&config, &sattrs) }.expect("cannot create the GL surface"));
        let context = context.make_current(&*surface).expect("cannot make the GL context current");
        // the game paces itself
        let _ = surface.set_swap_interval(&context, SwapInterval::DontWait);
        let gl = unsafe { glow::Context::from_loader_function_cstr(|s| display.get_proc_address(s)) };
        let blit = compile(&gl, BLIT_VS, BLIT_FS);
        let vao = unsafe { gl.create_vertex_array() }.expect("GL vertex array");
        let frame = texture(&gl, false);
        upload(&gl, frame, &vec![0; 640 * 480], 640, 480);
        let overlay = texture(&gl, true);
        let video = texture(&gl, true);
        let staged = [(texture(&gl, true), 0, 0), (texture(&gl, true), 0, 0)];
        let copy_fbo = [unsafe { gl.create_framebuffer() }.expect("GL framebuffer"), unsafe { gl.create_framebuffer() }.expect("GL framebuffer")];
        let g = Gl {
            gl,
            surface,
            context,
            config,
            raw: handle,
            blit,
            vao,
            frame,
            overlay,
            has_overlay: false,
            video,
            video_size: None,
            shown: (frame, 640, 480, true, 1.0),
            vsync: false,
            copy_fbo,
        };
        *STAGED.lock().unwrap() = Some((staged, 0));
        g.parts();
        (win, g)
    }

    /// The look's parts for other threads (`PARTS`).
    fn parts(&self) {
        *PARTS.lock().unwrap() = Some((self.blit, self.has_overlay.then_some(self.overlay), self.video_size.map(|(vw, vh)| (self.video, vw, vh))));
    }

    /// Show a 2D frame of 0RGB pixels.
    pub fn frame(&mut self, px: &[u32], w: u32, h: u32) {
        upload(&self.gl, self.frame, px, w, h);
        self.shown = (self.frame, w, h, true, 1.0);
    }

    pub fn overlay(&mut self, o: Option<(&[u32], u32, u32)>) {
        self.has_overlay = o.is_some();
        if let Some((px, w, h)) = o {
            upload(&self.gl, self.overlay, px, w, h);
            // the present thread may draw it next, with its context
            unsafe { self.gl.flush() };
        }
        self.parts();
    }

    /// Show a video frame (0RGB, w x h), or no video.
    pub fn video(&mut self, v: Option<(&[u32], u32, u32)>) {
        self.video_size = v.map(|v| (v.1, v.2));
        if let Some((px, w, h)) = v {
            upload(&self.gl, self.video, px, w, h);
            unsafe { self.gl.flush() };
        }
        self.parts();
    }

    pub fn resize(&self, w: u32, h: u32) {
        super::window::present_wait();
        let _p = PAINT.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some(w), Some(h)) = (NonZeroU32::new(w), NonZeroU32::new(h)) {
            self.surface.resize(&self.context, w, h);
        }
    }

    /// What the window shows, in a window of w x h.
    pub fn look(&self, w: u32, h: u32) -> Look {
        Look {
            blit: self.blit,
            shown: self.shown,
            overlay: self.has_overlay.then_some(self.overlay),
            video: self.video_size.map(|(vw, vh)| (self.video, vw, vh)),
            size: (w, h),
        }
    }

    /// Draw the shown picture and the overlay into the window (w x h), letterboxed; with `shot`,
    /// read back the picture's area as 0RGB pixels.
    pub fn draw(&mut self, w: u32, h: u32, shot: bool) -> Option<(Vec<u32>, u32, u32)> {
        // the present thread may be drawing the window
        super::window::present_wait();
        let _p = PAINT.lock().unwrap_or_else(|e| e.into_inner());
        let out = paint(&self.gl, self.vao, &self.look(w, h), shot);
        let vsync = VSYNC.load(std::sync::atomic::Ordering::Relaxed);
        if vsync != self.vsync {
            self.vsync = vsync;
            let interval = if vsync { SwapInterval::Wait(NonZeroU32::MIN) } else { SwapInterval::DontWait };
            let _ = self.surface.set_swap_interval(&self.context, interval);
        }
        let _ = self.surface.swap_buffers(&self.context);
        out
    }

    /// A context sharing this one's objects (textures, fences), for the present thread or a
    /// game thread drawing Glide.
    pub fn share(&self) -> SharedParts {
        let attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
            .with_profile(GlProfile::Core)
            .with_sharing(&self.context)
            .build(self.raw);
        let display = self.config.display();
        let context = unsafe { display.create_context(&self.config, &attrs) }.expect("cannot create a shared OpenGL context");
        SharedParts { context, surface: &*self.surface, display }
    }
}

/// The two copies of rendered pictures for the present thread (texture, size; textures are
/// shared by all contexts), and the one copied into last. Made with the window's context.
static STAGED: std::sync::Mutex<Option<([(glow::Texture, u32, u32); 2], usize)>> = std::sync::Mutex::new(None);

/// A copy of the rendered texture `tex` (w x h) for the present thread, in one of two
/// textures taking turns, through this thread's framebuffers `fbos`: the game draws on into
/// its own meanwhile (it may draw only what changes). Waits on the GPU until the present
/// thread has drawn the copy made into the same one two pictures ago (`DONE`). Returns the
/// copy, a fence after it, and its slot.
pub fn stage(gl: &glow::Context, fbos: [glow::Framebuffer; 2], tex: glow::Texture, w: u32, h: u32) -> Option<(glow::Texture, Fence, usize)> {
    let mut st = STAGED.lock().unwrap();
    let (staged, slot) = st.as_mut()?;
    *slot ^= 1;
    let k = *slot;
    unsafe {
        if let Some(f) = DONE.lock().unwrap()[k].take() {
            gl.wait_sync(f.0, 0, glow::TIMEOUT_IGNORED);
            gl.delete_sync(f.0);
        }
        let t = staged[k].0;
        if (staged[k].1, staged[k].2) != (w, h) {
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w as i32, h as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            staged[k] = (t, w, h);
        }
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbos[0]));
        gl.framebuffer_texture_2d(glow::READ_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbos[1]));
        gl.framebuffer_texture_2d(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(t), 0);
        gl.disable(glow::SCISSOR_TEST);
        gl.blit_framebuffer(0, 0, w as i32, h as i32, 0, 0, w as i32, h as i32, glow::COLOR_BUFFER_BIT, glow::NEAREST);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        let f = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0).expect("GL fence");
        // the present thread's context sees the commands only once they are sent
        gl.flush();
        Some((t, Fence(f), k))
    }
}

/// What the window shows besides the picture (the blit program, overlay, video), for threads
/// other than the window's: set by the window's thread whenever it changes.
static PARTS: std::sync::Mutex<Option<(glow::Program, Option<glow::Texture>, Option<(glow::Texture, u32, u32)>)>> = std::sync::Mutex::new(None);

/// The window's look with `shown` as the picture, in a window of `size` (None before the window
/// exists).
pub fn look_with(shown: (glow::Texture, u32, u32, bool, f32), size: (u32, u32)) -> Option<Look> {
    let (blit, overlay, video) = (*PARTS.lock().unwrap())?;
    Some(Look { blit, shown, overlay, video, size })
}

/// A GL fence (shared by the contexts of a share group).
pub struct Fence(pub glow::Fence);

// a fence is a name the share group knows; the present thread waits on it and deletes it
unsafe impl Send for Fence {}

/// The present thread's fences after drawing each staged texture (`Gl::stage`).
static DONE: std::sync::Mutex<[Option<Fence>; 2]> = std::sync::Mutex::new([None, None]);

/// What the window shows: the picture (texture, size, 0RGB words or not, gamma), the overlay
/// and a video (textures shared by all contexts), the window's size.
#[derive(Clone, Copy)]
pub struct Look {
    blit: glow::Program,
    pub shown: (glow::Texture, u32, u32, bool, f32),
    overlay: Option<glow::Texture>,
    video: Option<(glow::Texture, u32, u32)>,
    pub size: (u32, u32),
}

/// One thread at a time draws into the window and swaps (the window's thread, or the present
/// thread with its shared context).
static PAINT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Draw `l` into the window's back buffer; with `shot`, read back the picture's area.
fn paint(gl: &glow::Context, vao: glow::VertexArray, l: &Look, shot: bool) -> Option<(Vec<u32>, u32, u32)> {
        let (w, h) = l.size;
        let (tex, fw, fh, words, gamma) = l.shown;
        let (ox, oy, s) = super::window::fit(fw, fh, w, h);
        let (x0, y0) = (ox / w as f64 * 2.0 - 1.0, 1.0 - oy / h as f64 * 2.0);
        let (x1, y1) = (x0 + fw as f64 * s / w as f64 * 2.0, y0 - fh as f64 * s / h as f64 * 2.0);
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, w as i32, h as i32);
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            // the window stays opaque: 2D frames' 0RGB words have alpha 0, and a compositor that
            // blends windows by their alpha (Wayland) showed what is behind through the menu
            gl.color_mask(true, true, true, false);
            gl.use_program(Some(l.blit));
            gl.bind_vertex_array(Some(vao));
            gl.active_texture(glow::TEXTURE0);
            // asked of the driver once: NVIDIA's lookup makes system calls (a picture's five
            // cost ~10% of a game thread that shows its own pictures)
            let u = |n: &'static str| uniform(gl, l.blit, n);
            gl.uniform_4_f32(u("rect").as_ref(), x0 as f32, y1 as f32, x1 as f32, y0 as f32);
            gl.uniform_1_i32(u("tex").as_ref(), 0);
            gl.disable(glow::BLEND);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(u("words").as_ref(), words as i32);
            // rendered scenes have their first row at the bottom, uploaded frames at the top
            gl.uniform_1_i32(u("flip").as_ref(), !words as i32);
            gl.uniform_1_f32(u("gamma").as_ref(), gamma);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            // dialogs stay 4:3 over a wide picture
            let (sx, sw) = super::window::overlay_span(fw, fh);
            let (ox0, ox1) = (x0 + (x1 - x0) * sx as f64 / fw as f64, x0 + (x1 - x0) * (sx + sw) as f64 / fw as f64);
            // videos fill the window as far as their aspect allows, centred (the game placed
            // them in its 640x480 screen, briefings small and high up)
            if let Some((video, vw, vh)) = l.video {
                gl.clear(glow::COLOR_BUFFER_BIT);
                let (vx, vy, s) = super::window::fit(vw, vh, w, h);
                let (ax, ay) = (vx / w as f64 * 2.0 - 1.0, 1.0 - vy / h as f64 * 2.0);
                let (bx, by) = (ax + vw as f64 * s / w as f64 * 2.0, ay - vh as f64 * s / h as f64 * 2.0);
                gl.uniform_4_f32(u("rect").as_ref(), ax as f32, by as f32, bx as f32, ay as f32);
                gl.bind_texture(glow::TEXTURE_2D, Some(video));
                gl.uniform_1_i32(u("words").as_ref(), 1);
                gl.uniform_1_i32(u("flip").as_ref(), 0);
                gl.uniform_1_f32(u("gamma").as_ref(), 1.0);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            if let Some(overlay) = l.overlay {
                gl.uniform_4_f32(u("rect").as_ref(), ox0 as f32, y1 as f32, ox1 as f32, y0 as f32);
                gl.enable(glow::BLEND);
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                gl.bind_texture(glow::TEXTURE_2D, Some(overlay));
                gl.uniform_1_i32(u("words").as_ref(), 1);
                gl.uniform_1_i32(u("flip").as_ref(), 0);
                gl.uniform_1_f32(u("gamma").as_ref(), 1.0);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                gl.disable(glow::BLEND);
            }
            gl.color_mask(true, true, true, true);
        }
        let out = shot.then(|| {
            // the picture's area of the window, top row first
            let (rx, ry) = (ox.round() as i32, oy.round() as i32);
            let (rw, rh) = ((fw as f64 * s).round() as i32, (fh as f64 * s).round() as i32);
            let mut buf = vec![0u8; (rw * rh * 4) as usize];
            unsafe {
                gl.read_pixels(rx, h as i32 - ry - rh, rw, rh, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut buf)));
            }
            let px = (0..rh)
                .rev()
                .flat_map(|y| (0..rw).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let i = ((y * rw + x) * 4) as usize;
                    (buf[i] as u32) << 16 | (buf[i + 1] as u32) << 8 | buf[i + 2] as u32
                })
                .collect();
            (px, rw as u32, rh as u32)
        });
        // <PREFIX>_STATS' display, after the screenshot
        if let Some(new) = super::stats::on().then(super::stats::pixels) {
            draw_stats(gl, l.blit, new, w, h);
        }
        out
}

/// <PREFIX>_STATS' display in the top left corner, twice its size from 900 rows on (`new`: its
/// pixels when they changed). Its texture is shared by the contexts.
fn draw_stats(gl: &glow::Context, blit: glow::Program, new: Option<Vec<u32>>, w: u32, h: u32) {
    static TEX: std::sync::OnceLock<glow::Texture> = std::sync::OnceLock::new();
    let t = *TEX.get_or_init(|| texture(gl, false));
    if let Some(px) = new {
        upload(gl, t, &px, super::stats::W as u32, super::stats::H as u32);
    }
    let scale = if h >= 900 { 2.0 } else { 1.0 };
    let (x0, y0) = (-1.0 + 2.0 * 8.0 / w as f32, 1.0 - 2.0 * 8.0 / h as f32);
    let (x1, y1) = (x0 + 2.0 * super::stats::W as f32 * scale / w as f32, y0 - 2.0 * super::stats::H as f32 * scale / h as f32);
    let u = |n: &'static str| uniform(gl, blit, n);
    unsafe {
        gl.color_mask(true, true, true, false);
        gl.uniform_4_f32(u("rect").as_ref(), x0, y1, x1, y0);
        gl.uniform_1_i32(u("words").as_ref(), 1);
        gl.uniform_1_i32(u("flip").as_ref(), 0);
        gl.uniform_1_f32(u("gamma").as_ref(), 1.0);
        gl.enable(glow::BLEND);
        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        gl.bind_texture(glow::TEXTURE_2D, Some(t));
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.disable(glow::BLEND);
        gl.color_mask(true, true, true, true);
    }
}

/// A shared context (`Gl::share`) on its way to the present thread.
pub struct SharedParts {
    context: NotCurrentContext,
    /// the window's surface: the Gl that made it lives as long as the window
    surface: *const Surface<WindowSurface>,
    display: glutin::display::Display,
}

// the context is made current on the receiving thread only; the surface is the window's, which
// two contexts may draw to on two threads (one at a time: PAINT)
unsafe impl Send for SharedParts {}

/// A shared context current on its thread.
pub struct Shared {
    pub gl: glow::Context,
    context: PossiblyCurrentContext,
    surface: *const Surface<WindowSurface>,
    /// for drawing into the window (vertex arrays are not shared)
    vao: glow::VertexArray,
    /// the swap interval set: 1 (vsync) or 0
    vsync: bool,
    /// framebuffers to copy pictures through (`stage`; framebuffers are not shared)
    pub copy_fbo: [glow::Framebuffer; 2],
}

impl SharedParts {
    pub fn make_current(self) -> Shared {
        let context = self.context.make_current(unsafe { &*self.surface }).expect("cannot make the shared GL context current");
        let display = self.display;
        let gl = unsafe { glow::Context::from_loader_function_cstr(|s| display.get_proc_address(s)) };
        let vao = unsafe { gl.create_vertex_array() }.expect("GL vertex array");
        let _ = unsafe { &*self.surface }.set_swap_interval(&context, SwapInterval::DontWait);
        let copy_fbo = unsafe { [gl.create_framebuffer().expect("GL framebuffer"), gl.create_framebuffer().expect("GL framebuffer")] };
        Shared { gl, context, surface: self.surface, vao, vsync: false, copy_fbo }
    }
}

impl Shared {
    /// Draw `l` into the window and swap, once the GPU has done `fence` (the staged copy); the
    /// staged texture `slot`'s fence of being drawn goes to `DONE`.
    pub fn present(&mut self, l: &Look, fence: Fence, slot: usize, shot: bool) -> Option<(Vec<u32>, u32, u32)> {
        let _p = PAINT.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the window's surface lives as long as the window (see SharedParts)
        let surface = unsafe { &*self.surface };
        let vsync = VSYNC.load(std::sync::atomic::Ordering::Relaxed);
        if vsync != self.vsync {
            self.vsync = vsync;
            let interval = if vsync { SwapInterval::Wait(NonZeroU32::MIN) } else { SwapInterval::DontWait };
            let _ = surface.set_swap_interval(&self.context, interval);
        }
        unsafe {
            // the GPU waits for the copy, not this thread
            self.gl.wait_sync(fence.0, 0, glow::TIMEOUT_IGNORED);
            self.gl.delete_sync(fence.0);
        }
        let out = paint(&self.gl, self.vao, l, shot);
        unsafe {
            if let Some(f) = self.gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0).ok() {
                if let Some(old) = DONE.lock().unwrap()[slot].replace(Fence(f)) {
                    self.gl.delete_sync(old.0);
                }
            }
        }
        let _ = surface.swap_buffers(&self.context);
        out
    }
}

/// Vsync: the picture changes only at the display's refresh (no tearing in fullscreen; in a
/// window the desktop's compositor does that anyway). Off by default: the game paces itself.
static VSYNC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Vsync on or off, from the next swap. The pace stays the game's (or the port's): while the
/// game is slower than the display the driver queues a picture and the swap returns at once,
/// so the swap's return does not say when the refresh was (LESSONS, Vsync).
pub fn set_vsync(on: bool) {
    VSYNC.store(on, std::sync::atomic::Ordering::Relaxed);
}
