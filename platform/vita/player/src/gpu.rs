//! GPU front end on vita2d (GXM with precompiled shaders, linked
//! statically): no runtime shader compiler (`libshacccg.suprx`) or other
//! extra module is needed. All GPU/SDK calls stay out of the shared scene
//! VM. Normal 2D sprites are textured quads; complex effects may submit one
//! software-composed frame through the same swapchain.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Mutex;

use vitasdk_sys::{
    SCE_GXM_PRIMITIVE_TRIANGLE_STRIP, SCE_GXM_TEXTURE_FILTER_LINEAR,
    SCE_GXM_TEXTURE_FORMAT_U8U8U8U8_ABGR, SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW,
    SCE_KERNEL_MEMBLOCK_TYPE_USER_RW_UNCACHE, SceGxmPrimitiveType, SceGxmTextureFilter,
    SceGxmTextureFormat,
};

const SCREEN_W: i32 = 960;
const SCREEN_H: i32 = 544;
/// A 1280x720 scene with a few full-screen layers needs more than 16 MiB in
/// one frame, a 1920x1080 one 8 MiB per full-screen layer; when the
/// textures of one frame do not fit, it falls back to the (slow) software
/// compositor. CDRAM has ~110 MiB free after vita2d starts; small textures
/// live in ordinary memory (`CDRAM_MIN_TEXTURE_BYTES`) and a failed CDRAM
/// allocation falls back to it too.
const GPU_TEXTURE_BUDGET: usize = 80 * 1024 * 1024;
/// vita2d's per-frame pool for vertices (ours and its own clip/clear
/// rectangles). Its default is 1 MiB; running out makes it write through
/// null pointers.
const TEMP_POOL_BYTES: u32 = 4 * 1024 * 1024;
/// Draws stop (and are logged) when the pool has less than this left.
const POOL_RESERVE_BYTES: u32 = 16 * 1024;
/// CDRAM blocks are allocated in 256 KiB units; smaller textures (text,
/// icons) go to ordinary uncached memory instead of wasting most of a unit.
const CDRAM_MIN_TEXTURE_BYTES: usize = 256 * 1024;
/// Frames a retired texture's memory is kept (see `State::pending_free`).
const RETIRE_FRAMES: u64 = 4;

