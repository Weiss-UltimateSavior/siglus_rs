#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("siglus_vita_player must be built with cargo-vita for PS Vita");
}

#[cfg(target_os = "vita")]
mod gpu;

// newlib's default heap is too small for RewriteHF's rebuilt 43 MiB scene
// pack plus decoded images and a few 1280x720 video frames. Keep the heap
// below Vita's user-memory budget so vitaGL and system services still fit.
#[cfg(target_os = "vita")]
#[used]
#[unsafe(export_name = "_newlib_heap_size_user")]
pub static NEWLIB_HEAP_SIZE_USER: u32 = 160 * 1024 * 1024;

#[cfg(target_os = "vita")]
mod vita {
    use std::ffi::{CStr, c_char};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::mem::{MaybeUninit, size_of};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use siglus_scene_vm::audio::switch_backend::render_stereo_i16;
    use siglus_scene_vm::host::{SiglusHost, SiglusHostConfig};
    use siglus_scene_vm::render::Renderer;
    use siglus_scene_vm::runtime::input::VmKey;
    use vitasdk_sys::{
        SCE_AUDIO_OUT_MODE_STEREO, SCE_AUDIO_OUT_PORT_TYPE_MAIN, SCE_CTRL_CIRCLE, SCE_CTRL_CROSS,
        SCE_CTRL_DOWN, SCE_CTRL_LEFT, SCE_CTRL_LTRIGGER, SCE_CTRL_RIGHT, SCE_CTRL_RTRIGGER,
        SCE_CTRL_SELECT, SCE_CTRL_SQUARE, SCE_CTRL_START, SCE_CTRL_TRIANGLE, SCE_CTRL_UP,
        SCE_TOUCH_PORT_FRONT, SCE_TOUCH_SAMPLING_STATE_START, SceCtrlData,
        SceKernelFreeMemorySizeInfo, SceTouchData, SceTouchPanelInfo, sceAudioOutOpenPort,
        sceAudioOutOutput, sceAudioOutReleasePort, sceCtrlPeekBufferPositive,
        sceKernelGetFreeMemorySize, sceTouchGetPanelInfo, sceTouchPeek, sceTouchSetSamplingState,
    };

    const WIDTH: usize = 960;
    const HEIGHT: usize = 544;
    const AUDIO_FRAMES: usize = 512;
    // The Kira backend still needs to advance when Vita3K has no usable
    // audio device. One video tick is 768 stereo samples at 48 kHz.
    const SILENT_AUDIO_FRAMES: usize = 768;
    const MAX_LOGICAL_PIXELS: u64 = 1280 * 720;
    const ROOT: &str = "ux0:data/siglus_rs";

    #[repr(C)]
    struct MallInfo {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }

    unsafe extern "C" {
        fn mallinfo() -> MallInfo;
    }

