//! VitaGL front end: all GPU/SDK calls stay out of the shared scene VM.
//! Normal 2D sprites are textured quads. Complex effects may submit one
//! software-composed frame through the same GPU swapchain.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Mutex;

use vitasdk_sys::SCE_GXM_MULTISAMPLE_NONE;

const SCREEN_W: i32 = 960;
const SCREEN_H: i32 = 544;
const GPU_TEXTURE_BUDGET: usize = 16 * 1024 * 1024;
const GL_FALSE: u8 = 0;
const GL_TRUE: u8 = 1;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_BLEND: u32 = 0x0be2;
const GL_SCISSOR_TEST: u32 = 0x0c11;
const GL_TEXTURE_2D: u32 = 0x0de1;
const GL_MODELVIEW: u32 = 0x1700;
const GL_PROJECTION: u32 = 0x1701;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_FLOAT: u32 = 0x1406;
const GL_SRC_ALPHA: u32 = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812f;
const GL_VERTEX_ARRAY: u32 = 0x8074;
const GL_TEXTURE_COORD_ARRAY: u32 = 0x8078;
const GL_TRIANGLE_FAN: u32 = 0x0006;

unsafe extern "C" {
    fn shark_init(path: *const i8) -> i32;
    fn vglSetDisplayBufferCount(count: i32);
    fn vglSetCircularPoolSize(size: u32);
    fn vglInitWithCustomSizes(
        legacy_pool_size: i32,
        width: i32,
        height: i32,
        ram_pool_size: i32,
        cdram_pool_size: i32,
        phycont_pool_size: i32,
        cdlg_pool_size: i32,
        msaa: u32,
    ) -> u8;
    fn vglWaitVblankStart(enable: u8);
    fn vglSwapBuffers(has_common_dialog: u8);
    fn glViewport(x: i32, y: i32, width: i32, height: i32);
    fn glMatrixMode(mode: u32);
    fn glLoadIdentity();
    fn glOrthof(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32);
    fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32);
    fn glClear(mask: u32);
    fn glEnable(cap: u32);
    fn glDisable(cap: u32);
    fn glBlendFunc(source: u32, destination: u32);
    fn glScissor(x: i32, y: i32, width: i32, height: i32);
    fn glColor4f(red: f32, green: f32, blue: f32, alpha: f32);
    fn glGenTextures(count: i32, textures: *mut u32);
    fn glDeleteTextures(count: i32, textures: *const u32);
    fn glBindTexture(target: u32, texture: u32);
    fn glTexParameteri(target: u32, name: u32, value: i32);
    fn glTexImage2D(
        target: u32,
        level: i32,
        internal_format: i32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        pixel_type: u32,
        pixels: *const c_void,
    );
    fn glTexSubImage2D(
        target: u32,
        level: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        format: u32,
        pixel_type: u32,
        pixels: *const c_void,
    );
    fn glEnableClientState(array: u32);
    fn glVertexPointer(size: i32, kind: u32, stride: i32, pointer: *const c_void);
    fn glTexCoordPointer(size: i32, kind: u32, stride: i32, pointer: *const c_void);
    fn glDrawArrays(mode: u32, first: i32, count: i32);
    fn glGetError() -> u32;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GpuVertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
}

struct Texture {
    name: u32,
    source_ptr: usize,
    width: u32,
    height: u32,
    bytes: usize,
    last_frame: u64,
}