#[repr(C)]
struct Vita2dTexture {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Vita2dTextureVertex {
    x: f32,
    y: f32,
    z: f32,
    u: f32,
    v: f32,
}

unsafe extern "C" {
    fn vita2d_init_advanced(temp_pool_size: u32) -> i32;
    fn vita2d_pool_free_space() -> u32;
    fn vita2d_fini() -> i32;
    fn vita2d_wait_rendering_done();
    fn vita2d_start_drawing();
    fn vita2d_end_drawing();
    fn vita2d_swap_buffers();
    fn vita2d_clear_screen();
    fn vita2d_set_clear_color(color: u32);
    fn vita2d_set_vblank_wait(enable: i32);
    fn vita2d_enable_clipping();
    fn vita2d_disable_clipping();
    fn vita2d_set_clip_rectangle(x_min: i32, y_min: i32, x_max: i32, y_max: i32);
    fn vita2d_set_blend_mode_add(enable: i32);
    fn vita2d_pool_memalign(size: u32, alignment: u32) -> *mut c_void;
    fn vita2d_texture_set_alloc_memblock_type(kind: u32);
    fn vita2d_create_empty_texture_format(
        w: u32,
        h: u32,
        format: SceGxmTextureFormat,
    ) -> *mut Vita2dTexture;
    fn vita2d_free_texture(texture: *mut Vita2dTexture);
    fn vita2d_texture_get_stride(texture: *const Vita2dTexture) -> u32;
    fn vita2d_texture_get_datap(texture: *const Vita2dTexture) -> *mut c_void;
    fn vita2d_texture_set_filters(
        texture: *mut Vita2dTexture,
        min_filter: SceGxmTextureFilter,
        mag_filter: SceGxmTextureFilter,
    );
    fn vita2d_draw_array_textured(
        texture: *const Vita2dTexture,
        mode: SceGxmPrimitiveType,
        vertices: *const Vita2dTextureVertex,
        count: usize,
        color: u32,
    );
    fn vita2d_get_shader_patcher() -> *mut c_void;
    fn vita2d_start_drawing_advanced(target: *mut Vita2dTexture, flags: u32);
    fn vita2d_create_empty_texture_rendertarget(
        w: u32,
        h: u32,
        format: SceGxmTextureFormat,
    ) -> *mut Vita2dTexture;
    fn vita2d_pool_reset();
    /// The fragment program `vita2d_draw_array_textured` binds; vita2d's
    /// blend-mode switch rewrites it from its own table.
    static mut _vita2d_textureTintFragmentProgram: *const c_void;
    static texture_tint_f_gxp_start: u8;
    static texture_v_gxp_start: u8;
    fn sceGxmShaderPatcherRegisterProgram(
        patcher: *mut c_void,
        program: *const c_void,
        id: *mut *mut c_void,
    ) -> i32;
    fn sceGxmShaderPatcherUnregisterProgram(patcher: *mut c_void, id: *mut c_void) -> i32;
    fn sceGxmShaderPatcherCreateFragmentProgram(
        patcher: *mut c_void,
        id: *mut c_void,
        output_format: u32,
        multisample: u32,
        blend: *const [u8; 4],
        vertex_program: *const c_void,
        out: *mut *const c_void,
    ) -> i32;
    fn sceGxmShaderPatcherReleaseFragmentProgram(
        patcher: *mut c_void,
        program: *const c_void,
    ) -> i32;
}

/// Blend modes from the renderer (`SpriteBlend`): the index passed to
/// `siglus_vita_gpu_draw`.
const BLEND_NORMAL: u32 = 0;
const BLEND_MUL: u32 = 3;
const BLEND_SCREEN: u32 = 4;
/// Normal blending inside the wipe layer (see `BLEND_INFOS`).
const BLEND_LAYER_NORMAL: u32 = 5;
/// A premultiplied image over the destination (the wipe layer itself).
const BLEND_PREMULTIPLIED_OVER: u32 = 6;
const BLEND_MODES: usize = 7;
/// `SceGxmSceneFlags`: the layer's scene is finished before the display
/// scene reads it.
const SCE_GXM_SCENE_FRAGMENT_SET_DEPENDENCY: u32 = 1;
const SCE_GXM_SCENE_VERTEX_WAIT_FOR_DEPENDENCY: u32 = 2;

/// `SceGxmBlendInfo` bytes (colour mask, colour/alpha function, colour
/// src/dst factor, alpha src/dst factor; low nibble first) for modes 1..=4,
/// matching the software compositor. vita2d's own add mode is `ONE, ONE`,
/// which ignores the texture's alpha (an alpha-shaped additive movie showed
/// as a bright rectangle).
/// - Add: d + s*a (`ADD`, `SRC_ALPHA`, `ONE`).
/// - Sub: d - s*a (`REVERSE_SUBTRACT`, `SRC_ALPHA`, `ONE`).
/// - Mul: d * (1 - a + s*a) = (s*a)*d + d*(1 - a): premultiplied source,
///   `DST_COLOR`, `ONE_MINUS_SRC_ALPHA`.
/// - Screen: 1 - (1 - d)(1 - s*a) = (s*a)*(1 - d) + d: premultiplied
///   source, `ONE_MINUS_DST_COLOR`, `ONE`.
/// - Layer normal: colour as vita2d's normal mode, alpha `ONE,
///   ONE_MINUS_SRC_ALPHA`. The wipe layer starts transparent, so its colour
///   ends up premultiplied and its alpha must be the real coverage (vita2d
///   writes a*a + d*(1 - a)).
/// - Premultiplied over: `ONE, ONE_MINUS_SRC_ALPHA`, for drawing the layer.
const BLEND_INFOS: [[u8; 4]; BLEND_MODES - 1] = [
    [0x0f, 0x11, 0x14, 0x11],
    [0x0f, 0x13, 0x14, 0x11],
    [0x0f, 0x11, 0x56, 0x11],
    [0x0f, 0x11, 0x17, 0x11],
    [0x0f, 0x11, 0x54, 0x51],
    [0x0f, 0x11, 0x51, 0x51],
];
const SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4: u32 = 1;
const SCE_GXM_MULTISAMPLE_NONE: u32 = 0;

/// Modes whose texture is premultiplied by the renderer; the draw's alpha
/// then scales all four channels.
fn blend_is_premultiplied(blend: u32) -> bool {
    matches!(blend, BLEND_MUL | BLEND_SCREEN)
}

/// vita2d's tinted-texture shader registered once more, with one fragment
/// program per blend mode in `BLEND_INFOS`.
struct BlendPrograms {
    id: *mut c_void,
    programs: [*const c_void; BLEND_MODES - 1],
}

fn create_blend_programs() -> Option<BlendPrograms> {
    unsafe {
        let patcher = vita2d_get_shader_patcher();
        if patcher.is_null() {
            return None;
        }
        let mut id = std::ptr::null_mut();
        let code = sceGxmShaderPatcherRegisterProgram(
            patcher,
            (&raw const texture_tint_f_gxp_start).cast(),
            &mut id,
        );
        if code < 0 {
            super::vita::log(&format!("gpu: blend program register failed: {code:#x}"));
            return None;
        }
        let mut out = BlendPrograms {
            id,
            programs: [std::ptr::null(); BLEND_MODES - 1],
        };
        for (slot, info) in out.programs.iter_mut().zip(&BLEND_INFOS) {
            let code = sceGxmShaderPatcherCreateFragmentProgram(
                patcher,
                id,
                SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4,
                SCE_GXM_MULTISAMPLE_NONE,
                info,
                (&raw const texture_v_gxp_start).cast(),
                slot,
            );
            if code < 0 || slot.is_null() {
                super::vita::log(&format!("gpu: blend program create failed: {code:#x}"));
                *slot = std::ptr::null();
                release_blend_programs(out);
                return None;
            }
        }
        Some(out)
    }
}

fn release_blend_programs(blend: BlendPrograms) {
    unsafe {
        let patcher = vita2d_get_shader_patcher();
        for program in blend.programs {
            if !program.is_null() {
                sceGxmShaderPatcherReleaseFragmentProgram(patcher, program);
            }
        }
        sceGxmShaderPatcherUnregisterProgram(patcher, blend.id);
    }
}

/// Switches the blend mode of the following textured draws. Without our
/// programs only normal and vita2d's add mode exist.
fn set_blend(state: &State, blend: u32) -> bool {
    let blend = if state.in_layer && blend == BLEND_NORMAL {
        BLEND_LAYER_NORMAL
    } else {
        blend
    };
    unsafe {
        match (&state.blend_programs, blend) {
            // Restores vita2d's normal programs.
            (_, BLEND_NORMAL) => vita2d_set_blend_mode_add(0),
            (Some(programs), 1..=6) => {
                _vita2d_textureTintFragmentProgram = programs.programs[blend as usize - 1]
            }
            (None, 1) => vita2d_set_blend_mode_add(1),
            _ => return false,
        }
    }
    true
}

/// A vertex from the renderer: logical-screen position and 0..1 texture
/// coordinates, in fan order (corners clockwise from top-left).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GpuVertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
}