    pub(super) fn log(message: &str) {
        let _ = fs::create_dir_all(ROOT);
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{ROOT}/vita-player.log"))
        {
            let _ = writeln!(file, "{message}");
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn siglus_vita_log_marker(message: *const u8) {
        if message.is_null() {
            return;
        }
        let message = unsafe { CStr::from_ptr(message.cast::<c_char>()) };
        if let Ok(message) = message.to_str() {
            log(message.trim_end());
        }
    }

    fn log_memory(stage: &str) {
        let mut info = SceKernelFreeMemorySizeInfo {
            size: size_of::<SceKernelFreeMemorySizeInfo>() as i32,
            size_user: 0,
            size_cdram: 0,
            size_phycont: 0,
        };
        let code = unsafe { sceKernelGetFreeMemorySize(&mut info) };
        log(&format!(
            "{stage}: rc={code:#x} free-user={} free-cdram={} free-phycont={}",
            info.size_user, info.size_cdram, info.size_phycont
        ));
        let heap = unsafe { mallinfo() };
        log(&format!(
            "{stage}: heap-arena={} heap-used={} heap-free={}",
            heap.arena, heap.uordblks, heap.fordblks
        ));
    }

    fn log_engine_memory(host: &mut SiglusHost, stage: &str) {
        let scene = host.vm_mut().debug_scene_memory_stats();
        let images = host.vm_mut().ctx.images.debug_live_image_memory_stats();
        log(&format!(
            "{stage}: scene-pck={} scene-streams={} image-albums={} image-frames={} image-rgba={} gpu-textures={}",
            scene.scene_pck_bytes,
            scene.cached_scene_streams,
            images.albums,
            images.images,
            images.rgba_bytes,
            super::gpu::texture_bytes(),
        ));
    }

    struct AudioOutput {
        running: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl AudioOutput {
        fn start() -> Option<Self> {
            log("audio: open port begin");
            let port = unsafe {
                sceAudioOutOpenPort(
                    SCE_AUDIO_OUT_PORT_TYPE_MAIN,
                    AUDIO_FRAMES as i32,
                    48_000,
                    SCE_AUDIO_OUT_MODE_STEREO,
                )
            };
            log(&format!("audio: open port result {port:#x}"));
            if port < 0 {
                log(&format!("audio port unavailable: {port:#x}"));
                return None;
            }
            let running = Arc::new(AtomicBool::new(true));
            let worker_running = Arc::clone(&running);
            let worker = std::thread::Builder::new()
                .name("siglus-vita-audio".to_owned())
                .stack_size(128 * 1024)
                .spawn(move || {
                    log("audio: worker started");
                    let mut samples = [0i16; AUDIO_FRAMES * 2];
                    while worker_running.load(Ordering::Relaxed) {
                        unsafe { render_stereo_i16(samples.as_mut_ptr(), AUDIO_FRAMES) };
                        let code = unsafe { sceAudioOutOutput(port, samples.as_ptr().cast()) };
                        if code < 0 {
                            log(&format!("audio output failed: {code:#x}"));
                            break;
                        }
                    }
                    unsafe { sceAudioOutReleasePort(port) };
                });
            match worker {
                Ok(worker) => Some(Self {
                    running,
                    worker: Some(worker),
                }),
                Err(error) => {
                    unsafe { sceAudioOutReleasePort(port) };
                    log(&format!("audio thread failed: {error}"));
                    None
                }
            }
        }
    }

    impl Drop for AudioOutput {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn update_buttons(host: &mut SiglusHost, previous: &mut u32) {
        let mut pad = MaybeUninit::<SceCtrlData>::zeroed();
        let count = unsafe { sceCtrlPeekBufferPositive(0, pad.as_mut_ptr(), 1) };
        if count <= 0 {
            return;
        }
        let buttons = unsafe { pad.assume_init() }.buttons;
        let changed = buttons ^ *previous;
        let mapping = [
            (SCE_CTRL_CROSS, VmKey::Enter, 0),
            (SCE_CTRL_CIRCLE, VmKey::Escape, 1),
            (SCE_CTRL_SQUARE, VmKey::Space, 2),
            (SCE_CTRL_TRIANGLE, VmKey::Tab, 3),
            (SCE_CTRL_LTRIGGER, VmKey::Shift, 6),
            (SCE_CTRL_RTRIGGER, VmKey::Control, 7),
            (SCE_CTRL_START, VmKey::Escape, 10),
            (SCE_CTRL_SELECT, VmKey::Tab, 11),
            (SCE_CTRL_LEFT, VmKey::ArrowLeft, 12),
            (SCE_CTRL_UP, VmKey::ArrowUp, 13),
            (SCE_CTRL_RIGHT, VmKey::ArrowRight, 14),
            (SCE_CTRL_DOWN, VmKey::ArrowDown, 15),
        ];
        for (bit, key, joypad_index) in mapping {
            if changed & bit == 0 {
                continue;
            }
            let down = buttons & bit != 0;
            if down {
                host.key_down(key);
            } else {
                host.key_up(key);
            }
            host.joypad_button(joypad_index, down);
        }
        *previous = buttons;
    }

    fn update_touch(
        host: &mut SiglusHost,
        active: &mut bool,
        last_position: &mut (f64, f64),
        panel: &SceTouchPanelInfo,
    ) {
        let mut touch = MaybeUninit::<SceTouchData>::zeroed();
        let count = unsafe { sceTouchPeek(SCE_TOUCH_PORT_FRONT, touch.as_mut_ptr(), 1) };
        if count <= 0 {
            return;
        }
        let touch = unsafe { touch.assume_init() };
        if touch.reportNum > 0 {
            let report = touch.report[0];
            let range_x = (i32::from(panel.maxDispX) - i32::from(panel.minDispX)).max(1);
            let range_y = (i32::from(panel.maxDispY) - i32::from(panel.minDispY)).max(1);
            let x = ((i32::from(report.x) - i32::from(panel.minDispX)) * WIDTH as i32 / range_x)
                .clamp(0, WIDTH as i32 - 1);
            let y = ((i32::from(report.y) - i32::from(panel.minDispY)) * HEIGHT as i32 / range_y)
                .clamp(0, HEIGHT as i32 - 1);
            let (logical_w, logical_h) = host.logical_size();
            let scale =
                (WIDTH as f64 / f64::from(logical_w)).min(HEIGHT as f64 / f64::from(logical_h));
            let draw_w = (f64::from(logical_w) * scale).round().min(WIDTH as f64);
            let draw_h = (f64::from(logical_h) * scale).round().min(HEIGHT as f64);
            let offset_x = (WIDTH as f64 - draw_w) / 2.0;
            let offset_y = (HEIGHT as f64 - draw_h) / 2.0;
            let logical_x = ((f64::from(x) - offset_x) * f64::from(logical_w) / draw_w)
                .clamp(0.0, f64::from(logical_w) - 1.0);
            let logical_y = ((f64::from(y) - offset_y) * f64::from(logical_h) / draw_h)
                .clamp(0.0, f64::from(logical_h) - 1.0);
            *last_position = (logical_x, logical_y);
            host.touch(if *active { 1 } else { 0 }, logical_x, logical_y);
            *active = true;
        } else if *active {
            host.touch(2, last_position.0, last_position.1);
            *active = false;
        }
    }

    fn run() -> Result<(), String> {
        log("player start");
        log_memory("startup");
        let game_dir = PathBuf::from(ROOT).join("game");
        if !game_dir.join("Scene.pck").exists() {
            return Err(format!("missing {}", game_dir.join("Scene.pck").display()));
        }
        let _display = super::gpu::GpuDisplay::new()?;
        log_memory("after GPU initialization");
        let renderer = Renderer::new(WIDTH as u32, HEIGHT as u32)
            .map_err(|error| format!("renderer init: {error:#}"))?;
        let config = SiglusHostConfig::new(game_dir);
        let mut host = SiglusHost::new_with_renderer_sync(config, renderer)
            .map_err(|error| format!("engine init: {error:#}"))?;
        let (logical_w, logical_h) = host.logical_size();
        if u64::from(logical_w) * u64::from(logical_h) > MAX_LOGICAL_PIXELS {
            return Err(format!(
                "game resolution {logical_w}x{logical_h} exceeds Vita frame budget"
            ));
        }
        host.resize_with_logical_viewport(
            WIDTH as u32,
            HEIGHT as u32,
            1.0,
            logical_w,
            logical_h,
            0,
            0,
            WIDTH as u32,
            HEIGHT as u32,
        );
        log(&format!("game logical size: {logical_w}x{logical_h}"));
        log_memory("after engine init");
        log_engine_memory(&mut host, "after engine init");
        // Vita3K can crash in its host audio implementation before the API
        // returns. Keep real-device audio enabled while allowing emulator
        // rendering/VM diagnosis through an explicit local marker file.
        let _audio = if PathBuf::from(ROOT).join("disable-audio").exists() {
            log("audio disabled by ux0:data/siglus_rs/disable-audio");
            None
        } else {
            log("audio start begin");
            let audio = AudioOutput::start();
            log("audio start complete");
            audio
        };
        let touch_code = unsafe {
            sceTouchSetSamplingState(SCE_TOUCH_PORT_FRONT, SCE_TOUCH_SAMPLING_STATE_START)
        };
        if touch_code < 0 {
            log(&format!("touch sampling unavailable: {touch_code:#x}"));
        }
        let mut panel = MaybeUninit::<SceTouchPanelInfo>::zeroed();
        let panel_code = unsafe { sceTouchGetPanelInfo(SCE_TOUCH_PORT_FRONT, panel.as_mut_ptr()) };
        let panel = (panel_code >= 0 && touch_code >= 0).then(|| unsafe { panel.assume_init() });
        let mut previous_buttons = 0;
        let mut touch_active = false;
        let mut last_touch_position = (0.0, 0.0);
        // Local Vita3K smoke mode: the emulator's macOS window sometimes
        // crashes while dispatching a mouse click. Drive the same host touch
        // path once, after the opening has reached RewriteHF's title screen.
        let title_tap_smoke = PathBuf::from(ROOT).join("title-tap-smoke").exists();
        let mut frame = 0u64;
        let mut silent_samples = [0i16; SILENT_AUDIO_FRAMES * 2];
        loop {
            if frame < 6 {
                log(&format!("frame {frame}: input begin"));
            }
            update_buttons(&mut host, &mut previous_buttons);
            if let Some(panel) = &panel {
                update_touch(
                    &mut host,
                    &mut touch_active,
                    &mut last_touch_position,
                    panel,
                );
            }
            if title_tap_smoke && (6_700..=6_704).contains(&frame) {
                let phase = if frame == 6_700 {
                    0
                } else if frame == 6_704 {
                    2
                } else {
                    1
                };
                host.touch(phase, 445.0, 662.0);
                if phase != 1 {
                    log(&format!("frame {frame}: title smoke touch phase={phase}"));
                }
            }
            if frame < 6 {
                log(&format!("frame {frame}: step begin"));
            }
            if _audio.is_none() {
                unsafe { render_stereo_i16(silent_samples.as_mut_ptr(), SILENT_AUDIO_FRAMES) };
            }
            if host
                .step(16)
                .map_err(|error| format!("engine frame: {error:#}"))?
            {
                break;
            }
            if frame < 6 {
                log(&format!("frame {frame}: step complete"));
            }
            frame += 1;
            if frame % 600 == 0 {
                log_memory(&format!("frame {frame}"));
                log_engine_memory(&mut host, &format!("frame {frame}"));
                log(&format!("frame {frame}: {}", host.debug_status_summary()));
                let stats = host.movie_memory_stats();
                log(&format!(
                    "frame {frame}: movie-streams={} movie-frames={} movie-rgba-bytes={} movie-pcm-bytes={} movie-assets={} movie-previews={}",
                    stats.active_streams,
                    stats.video_frames,
                    stats.video_bytes,
                    stats.audio_pcm_bytes,
                    stats.asset_cache_entries,
                    stats.preview_cache_entries,
                ));
            }
        }
        log_memory("shutdown");
        log("player exit");
        Ok(())
    }

    pub fn main() {
        std::panic::set_hook(Box::new(|info| {
            log(&format!("player panic: {info}"));
        }));
        if let Err(error) = run() {
            log(&format!("player error: {error}"));
            eprintln!("{error}");
        }
    }
}

#[cfg(target_os = "vita")]
fn main() {
    vita::main();
}