struct State {
    textures: HashMap<u32, Texture>,
    texture_bytes: usize,
    fallback_name: u32,
    fallback_size: (u32, u32),
    frame: u64,
    logical_size: (u32, u32),
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub struct GpuDisplay;

impl GpuDisplay {
    pub fn new() -> Result<Self, String> {
        if !std::path::Path::new("ur0:data/libshacccg.suprx").exists()
            && !std::path::Path::new("ur0:data/external/libshacccg.suprx").exists()
        {
            return Err("vitaGL shader compiler missing; install libshacccg.suprx at ur0:data/libshacccg.suprx".to_owned());
        }
        unsafe {
            // Two scanout surfaces cost about 4 MiB. The default circular pool
            // alone is 32 MiB, so replace it with a measured starting budget.
            vglSetDisplayBufferCount(2);
            vglSetCircularPoolSize(4 * 1024 * 1024);
            // vitaGL returns whether it had to fall back to a smaller
            // display resolution, not whether initialization succeeded.
            let _resolution_fallback = vglInitWithCustomSizes(
                0,
                SCREEN_W,
                SCREEN_H,
                32 * 1024 * 1024,
                16 * 1024 * 1024,
                0,
                0,
                SCE_GXM_MULTISAMPLE_NONE,
            );
            // vitaGL's fixed-function draw path compiles shaders on demand.
            // Its splash/clear can appear even when SceShaccCg failed to load,
            // followed by a hard crash at the first textured draw.
            let shader_status = shark_init(std::ptr::null());
            if shader_status < 0 {
                return Err(format!(
                    "vitaGL shader compiler unavailable ({shader_status:#x}); install libshacccg.suprx at ur0:data/libshacccg.suprx"
                ));
            }
            let error = glGetError();
            if error != 0 {
                super::vita::log(&format!("gpu shader init: OpenGL error {error:#x}"));
            }
        }
        let mut slot = STATE.lock().map_err(|_| "GPU state mutex poisoned")?;
        *slot = Some(State {
            textures: HashMap::new(),
            texture_bytes: 0,
            fallback_name: 0,
            fallback_size: (0, 0),
            frame: 0,
            logical_size: (SCREEN_W as u32, SCREEN_H as u32),
        });
        Ok(Self)
    }
}

impl Drop for GpuDisplay {
    fn drop(&mut self) {
        if let Ok(mut slot) = STATE.lock() {
            if let Some(state) = slot.take() {
                for texture in state.textures.values() {
                    unsafe { glDeleteTextures(1, &texture.name) };
                }
                if state.fallback_name != 0 {
                    unsafe { glDeleteTextures(1, &state.fallback_name) };
                }
            }
        }
    }
}

fn begin_frame(state: &mut State, width: u32, height: u32) -> bool {
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return false;
    }
    state.frame = state.frame.wrapping_add(1);
    state.logical_size = (width, height);
    let scale = (SCREEN_W as f64 / width as f64).min(SCREEN_H as f64 / height as f64);
    let draw_w = (width as f64 * scale).round() as i32;
    let draw_h = (height as f64 * scale).round() as i32;
    let viewport_x = (SCREEN_W - draw_w) / 2;
    let viewport_y = (SCREEN_H - draw_h) / 2;
    unsafe {
        glViewport(0, 0, SCREEN_W, SCREEN_H);
        glDisable(GL_SCISSOR_TEST);
        glClearColor(0.0, 0.0, 0.0, 1.0);
        glClear(GL_COLOR_BUFFER_BIT);
        glViewport(viewport_x, viewport_y, draw_w, draw_h);
        glMatrixMode(GL_PROJECTION);
        glLoadIdentity();
        glOrthof(0.0, width as f32, height as f32, 0.0, -1.0, 1.0);
        glMatrixMode(GL_MODELVIEW);
        glLoadIdentity();
        glEnable(GL_TEXTURE_2D);
        glEnable(GL_BLEND);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        glEnableClientState(GL_VERTEX_ARRAY);
        glEnableClientState(GL_TEXTURE_COORD_ARRAY);
    }
    true
}

fn set_clip(state: &State, clip: &[i32; 4]) -> bool {
    let (width, height) = state.logical_size;
    let left = clip[0].clamp(0, width as i32);
    let top = clip[1].clamp(0, height as i32);
    let right = clip[2].clamp(0, width as i32);
    let bottom = clip[3].clamp(0, height as i32);
    if left >= right || top >= bottom {
        return false;
    }
    let scale = (SCREEN_W as f64 / width as f64).min(SCREEN_H as f64 / height as f64);
    let draw_w = (width as f64 * scale).round() as i32;
    let draw_h = (height as f64 * scale).round() as i32;
    let x0 = (SCREEN_W - draw_w) / 2 + (left as f64 * draw_w as f64 / width as f64).floor() as i32;
    let x1 = (SCREEN_W - draw_w) / 2 + (right as f64 * draw_w as f64 / width as f64).ceil() as i32;
    let y0 = (SCREEN_H - draw_h) / 2
        + ((height as i32 - bottom) as f64 * draw_h as f64 / height as f64).floor() as i32;
    let y1 = (SCREEN_H - draw_h) / 2
        + ((height as i32 - top) as f64 * draw_h as f64 / height as f64).ceil() as i32;
    unsafe {
        glEnable(GL_SCISSOR_TEST);
        glScissor(x0, y0, x1 - x0, y1 - y0);
    }
    true
}

fn draw_quad(texture: u32, vertices: &[GpuVertex; 4], alpha: f32) {
    unsafe {
        glBindTexture(GL_TEXTURE_2D, texture);
        glColor4f(1.0, 1.0, 1.0, alpha.clamp(0.0, 1.0));
        glVertexPointer(
            2,
            GL_FLOAT,
            size_of::<GpuVertex>() as i32,
            vertices.as_ptr().cast(),
        );
        glTexCoordPointer(
            2,
            GL_FLOAT,
            size_of::<GpuVertex>() as i32,
            (&vertices[0].u as *const f32).cast(),
        );
        glDrawArrays(GL_TRIANGLE_FAN, 0, 4);
    }
}