struct Texture {
    handle: *mut Vita2dTexture,
    /// Identifies the pixels last uploaded (`siglus_vita_gpu_draw`'s
    /// `source_id`).
    source_id: u64,
    width: u32,
    height: u32,
    bytes: usize,
    last_frame: u64,
}

struct State {
    textures: HashMap<u32, Texture>,
    texture_bytes: usize,
    fallback: *mut Vita2dTexture,
    fallback_size: (u32, u32),
    frame: u64,
    logical_size: (u32, u32),
    /// The letterboxed game area on screen: x, y, width, height.
    viewport: (f32, f32, f32, f32),
    /// Between `vita2d_start_drawing` and `vita2d_end_drawing`.
    drawing: bool,
    /// Textures no longer used, with the frame they were retired in. They
    /// are freed `RETIRE_FRAMES` frames later: Vita3K's renderer reads
    /// textures on its own thread and can still reach one after
    /// `sceGxmFinish` has returned (it crashed uploading freed memory).
    pending_free: Vec<(u64, *mut Vita2dTexture)>,
    /// The clip rectangle set in this frame (`None`: clipping off).
    /// vita2d draws two stencil rectangles, one full-screen, per change.
    clip: Option<[i32; 4]>,
    /// Draws skipped because vita2d's pool ran low (logged once a frame).
    pool_exhausted: bool,
    /// The blend mode set for this frame's draws (`BLEND_*`).
    blend: u32,
    /// Fragment programs for the non-normal blend modes (None: only
    /// vita2d's normal and `ONE, ONE` add modes).
    blend_programs: Option<BlendPrograms>,
    /// Screen-sized render target for a wipe's incoming screen, drawn over
    /// the outgoing one at the wipe's progress (created on first use).
    layer: *mut Vita2dTexture,
    /// Drawing into `layer` (its own GXM scene).
    in_layer: bool,
    /// `layer` was drawn this frame: the display scene waits for it.
    layer_ready: bool,
}

// The raw texture handles are only touched from the main thread; the mutex
// guards the state against the C entry points being re-entered.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Texture traffic since the last `take_upload_summary` (periodic log).
static UPLOAD_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UPLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CREATED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// This frame's GPU wait and texture uploads (slow-frame log).
static FRAME_WAIT_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FRAME_UPLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FRAME_UPLOAD_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The current frame's GPU wait and uploads, then reset.
pub fn take_frame_detail() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    format!(
        "gpu wait {} us, uploads {} ({} bytes)",
        FRAME_WAIT_US.swap(0, Relaxed),
        FRAME_UPLOADS.swap(0, Relaxed),
        FRAME_UPLOAD_BYTES.swap(0, Relaxed)
    )
}

pub fn take_upload_summary() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    format!(
        "textures uploads={} upload-bytes={} created={} cached-bytes={}",
        UPLOADS.swap(0, Relaxed),
        UPLOAD_BYTES.swap(0, Relaxed),
        CREATED.swap(0, Relaxed),
        texture_bytes()
    )
}

pub struct GpuDisplay;

impl GpuDisplay {
    pub fn new() -> Result<Self, String> {
        let code = unsafe { vita2d_init_advanced(TEMP_POOL_BYTES) };
        if code < 0 {
            return Err(format!("vita2d init failed: {code:#x}"));
        }
        unsafe { vita2d_set_clear_color(0xFF00_0000) };
        let mut slot = STATE.lock().map_err(|_| "GPU state mutex poisoned")?;
        *slot = Some(State {
            textures: HashMap::new(),
            texture_bytes: 0,
            fallback: std::ptr::null_mut(),
            fallback_size: (0, 0),
            frame: 0,
            logical_size: (SCREEN_W as u32, SCREEN_H as u32),
            viewport: (0.0, 0.0, SCREEN_W as f32, SCREEN_H as f32),
            drawing: false,
            pending_free: Vec::new(),
            clip: None,
            pool_exhausted: false,
            blend: BLEND_NORMAL,
            blend_programs: create_blend_programs(),
            layer: std::ptr::null_mut(),
            in_layer: false,
            layer_ready: false,
        });
        Ok(Self)
    }
}

impl Drop for GpuDisplay {
    fn drop(&mut self) {
        if let Ok(mut slot) = STATE.lock()
            && let Some(state) = slot.take()
        {
            unsafe {
                if state.drawing {
                    vita2d_end_drawing();
                }
                vita2d_wait_rendering_done();
                for texture in state.textures.values() {
                    vita2d_free_texture(texture.handle);
                }
                for (_, texture) in state.pending_free {
                    vita2d_free_texture(texture);
                }
                if !state.fallback.is_null() {
                    vita2d_free_texture(state.fallback);
                }
                if !state.layer.is_null() {
                    vita2d_free_texture(state.layer);
                }
                vita2d_set_blend_mode_add(0);
                if let Some(programs) = state.blend_programs {
                    release_blend_programs(programs);
                }
                vita2d_fini();
            }
        }
    }
}

/// Closes a frame the renderer abandoned (it fell back to the software
/// compositor part way) without showing it: GXM scenes cannot nest, and
/// drawing into a failed scene makes vita2d use invalid uniform buffers.
fn close_abandoned_frame(state: &mut State) {
    if state.drawing {
        unsafe {
            vita2d_disable_clipping();
            vita2d_end_drawing();
            vita2d_wait_rendering_done();
        }
        free_retired(state);
        state.drawing = false;
    }
    state.in_layer = false;
    state.layer_ready = false;
}

/// Per-scene drawing state: no clip, normal blending, the game's viewport.
fn reset_scene_state(state: &mut State, width: u32, height: u32) {
    state.clip = None;
    state.blend = BLEND_NORMAL;
    state.logical_size = (width, height);
    let scale = (SCREEN_W as f64 / width as f64).min(SCREEN_H as f64 / height as f64);
    let draw_w = (width as f64 * scale).round() as i32;
    let draw_h = (height as f64 * scale).round() as i32;
    state.viewport = (
        ((SCREEN_W - draw_w) / 2) as f32,
        ((SCREEN_H - draw_h) / 2) as f32,
        draw_w as f32,
        draw_h as f32,
    );
}