fn make_texture(width: u32, height: u32, pixels: *const u8) -> Option<u32> {
    let mut name = 0;
    unsafe {
        let prior_error = glGetError();
        if prior_error != 0 {
            super::vita::log(&format!("gpu before texture: OpenGL error {prior_error:#x}"));
        }
        glGenTextures(1, &mut name);
        if name == 0 {
            return None;
        }
        glBindTexture(GL_TEXTURE_2D, name);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_RGBA as i32,
            width as i32,
            height as i32,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            pixels.cast(),
        );
        let error = glGetError();
        if error != 0 {
            super::vita::log(&format!("gpu texture: OpenGL error {error:#x}"));
            glDeleteTextures(1, &name);
            return None;
        }
    }
    Some(name)
}

fn cached_texture(
    state: &mut State,
    key: u32,
    pixels: *const u8,
    width: u32,
    height: u32,
) -> Option<u32> {
    let bytes = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if bytes > GPU_TEXTURE_BUDGET {
        return None;
    }
    if let Some(existing) = state.textures.get_mut(&key) {
        if existing.source_ptr == pixels as usize
            && existing.width == width
            && existing.height == height
        {
            existing.last_frame = state.frame;
            return Some(existing.name);
        }
    }
    if let Some(old) = state.textures.remove(&key) {
        state.texture_bytes -= old.bytes;
        unsafe { glDeleteTextures(1, &old.name) };
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
        unsafe { glDeleteTextures(1, &old.name) };
    }
    let name = make_texture(width, height, pixels)?;
    state.texture_bytes += bytes;
    state.textures.insert(
        key,
        Texture {
            name,
            source_ptr: pixels as usize,
            width,
            height,
            bytes,
            last_frame: state.frame,
        },
    );
    Some(name)
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
    pixels: *const u8,
    width: u32,
    height: u32,
    vertices: *const GpuVertex,
    clip: *const i32,
    alpha: f32,
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
    let clip = unsafe { &*(clip as *const [i32; 4]) };
    if !set_clip(state, clip) {
        return true;
    }
    let Some(texture) = cached_texture(state, key, pixels, width, height) else {
        return false;
    };
    let vertices = unsafe { &*(vertices as *const [GpuVertex; 4]) };
    draw_quad(texture, vertices, alpha);
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn siglus_vita_gpu_end(wait_vsync: bool) {
    if let Ok(slot) = STATE.lock() {
        if slot.is_some() {
            unsafe {
                vglWaitVblankStart(if wait_vsync { GL_TRUE } else { GL_FALSE });
                vglSwapBuffers(GL_FALSE);
            }
        }
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
    if !begin_frame(state, width, height) {
        return;
    }
    let name = if state.fallback_name == 0 || state.fallback_size != (width, height) {
        if state.fallback_name != 0 {
            unsafe { glDeleteTextures(1, &state.fallback_name) };
        }
        let Some(name) = make_texture(width, height, pixels) else {
            super::vita::log("gpu fallback: texture create failed");
            return;
        };
        state.fallback_name = name;
        state.fallback_size = (width, height);
        name
    } else {
        unsafe {
            glBindTexture(GL_TEXTURE_2D, state.fallback_name);
            glTexSubImage2D(
                GL_TEXTURE_2D,
                0,
                0,
                0,
                width as i32,
                height as i32,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                pixels.cast(),
            );
        }
        state.fallback_name
    };
    unsafe { glDisable(GL_SCISSOR_TEST) };
    let vertices = [
        GpuVertex {
            x: 0.0,
            y: 0.0,
            u: 0.0,
            v: 0.0,
        },
        GpuVertex {
            x: width as f32,
            y: 0.0,
            u: 1.0,
            v: 0.0,
        },
        GpuVertex {
            x: width as f32,
            y: height as f32,
            u: 1.0,
            v: 1.0,
        },
        GpuVertex {
            x: 0.0,
            y: height as f32,
            u: 0.0,
            v: 1.0,
        },
    ];
    draw_quad(name, &vertices, 1.0);
    unsafe {
        vglWaitVblankStart(if wait_vsync { GL_TRUE } else { GL_FALSE });
        vglSwapBuffers(GL_FALSE);
    }
}

pub fn texture_bytes() -> usize {
    STATE
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|v| v.texture_bytes))
        .unwrap_or(0)
}