fn begin_frame(state: &mut State, width: u32, height: u32) -> bool {
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return false;
    }
    // After a wipe layer the frame is already under way: its scene has
    // been submitted, and resetting vita2d's pool (`vita2d_start_drawing`)
    // would overwrite vertices the GPU has yet to read.
    let after_layer = state.layer_ready && !state.drawing;
    if !after_layer {
        close_abandoned_frame(state);
        state.pool_exhausted = false;
        state.frame = state.frame.wrapping_add(1);
    }
    reset_scene_state(state, width, height);
    unsafe {
        if after_layer {
            vita2d_start_drawing_advanced(
                std::ptr::null_mut(),
                SCE_GXM_SCENE_VERTEX_WAIT_FOR_DEPENDENCY,
            );
        } else {
            vita2d_start_drawing();
        }
        vita2d_disable_clipping();
        vita2d_set_blend_mode_add(0);
        vita2d_clear_screen();
    }
    state.drawing = true;
    true
}

/// Starts a frame by drawing into the wipe layer (cleared transparent).
fn begin_layer(state: &mut State, width: u32, height: u32) -> bool {
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return false;
    }
    if state.blend_programs.is_none() {
        return false;
    }
    close_abandoned_frame(state);
    if state.layer.is_null() {
        unsafe {
            vita2d_texture_set_alloc_memblock_type(SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW);
            state.layer = vita2d_create_empty_texture_rendertarget(
                SCREEN_W as u32,
                SCREEN_H as u32,
                SCE_GXM_TEXTURE_FORMAT_U8U8U8U8_ABGR,
            );
        }
        if state.layer.is_null() {
            super::vita::log("gpu: wipe layer render target allocation failed");
            return false;
        }
    }
    state.pool_exhausted = false;
    state.frame = state.frame.wrapping_add(1);
    reset_scene_state(state, width, height);
    unsafe {
        vita2d_pool_reset();
        vita2d_start_drawing_advanced(state.layer, SCE_GXM_SCENE_FRAGMENT_SET_DEPENDENCY);
        vita2d_disable_clipping();
        vita2d_set_blend_mode_add(0);
        vita2d_set_clear_color(0);
        vita2d_clear_screen();
        vita2d_set_clear_color(0xFF00_0000);
    }
    state.drawing = true;
    state.in_layer = true;
    true
}

fn end_layer(state: &mut State) -> bool {
    if !state.drawing || !state.in_layer {
        return false;
    }
    unsafe {
        vita2d_disable_clipping();
        vita2d_end_drawing();
    }
    state.drawing = false;
    state.in_layer = false;
    state.layer_ready = true;
    true
}

fn end_frame(state: &mut State, wait_vsync: bool) {
    if !state.drawing {
        return;
    }
    let wait_start = std::time::Instant::now();
    unsafe {
        vita2d_disable_clipping();
        vita2d_end_drawing();
        // Textures may be replaced or freed from the next frame on; let the
        // GPU finish with them (and with this frame's vertices) first.
        vita2d_wait_rendering_done();
    }
    FRAME_WAIT_US.fetch_add(
        wait_start.elapsed().as_micros() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    free_retired(state);
    unsafe {
        vita2d_set_vblank_wait(i32::from(wait_vsync));
        vita2d_swap_buffers();
    }
    state.drawing = false;
    state.layer_ready = false;
}

/// Queues a texture to be freed once no frame in flight can use it.
fn retire(state: &mut State, texture: *mut Vita2dTexture) {
    state.pending_free.push((state.frame, texture));
}

fn free_retired(state: &mut State) {
    let frame = state.frame;
    state.pending_free.retain(|&(retired, texture)| {
        let expired = frame.wrapping_sub(retired) >= RETIRE_FRAMES;
        if expired {
            unsafe { vita2d_free_texture(texture) };
        }
        !expired
    });
}

fn set_clip(state: &mut State, clip: &[i32; 4]) -> bool {
    let (width, height) = state.logical_size;
    let left = clip[0].clamp(0, width as i32);
    let top = clip[1].clamp(0, height as i32);
    let right = clip[2].clamp(0, width as i32);
    let bottom = clip[3].clamp(0, height as i32);
    if left >= right || top >= bottom {
        return false;
    }
    // The whole screen needs no clipping.
    let rect = [left, top, right, bottom];
    let wanted = (rect != [0, 0, width as i32, height as i32]).then_some(rect);
    if wanted == state.clip {
        return true;
    }
    state.clip = wanted;
    let Some(_) = wanted else {
        unsafe { vita2d_disable_clipping() };
        return true;
    };
    let (vx, vy, vw, vh) = state.viewport;
    let sx = vw / width as f32;
    let sy = vh / height as f32;
    let x0 = (vx + left as f32 * sx).floor() as i32;
    let x1 = (vx + right as f32 * sx).ceil() as i32;
    let y0 = (vy + top as f32 * sy).floor() as i32;
    let y1 = (vy + bottom as f32 * sy).ceil() as i32;
    unsafe {
        vita2d_set_clip_rectangle(x0, y0, x1, y1);
        vita2d_enable_clipping();
    }
    true
}

fn draw_quad(
    state: &State,
    texture: *const Vita2dTexture,
    vertices: &[GpuVertex; 4],
    alpha: f32,
    premultiplied: bool,
) {
    let (vx, vy, vw, vh) = state.viewport;
    let (width, height) = state.logical_size;
    let sx = vw / width as f32;
    let sy = vh / height as f32;
    // The GPU reads vertices after this call returns: they live in
    // vita2d's per-frame pool.
    let out = unsafe {
        vita2d_pool_memalign(
            (4 * size_of::<Vita2dTextureVertex>()) as u32,
            size_of::<Vita2dTextureVertex>() as u32,
        )
    }
    .cast::<Vita2dTextureVertex>();
    if out.is_null() {
        return;
    }
    // Fan order (0, 1, 2, 3) as a strip is (0, 1, 3, 2).
    for (slot, &index) in [0usize, 1, 3, 2].iter().enumerate() {
        let v = vertices[index];
        unsafe {
            out.add(slot).write(Vita2dTextureVertex {
                x: vx + v.x * sx,
                y: vy + v.y * sy,
                z: 0.5,
                u: v.u,
                v: v.v,
            });
        }
    }
    let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    // vita2d's tint multiplies the texel: straight textures keep their
    // colour, premultiplied ones are scaled with their alpha.
    let color = if premultiplied {
        alpha * 0x0101_0101
    } else {
        (alpha << 24) | 0x00FF_FFFF
    };
    unsafe {
        vita2d_draw_array_textured(texture, SCE_GXM_PRIMITIVE_TRIANGLE_STRIP, out, 4, color);
    }
}

/// Copies straight RGBA rows into a texture (whose rows are padded).
fn upload(texture: *mut Vita2dTexture, width: u32, height: u32, pixels: *const u8) {
    UPLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FRAME_UPLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    FRAME_UPLOAD_BYTES.fetch_add(
        u64::from(width) * u64::from(height) * 4,
        std::sync::atomic::Ordering::Relaxed,
    );
    UPLOAD_BYTES.fetch_add(
        u64::from(width) * u64::from(height) * 4,
        std::sync::atomic::Ordering::Relaxed,
    );
    unsafe {
        let stride = vita2d_texture_get_stride(texture) as usize;
        let data = vita2d_texture_get_datap(texture).cast::<u8>();
        let row = width as usize * 4;
        for y in 0..height as usize {
            std::ptr::copy_nonoverlapping(pixels.add(y * row), data.add(y * stride), row);
        }
    }
}

/// `vita2d_create_empty_texture_format` mallocs its `vita2d_texture` without
/// clearing it and sets only `palette_UID`, but `vita2d_free_texture` also
/// destroys `gxm_rtgt` and frees `depth_UID` when they are non-zero. On
/// reused heap memory those are garbage: freeing a texture then unmapped and
/// freed an unrelated memblock (`sceGxmUnmapMemory` INVALID_POINTER, then a
/// corrupted heap). Offsets are from vita2d.h: SceGxmTexture (16 bytes),
/// data_UID @16, palette_UID @20, gxm_rtgt @24, gxm_sfc @28, gxm_sfd @52,
/// depth_UID @96 (100 bytes in all, the size vita2d allocates).
unsafe fn clear_render_target_fields(texture: *mut Vita2dTexture) {
    unsafe {
        let base = texture.cast::<u8>();
        base.add(24)
            .cast::<*mut c_void>()
            .write(std::ptr::null_mut());
        base.add(96).cast::<i32>().write(0);
    }
}

fn make_texture(width: u32, height: u32, pixels: *const u8) -> Option<*mut Vita2dTexture> {
    let bytes = width as usize * height as usize * 4;
    unsafe {
        vita2d_texture_set_alloc_memblock_type(if bytes >= CDRAM_MIN_TEXTURE_BYTES {
            SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW
        } else {
            SCE_KERNEL_MEMBLOCK_TYPE_USER_RW_UNCACHE
        });
        CREATED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let texture =
            vita2d_create_empty_texture_format(width, height, SCE_GXM_TEXTURE_FORMAT_U8U8U8U8_ABGR);
        if texture.is_null() {
            super::vita::log(&format!(
                "gpu texture: allocation of {width}x{height} failed"
            ));
            return None;
        }
        clear_render_target_fields(texture);
        vita2d_texture_set_filters(
            texture,
            SCE_GXM_TEXTURE_FILTER_LINEAR,
            SCE_GXM_TEXTURE_FILTER_LINEAR,
        );
        upload(texture, width, height, pixels);
        Some(texture)
    }
}

fn cached_texture(
    state: &mut State,
    key: u32,
    source_id: u64,
    pixels: *const u8,
    width: u32,
    height: u32,
) -> Option<*mut Vita2dTexture> {
    let bytes = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if bytes > GPU_TEXTURE_BUDGET {
        return None;
    }
    if let Some(existing) = state.textures.get_mut(&key)
        && existing.source_id == source_id
        && existing.width == width
        && existing.height == height
    {
        existing.last_frame = state.frame;
        return Some(existing.handle);
    }
    // New pixels for the same image (a movie or animated frame): rewrite
    // the texture in place. The GPU has finished every earlier frame (see
    // `end_frame`), so only one already drawn in this frame needs a new
    // texture. Freeing and allocating GPU memory every movie frame is slow
    // and crashed Vita3K's GXM memory mapping.
    if let Some(existing) = state.textures.get_mut(&key)
        && existing.width == width
        && existing.height == height
        && existing.last_frame != state.frame
    {
        upload(existing.handle, width, height, pixels);
        existing.source_id = source_id;
        existing.last_frame = state.frame;
        return Some(existing.handle);
    }
    // Textures are only freed or replaced between frames' GPU work (see
    // `end_frame`); ones drawn in this frame are kept.
    if let Some(old) = state.textures.remove(&key) {
        state.texture_bytes -= old.bytes;
        retire(state, old.handle);
    }
    while state.texture_bytes + bytes > GPU_TEXTURE_BUDGET {
        let Some((&old_key, _)) = state
            .textures
            .iter()
            .filter(|(_, t)| t.last_frame != state.frame)
            .min_by_key(|(_, t)| t.last_frame)
        else {
            return None;
        };
        let old = state.textures.remove(&old_key)?;
        state.texture_bytes -= old.bytes;
        retire(state, old.handle);
    }
    let handle = make_texture(width, height, pixels)?;
    state.texture_bytes += bytes;
    state.textures.insert(
        key,
        Texture {
            handle,
            source_id,
            width,
            height,
            bytes,
            last_frame: state.frame,
        },
    );
    Some(handle)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn siglus_vita_gpu_begin(width: u32, height: u32) -> bool {
    let Ok(mut slot) = STATE.lock() else {
        return false;
    };
    let Some(state) = slot.as_mut() else {
        return false;
    };
    begin_frame(state, width, height)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn siglus_vita_gpu_draw(
    key: u32,
    source_id: u64,
    pixels: *const u8,
    width: u32,
    height: u32,
    vertices: *const GpuVertex,
    clip: *const i32,
    alpha: f32,
    blend: u32,
) -> bool {
    if pixels.is_null()
        || vertices.is_null()
        || clip.is_null()
        || width == 0
        || height == 0
        || width > 2048
        || height > 2048
    {
        return false;
    }
    let Ok(mut slot) = STATE.lock() else {
        return false;
    };
    let Some(state) = slot.as_mut() else {
        return false;
    };
    if !state.drawing {
        return false;
    }
    if unsafe { vita2d_pool_free_space() } < POOL_RESERVE_BYTES {
        if !state.pool_exhausted {
            state.pool_exhausted = true;
            super::vita::log(
                "gpu: vita2d pool exhausted; skipping the rest of this frame's sprites",
            );
        }
        return true;
    }
    let clip = unsafe { &*(clip as *const [i32; 4]) };
    if !set_clip(state, clip) {
        return true;
    }
    let Some(texture) = cached_texture(state, key, source_id, pixels, width, height) else {
        return false;
    };
    let vertices = unsafe { &*(vertices as *const [GpuVertex; 4]) };
    if blend != state.blend {
        if !set_blend(state, blend) {
            return false;
        }
        state.blend = blend;
    }
    draw_quad(
        state,
        texture,
        vertices,
        alpha,
        blend_is_premultiplied(blend),
    );
    true
}

/// Starts a frame whose first scene is the wipe layer; `siglus_vita_gpu_draw`
/// then draws into it until `siglus_vita_gpu_end_layer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn siglus_vita_gpu_begin_layer(width: u32, height: u32) -> bool {
    let Ok(mut slot) = STATE.lock() else {
        return false;
    };
    let Some(state) = slot.as_mut() else {
        return false;
    };
    begin_layer(state, width, height)
}

#[unsafe(no_mangle)]
pub extern "C" fn siglus_vita_gpu_end_layer() -> bool {
    let Ok(mut slot) = STATE.lock() else {
        return false;
    };
    slot.as_mut().is_some_and(end_layer)
}

/// Draws this frame's wipe layer over the screen at `alpha`.
#[unsafe(no_mangle)]
pub extern "C" fn siglus_vita_gpu_draw_layer(alpha: f32) -> bool {
    let Ok(mut slot) = STATE.lock() else {
        return false;
    };
    let Some(state) = slot.as_mut() else {
        return false;
    };
    if !state.drawing || state.in_layer || !state.layer_ready || state.layer.is_null() {
        return false;
    }
    if state.clip.is_some() {
        state.clip = None;
        unsafe { vita2d_disable_clipping() };
    }
    if state.blend != BLEND_PREMULTIPLIED_OVER {
        if !set_blend(state, BLEND_PREMULTIPLIED_OVER) {
            return false;
        }
        state.blend = BLEND_PREMULTIPLIED_OVER;
    }
    // The layer covers the whole screen: draw it in screen coordinates.
    let (width, height) = state.logical_size;
    let viewport = state.viewport;
    state.logical_size = (SCREEN_W as u32, SCREEN_H as u32);
    state.viewport = (0.0, 0.0, SCREEN_W as f32, SCREEN_H as f32);
    let (w, h) = (SCREEN_W as f32, SCREEN_H as f32);
    let vertices = [
        GpuVertex {
            x: 0.0,
            y: 0.0,
            u: 0.0,
            v: 0.0,
        },
        GpuVertex {
            x: w,
            y: 0.0,
            u: 1.0,
            v: 0.0,
        },
        GpuVertex {
            x: w,
            y: h,
            u: 1.0,
            v: 1.0,
        },
        GpuVertex {
            x: 0.0,
            y: h,
            u: 0.0,
            v: 1.0,
        },
    ];
    let layer = state.layer;
    draw_quad(state, layer, &vertices, alpha, true);
    state.logical_size = (width, height);
    state.viewport = viewport;
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn siglus_vita_gpu_end(wait_vsync: bool) {
    if let Ok(mut slot) = STATE.lock()
        && let Some(state) = slot.as_mut()
    {
        end_frame(state, wait_vsync);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn siglus_vita_present_rgba(
    pixels: *const u8,
    width: u32,
    height: u32,
    wait_vsync: bool,
) {
    if pixels.is_null() || width == 0 || height == 0 || width > 2048 || height > 2048 {
        return;
    }
    let Ok(mut slot) = STATE.lock() else {
        return;
    };
    let Some(state) = slot.as_mut() else {
        return;
    };
    // The previous frame is finished (see `end_frame`), so the fallback
    // texture can be rewritten in place.
    if state.fallback.is_null() || state.fallback_size != (width, height) {
        if !state.fallback.is_null() {
            let old = state.fallback;
            retire(state, old);
            state.fallback = std::ptr::null_mut();
        }
        let Some(texture) = make_texture(width, height, pixels) else {
            super::vita::log("gpu fallback: texture create failed");
            return;
        };
        state.fallback = texture;
        state.fallback_size = (width, height);
    } else {
        upload(state.fallback, width, height, pixels);
    }
    if !begin_frame(state, width, height) {
        return;
    }
    let (w, h) = (width as f32, height as f32);
    let vertices = [
        GpuVertex {
            x: 0.0,
            y: 0.0,
            u: 0.0,
            v: 0.0,
        },
        GpuVertex {
            x: w,
            y: 0.0,
            u: 1.0,
            v: 0.0,
        },
        GpuVertex {
            x: w,
            y: h,
            u: 1.0,
            v: 1.0,
        },
        GpuVertex {
            x: 0.0,
            y: h,
            u: 0.0,
            v: 1.0,
        },
    ];
    let fallback = state.fallback;
    draw_quad(state, fallback, &vertices, 1.0, false);
    end_frame(state, wait_vsync);
}

pub fn texture_bytes() -> usize {
    STATE
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|v| v.texture_bytes))
        .unwrap_or(0)
}
