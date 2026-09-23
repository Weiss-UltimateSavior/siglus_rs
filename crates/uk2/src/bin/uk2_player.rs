use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::JoinHandle;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use anyhow::{Context, Result, bail};
use encoding_rs::SHIFT_JIS;
use kira::manager::backend::DefaultBackend;
use kira::manager::{AudioManager, AudioManagerSettings};
use kira::sound::static_sound::StaticSoundHandle;
use kira::tween::Tween;
use pollster::block_on;
use uk2::pdt::{Pdt34Header, composite_pdt34_sprite};
use uk2::{
    MES_CODE_OFFSET, Pdt34Image, ResolvedArg, RuntimeValue, ServiceCommand, Uk2Game, Uk2Host,
    Uk2MapLayout, Uk2Memory, Uk2MusicFile, Uk2Vm,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

const SHADER: &str = r#"
@group(0) @binding(0) var frame_texture: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    var uvs = array<vec2<f32>, 6>(
        vec2(0.0, 1.0), vec2(1.0, 1.0), vec2(0.0, 0.0),
        vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(1.0, 0.0),
    );
    var output: VertexOutput;
    output.position = vec4(positions[index], 0.0, 1.0);
    output.uv = uvs[index];
    return output;
}

@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(frame_texture, frame_sampler, input.uv);
}
"#;

const INPUT_CONFIRM: u8 = 1 << 0;
const INPUT_CANCEL: u8 = 1 << 1;
const INPUT_LEFT: u8 = 1 << 2;
const INPUT_RIGHT: u8 = 1 << 3;
const INPUT_UP: u8 = 1 << 4;
const INPUT_DOWN: u8 = 1 << 5;
const INPUT_MOUSE_LEFT: u8 = 1 << 6;

fn main() -> Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: uk2_player <game-directory>"))?;
    let event_loop = EventLoop::new()?;
    let app = Uk2App::new(root)?;
    event_loop.run_app(app)?;
    Ok(())
}

struct Uk2App {
    root: PathBuf,
    title: String,
    window: Option<&'static dyn Window>,
    graphics: Option<Graphics>,
    runtime_messages: Option<Receiver<RuntimeMessage>>,
    runtime_thread: Option<JoinHandle<()>>,
    input: Arc<AtomicU8>,
    pointer: Arc<AtomicU32>,
}

impl Uk2App {
    fn new(root: PathBuf) -> Result<Self> {
        Ok(Self {
            root,
            title: "uk2 player".to_owned(),
            window: None,
            graphics: None,
            runtime_messages: None,
            runtime_thread: None,
            input: Arc::new(AtomicU8::new(0)),
            pointer: Arc::new(AtomicU32::new(0)),
        })
    }
}

impl ApplicationHandler for Uk2App {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = event_loop
            .create_window(
                WindowAttributes::default()
                    .with_title("uk2 player")
                    .with_resizable(true)
                    .with_surface_size(PhysicalSize::new(640, 400))
                    .with_min_surface_size(PhysicalSize::new(320, 200)),
            )
            .expect("create uk2 window");
        let window: &'static dyn Window = Box::leak(window);
        // The PC-98 runtime starts with its software mouse cursor hidden.
        window.set_cursor_visible(false);
        let setup = async {
            let graphics = Graphics::new(window).await?;
            let game = Uk2Game::open(&self.root)?;
            let start = game.start_mes()?;
            Ok::<_, anyhow::Error>((graphics, game, start))
        };
        match block_on(setup) {
            Ok((graphics, game, start)) => {
                let (message_tx, message_rx) = mpsc::sync_channel(2);
                let input = Arc::clone(&self.input);
                let pointer = Arc::clone(&self.pointer);
                let runtime_thread =
                    std::thread::Builder::new()
                        .name("uk2-vm".to_owned())
                        .spawn(move || {
                            let result = (|| -> Result<u16> {
                                let mut host =
                                    DesktopHost::new(game, message_tx.clone(), input, pointer)?;
                                let mut vm = Uk2Vm::new(start);
                                vm.run(&mut host)
                            })();
                            let message = match result {
                                Ok(status) => RuntimeMessage::Finished(status),
                                Err(error) => RuntimeMessage::Error(format!("{error:#}")),
                            };
                            let _ = message_tx.send(message);
                        });
                match runtime_thread {
                    Ok(runtime_thread) => {
                        self.runtime_thread = Some(runtime_thread);
                        self.runtime_messages = Some(message_rx);
                        self.graphics = Some(graphics);
                        self.window = Some(window);
                        window.request_redraw();
                    }
                    Err(error) => {
                        eprintln!("start UK2 runtime thread: {error}");
                        event_loop.exit();
                    }
                }
            }
            Err(error) => {
                eprintln!("initialize UK2 player: {error:#}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        let Some(graphics) = self.graphics.as_mut() else {
            return;
        };
        match event {
            WindowEvent::RedrawRequested => {
                if let Err(error) = graphics.render() {
                    eprintln!("render error: {error:#}");
                }
            }
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::SurfaceResized(size) => {
                graphics.resize(size.width.max(1), size.height.max(1));
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if let Some(mask) = input_mask(code) {
                        update_input(&self.input, mask, event.state == ElementState::Pressed);
                    }
                }
            }
            WindowEvent::PointerButton { state, button, .. } => {
                let mask = match button.mouse_button() {
                    Some(MouseButton::Left) => INPUT_CONFIRM | INPUT_MOUSE_LEFT,
                    Some(MouseButton::Right) => INPUT_CANCEL,
                    _ => 0,
                };
                if mask != 0 {
                    update_input(&self.input, mask, state == ElementState::Pressed);
                }
            }
            WindowEvent::PointerMoved { position, .. } => {
                let size = window.surface_size();
                let x = (position.x.max(0.0) as u64 * u64::from(uk2::pdt::PDT34_WIDTH)
                    / u64::from(size.width.max(1)))
                .min(u64::from(uk2::pdt::PDT34_WIDTH - 1)) as u16;
                let y = (position.y.max(0.0) as u64 * u64::from(uk2::pdt::PDT34_HEIGHT)
                    / u64::from(size.height.max(1)))
                .min(u64::from(uk2::pdt::PDT34_HEIGHT - 1)) as u16;
                self.pointer
                    .store(u32::from(x) | (u32::from(y) << 16), Ordering::Relaxed);
            }
            WindowEvent::Focused(false) => self.input.store(0, Ordering::Relaxed),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &dyn ActiveEventLoop) {
        if let Some(messages) = &self.runtime_messages {
            loop {
                match messages.try_recv() {
                    Ok(RuntimeMessage::Frame(image)) => {
                        if let Some(graphics) = self.graphics.as_mut() {
                            graphics.upload(Some(&image));
                        }
                    }
                    Ok(RuntimeMessage::MenuUpdate {
                        choices,
                        selected,
                        x,
                        top,
                        width,
                        row_height,
                    }) => {
                        if let Some(graphics) = self.graphics.as_mut() {
                            graphics.set_menu(Some(MenuOverlay {
                                choices: choices.clone(),
                                selected,
                                x,
                                top,
                                width,
                                row_height,
                            }));
                        }
                        if let Some(window) = self.window {
                            let label = choices.get(selected).map(String::as_str).unwrap_or("");
                            window.set_title(&format!(
                                "{} — {}/{}: {} (↑/↓, Enter)",
                                self.title,
                                selected + 1,
                                choices.len(),
                                label
                            ));
                        }
                    }
                    Ok(RuntimeMessage::MenuClosed) => {
                        if let Some(graphics) = self.graphics.as_mut() {
                            graphics.set_menu(None);
                        }
                        if let Some(window) = self.window {
                            window.set_title(&self.title);
                        }
                    }
                    Ok(RuntimeMessage::TextUpdate { object_id, text }) => {
                        if let Some(graphics) = self.graphics.as_mut() {
                            graphics.set_text(TextOverlay { object_id, text });
                        }
                    }
                    Ok(RuntimeMessage::TextClosed(object_id)) => {
                        if let Some(graphics) = self.graphics.as_mut() {
                            graphics.close_text(object_id);
                        }
                    }
                    Ok(RuntimeMessage::WindowDefined(window)) => {
                        if let Some(graphics) = self.graphics.as_mut() {
                            graphics.define_window(window);
                        }
                    }
                    Ok(RuntimeMessage::CursorVisibility(visible)) => {
                        if let Some(window) = self.window {
                            window.set_cursor_visible(visible);
                        }
                    }
                    Ok(RuntimeMessage::Finished(status)) => {
                        log::info!("UK2 VM finished with status {status}");
                        self.runtime_messages = None;
                        self.runtime_thread = None;
                        break;
                    }
                    Ok(RuntimeMessage::Error(error)) => {
                        eprintln!("UK2 runtime error: {error}");
                        self.runtime_messages = None;
                        self.runtime_thread = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.runtime_messages = None;
                        self.runtime_thread = None;
                        break;
                    }
                }
            }
        }
        if let Some(window) = self.window {
            window.request_redraw();
        }
    }
}

enum RuntimeMessage {
    Frame(Pdt34Image),
    MenuUpdate {
        choices: Vec<String>,
        selected: usize,
        x: usize,
        top: usize,
        width: usize,
        row_height: usize,
    },
    MenuClosed,
    TextUpdate {
        object_id: u16,
        text: String,
    },
    TextClosed(u16),
    WindowDefined(Uk2Window),
    CursorVisibility(bool),
    Finished(u16),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Uk2MenuChoice {
    script_index: usize,
    label: String,
}

#[derive(Debug, Clone)]
struct MenuOverlay {
    choices: Vec<String>,
    selected: usize,
    x: usize,
    top: usize,
    width: usize,
    row_height: usize,
}

#[derive(Debug, Clone)]
struct TextOverlay {
    object_id: u16,
    text: String,
}

#[derive(Debug, Clone, Copy)]
struct Uk2Window {
    object_id: u16,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

struct DesktopHost {
    game: Uk2Game,
    audio: AudioManager<DefaultBackend>,
    message_tx: SyncSender<RuntimeMessage>,
    input: Arc<AtomicU8>,
    pointer: Arc<AtomicU32>,
    current_music: Option<Uk2MusicFile>,
    current_music_handle: Option<StaticSoundHandle>,
    music_volume: u8,
    windows: BTreeMap<u16, Uk2Window>,
    mouse_cursor_enabled: bool,
    current_frame: Option<Pdt34Image>,
    scene_base_frame: Option<Pdt34Image>,
    current_map: Option<Uk2MapLayout>,
    map_object_id: Option<u16>,
    map_scroll: (u16, u16),
    fk_previous_buttons: u8,
    color_table: uk2::ColorTable,
}

impl DesktopHost {
    fn new(
        game: Uk2Game,
        message_tx: SyncSender<RuntimeMessage>,
        input: Arc<AtomicU8>,
        pointer: Arc<AtomicU32>,
    ) -> Result<Self> {
        let color_table = uk2::ColorTable::parse(&game.read("color.tbl1")?)
            .context("load UK2 COLOR.TBL palette banks")?;
        Ok(Self {
            game,
            audio: AudioManager::new(AudioManagerSettings::default())
                .context("init Kira audio manager")?,
            message_tx,
            input,
            pointer,
            current_music: None,
            current_music_handle: None,
            music_volume: 127,
            windows: BTreeMap::new(),
            mouse_cursor_enabled: false,
            current_frame: None,
            scene_base_frame: None,
            current_map: None,
            map_object_id: None,
            map_scroll: (0, 0),
            fk_previous_buttons: 0,
            color_table,
        })
    }
}

impl Uk2Host for DesktopHost {
    fn load_mes(&mut self, name: &[u8]) -> Result<uk2::MesProgram> {
        self.game.load_mes_engine_name(name)
    }

    fn poll(&mut self, memory: &mut Uk2Memory) -> Result<()> {
        let held = self.input.load(Ordering::Relaxed);
        let buttons = held & (INPUT_CONFIRM | INPUT_CANCEL);
        let new_presses = buttons & !self.fk_previous_buttons;
        self.fk_previous_buttons = buttons;
        for (mask, slot) in [(INPUT_CANCEL, 1), (INPUT_CONFIRM, 3)] {
            if new_presses & mask != 0 {
                memory.register_mouse_press(slot)?;
            }
        }
        Ok(())
    }

    fn command(&mut self, command: &ServiceCommand, memory: &mut Uk2Memory) -> Result<u16> {
        match command.opcode {
            uk2::TwoOpcode::W1 => {
                let window = window_from_w1(command)?;
                self.windows.insert(window.object_id, window);
                self.message_tx
                    .send(RuntimeMessage::WindowDefined(window))
                    .context("define UK2 window")?;
                Ok(0)
            }
            uk2::TwoOpcode::U1 => {
                let frame_delay = command_number(command, 0)?;
                let bank_index = command_number(command, 1)?;
                self.transition_palette(frame_delay, bank_index)?;
                Ok(0)
            }
            uk2::TwoOpcode::U0 => {
                let enabled = command_number(command, 0)? != 0;
                if self.mouse_cursor_enabled != enabled {
                    self.mouse_cursor_enabled = enabled;
                    self.message_tx
                        .send(RuntimeMessage::CursorVisibility(enabled))
                        .context("update UK2 mouse cursor visibility")?;
                }
                // The helper's previous-mode value stays in AX; the opcode
                // dispatcher only uses DI for control-flow status.
                Ok(0)
            }
            uk2::TwoOpcode::W5 => {
                let name = direct_string(command, 0)?;
                let frame = self.game.load_pdt34(&name)?;
                self.current_map = None;
                self.map_object_id = None;
                self.map_scroll = (0, 0);
                self.current_frame = Some(frame.clone());
                self.scene_base_frame = Some(frame.clone());
                self.message_tx
                    .send(RuntimeMessage::Frame(frame))
                    .context("send frame to winit event loop")?;
                Ok(0)
            }
            uk2::TwoOpcode::F0 => {
                let name = direct_string(command, 0)?;
                let map = self.game.load_map(&name)?;
                let frame = self.game.render_map(&map, 0, 0)?;
                self.current_map = Some(map);
                self.map_object_id = None;
                self.map_scroll = (0, 0);
                self.current_frame = Some(frame.clone());
                self.scene_base_frame = Some(frame.clone());
                self.message_tx
                    .send(RuntimeMessage::Frame(frame))
                    .context("present UK2 MAP scene")?;
                Ok(0)
            }
            uk2::TwoOpcode::FA => {
                let values = five_values(command)?;
                if let Some(map) = &mut self.current_map {
                    map.apply_fa(values);
                }
                Ok(0)
            }
            uk2::TwoOpcode::FB => {
                let [x, y, object_id, chip, layer] = five_values(command)?;
                if self.map_object_id == Some(object_id) {
                    if let Some(map) = &mut self.current_map {
                        if map.apply_fb(x, y, chip, layer) {
                            let frame =
                                self.game
                                    .render_map(map, self.map_scroll.0, self.map_scroll.1)?;
                            self.current_frame = Some(frame.clone());
                            self.scene_base_frame = Some(frame.clone());
                            self.message_tx
                                .send(RuntimeMessage::Frame(frame))
                                .context("redraw UK2 map after FB")?;
                        }
                    }
                }
                Ok(0)
            }
            uk2::TwoOpcode::FC | uk2::TwoOpcode::FD => {
                let Some(ResolvedArg::Values(values)) = command.arguments.first() else {
                    bail!("uk2: {} requires two trigger values", command.opcode);
                };
                if values.len() != 2 {
                    bail!(
                        "uk2: {} has {} values, expected two",
                        command.opcode,
                        values.len()
                    );
                }
                let object_id = values[0].number()?;
                let trigger_id = values[1].number()?;
                if self.map_object_id == Some(object_id) {
                    if let Some(map) = &mut self.current_map {
                        map.set_trigger_enabled(trigger_id, command.opcode == uk2::TwoOpcode::FD);
                    }
                }
                Ok(0)
            }
            uk2::TwoOpcode::F5 => {
                // sub_1CF65 attaches the map loaded by F0 to a UI object and
                // initializes that object's viewport from map dimensions.
                self.map_object_id = Some(command_number(command, 0)?);
                Ok(0)
            }
            uk2::TwoOpcode::WC => {
                let object_id = command_number(command, 0)?;
                if self.map_object_id == Some(object_id) {
                    if let Some(frame) = &self.current_frame {
                        self.message_tx
                            .send(RuntimeMessage::Frame(frame.clone()))
                            .context("redraw UK2 map object")?;
                    }
                }
                Ok(0)
            }
            uk2::TwoOpcode::FJ => {
                let Some(ResolvedArg::Values(values)) = command.arguments.first() else {
                    bail!("uk2: FJ requires three map viewport values");
                };
                if values.len() != 3 {
                    bail!("uk2: FJ has {} values, expected three", values.len());
                }
                let x = values[0].number()?;
                let y = values[1].number()?;
                let object_id = values[2].number()?;
                if self.map_object_id == Some(object_id) {
                    if let Some(map) = &self.current_map {
                        let viewport_w = map.width.saturating_mul(2).min(80) / 2;
                        let viewport_h = map.height.saturating_mul(16).min(400) / 16;
                        self.map_scroll = (
                            x.saturating_sub(viewport_w / 2)
                                .min(map.width.saturating_sub(viewport_w)),
                            y.saturating_sub(viewport_h / 2)
                                .min(map.height.saturating_sub(viewport_h)),
                        );
                        let frame =
                            self.game
                                .render_map(map, self.map_scroll.0, self.map_scroll.1)?;
                        self.current_frame = Some(frame.clone());
                        self.scene_base_frame = Some(frame.clone());
                        self.message_tx
                            .send(RuntimeMessage::Frame(frame))
                            .context("scroll UK2 map viewport")?;
                    }
                }
                Ok(0)
            }
            uk2::TwoOpcode::FK => {
                // Actor/map frame updates still need the full sub_1DB80
                // port. The IRQ-like button events are pumped above.
                std::thread::sleep(std::time::Duration::from_millis(16));
                Ok(0)
            }
            uk2::TwoOpcode::U3 => {
                let Some(ResolvedArg::Values(values)) = command.arguments.first() else {
                    bail!("uk2: U3 requires two display values");
                };
                if values.len() != 2 {
                    bail!("uk2: U3 has {} values, expected two", values.len());
                }
                let _transition = values[0].number()?;
                let _display_mode = values[1].number()?;
                let name = direct_string(command, 1)?;
                let frame = self.game.load_pdt34(&name)?;
                self.current_map = None;
                self.map_object_id = None;
                self.current_frame = Some(frame.clone());
                self.scene_base_frame = Some(frame.clone());
                self.message_tx
                    .send(RuntimeMessage::Frame(frame))
                    .context("display UK2 U3 image")?;
                Ok(0)
            }
            uk2::TwoOpcode::UE => {
                let resources = command
                    .arguments
                    .iter()
                    .find_map(|argument| match argument {
                        ResolvedArg::Resources(resources) => Some(resources),
                        _ => None,
                    })
                    .ok_or_else(|| anyhow::anyhow!("uk2: UE has no resource list"))?;
                let frame = self.current_frame.get_or_insert_with(blank_frame);
                for resource in resources {
                    let name = uk2::Uk2Game::decode_engine_name(resource.resource.string()?)?;
                    let x = resource
                        .arguments
                        .first()
                        .map(RuntimeValue::number)
                        .transpose()?
                        .unwrap_or(1000);
                    let y = resource
                        .arguments
                        .get(1)
                        .map(RuntimeValue::number)
                        .transpose()?
                        .unwrap_or(1000);
                    let bytes = self.game.read(&name)?;
                    let header = Pdt34Header::parse(&bytes)?;
                    let sprite = header.decode_image(&bytes)?;
                    composite_pdt34_sprite(frame, &sprite, header.rect, x, y)
                        .with_context(|| format!("uk2: composite UE resource {name:?}"))?;
                }
                self.message_tx
                    .send(RuntimeMessage::Frame(
                        self.current_frame
                            .as_ref()
                            .expect("UE frame initialized")
                            .clone(),
                    ))
                    .context("send UE composition to winit event loop")?;
                Ok(0)
            }
            uk2::TwoOpcode::FI => {
                // sub_1C0B0 restores the backing image over byte columns
                // 6..69 and scanlines 16..239 before drawing a named PDT.
                let name = direct_string(command, 0)?;
                let bytes = self.game.read(&name)?;
                let header = Pdt34Header::parse(&bytes)?;
                let image = header.decode_image(&bytes)?;
                let frame = self.current_frame.get_or_insert_with(blank_frame);
                if let Some(base) = &self.scene_base_frame {
                    restore_fi_region(frame, base);
                }
                composite_pdt34_sprite(frame, &image, header.rect, 1000, 1000)
                    .with_context(|| format!("uk2: composite FI resource {name:?}"))?;
                self.message_tx
                    .send(RuntimeMessage::Frame(frame.clone()))
                    .context("display UK2 FI image")?;
                Ok(0)
            }
            uk2::TwoOpcode::U2 => {
                let object_id = command_number(command, 0)?;
                let text = command_text(command)?;
                if !text.is_empty() {
                    self.message_tx
                        .send(RuntimeMessage::TextUpdate { object_id, text })
                        .context("send UK2 text to winit event loop")?;
                }
                Ok(0)
            }
            uk2::TwoOpcode::W6 => {
                let object_id = command_number(command, 0)?;
                let text = command_direct_text(command, 1)?;
                if !text.is_empty() {
                    self.message_tx
                        .send(RuntimeMessage::TextUpdate { object_id, text })
                        .context("send UK2 W6 text object to winit event loop")?;
                }
                Ok(0)
            }
            uk2::TwoOpcode::W7 => {
                // W7 arms the engine's wait on the given UI object. In the
                // reference loop the interpreter does not advance until the
                // player produces a fresh confirm/cancel input edge.
                let _object_id = command_number(command, 0)?;
                self.wait_for_text_advance();
                memory.clear_mouse_events();
                self.fk_previous_buttons =
                    self.input.load(Ordering::Relaxed) & (INPUT_CONFIRM | INPUT_CANCEL);
                Ok(0)
            }
            uk2::TwoOpcode::W2 => {
                // sub_213B6 waits for a key or left-button event, then
                // sub_213D4 clears events only after all buttons are up.
                self.wait_for_text_advance();
                while self.input.load(Ordering::Relaxed) & (INPUT_CONFIRM | INPUT_CANCEL) != 0 {
                    std::thread::sleep(std::time::Duration::from_millis(16));
                }
                memory.clear_mouse_events();
                self.fk_previous_buttons = 0;
                Ok(0)
            }
            uk2::TwoOpcode::WD => {
                let _repeat_count = command_number(command, 0)?;
                if let Some(frame) = &self.current_frame {
                    self.message_tx
                        .send(RuntimeMessage::Frame(frame.clone()))
                        .context("flush UK2 display")?;
                }
                Ok(0)
            }
            uk2::TwoOpcode::W4 => {
                if let Some(object_id) = command
                    .arguments
                    .iter()
                    .find_map(|argument| match argument {
                        ResolvedArg::Values(values) => values.first(),
                        _ => None,
                    })
                    .and_then(|value| value.number().ok())
                {
                    let _ = self.message_tx.send(RuntimeMessage::TextClosed(object_id));
                }
                Ok(0)
            }
            uk2::TwoOpcode::WA => {
                let object_id = command_number(command, 0)?;
                self.message_tx
                    .send(RuntimeMessage::TextClosed(object_id))
                    .context("close UK2 UI object")?;
                Ok(0)
            }
            uk2::TwoOpcode::M0 => {
                let name = direct_string(command, 0)?;
                self.stop_music();
                let music = self.game.load_configured_music(&name)?;
                self.current_music = Some(music);
                Ok(0)
            }
            uk2::TwoOpcode::M1 => {
                let volume = command
                    .arguments
                    .iter()
                    .find_map(|argument| match argument {
                        ResolvedArg::Values(values) => values.first(),
                        _ => None,
                    })
                    .ok_or_else(|| anyhow::anyhow!("uk2: M1 has no volume value"))?
                    .number()?;
                self.music_volume = clamp_music_volume(volume);
                if let Some(handle) = &mut self.current_music_handle {
                    handle.set_volume(self.music_volume as f64 / 127.0, Tween::default());
                }
                Ok(0)
            }
            uk2::TwoOpcode::M2 => {
                self.stop_music();
                Ok(0)
            }
            uk2::TwoOpcode::M4 => {
                // The reference helper sends driver function 0 to start the
                // currently loaded score. Playback is not implemented yet.
                if self
                    .current_music_handle
                    .as_ref()
                    .is_some_and(|handle| handle.state() != kira::sound::PlaybackState::Stopped)
                {
                    let handle = self
                        .current_music_handle
                        .as_mut()
                        .expect("active music handle");
                    handle.seek_to(0.0);
                }
                Ok(0)
            }
            uk2::TwoOpcode::M5 => {
                // This legacy path is unused in the supplied game scripts.
                Ok(0)
            }
            uk2::TwoOpcode::UB => {
                let exists = direct_string(command, 1)
                    .ok()
                    .map(|name| self.game.contains_resource(&name))
                    .unwrap_or(false);
                if let Some(ResolvedArg::Direct { operand, .. }) = command.arguments.first() {
                    memory.write_operand(
                        operand,
                        RuntimeValue::Number {
                            value: if exists { 0 } else { 1 },
                            width: uk2::vm::NumericWidth::Word,
                        },
                    )?;
                }
                Ok(0)
            }
            uk2::TwoOpcode::U9 => {
                let pressed = self.input.load(Ordering::Relaxed) & INPUT_CONFIRM != 0;
                if let Some(ResolvedArg::Direct { operand, .. }) = command.arguments.first() {
                    memory.write_operand(
                        operand,
                        RuntimeValue::Number {
                            value: u16::from(pressed),
                            width: uk2::vm::NumericWidth::Word,
                        },
                    )?;
                }
                Ok(0)
            }
            uk2::TwoOpcode::W8 => {
                let choices = menu_choices(command)?;
                if choices.is_empty() {
                    bail!("uk2: W8 menu has no selectable options");
                }
                let object_id = command_number(command, 1)?;
                let width = (usize::from(command_number(command, 2)?) * 16).clamp(80, 640);
                let row_height = if command_number(command, 3)? == 0 {
                    24
                } else {
                    16
                };
                let (x, top) = self
                    .windows
                    .get(&object_id)
                    .map(|window| (window.x, window.y))
                    .unwrap_or((0, 0));
                let selected = self.wait_for_menu(&choices, x, top, width, row_height)?;
                memory.clear_mouse_events();
                self.fk_previous_buttons =
                    self.input.load(Ordering::Relaxed) & (INPUT_CONFIRM | INPUT_CANCEL);
                if let Some(ResolvedArg::Direct { operand, .. }) = command.arguments.first() {
                    memory.write_operand(
                        operand,
                        RuntimeValue::Number {
                            value: selected,
                            width: uk2::vm::NumericWidth::Word,
                        },
                    )?;
                }
                Ok(0)
            }
            other => bail!(
                "uk2: desktop host has not implemented {other} at MES offset {:#x}",
                command.offset
            ),
        }
    }
}

fn restore_fi_region(frame: &mut Pdt34Image, base: &Pdt34Image) {
    const WIDTH: usize = 640;
    for y in 16..240 {
        let first_pixel = y * WIDTH + 6 * 8;
        let last_pixel = y * WIDTH + 70 * 8;
        frame.indexed[first_pixel / 2..last_pixel / 2]
            .copy_from_slice(&base.indexed[first_pixel / 2..last_pixel / 2]);
        frame.rgba[first_pixel * 4..last_pixel * 4]
            .copy_from_slice(&base.rgba[first_pixel * 4..last_pixel * 4]);
    }
}

fn input_mask(code: KeyCode) -> Option<u8> {
    Some(match code {
        KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space | KeyCode::KeyZ => INPUT_CONFIRM,
        KeyCode::Escape | KeyCode::KeyX => INPUT_CANCEL,
        KeyCode::ArrowLeft | KeyCode::KeyA => INPUT_LEFT,
        KeyCode::ArrowRight | KeyCode::KeyD => INPUT_RIGHT,
        KeyCode::ArrowUp | KeyCode::KeyW => INPUT_UP,
        KeyCode::ArrowDown | KeyCode::KeyS => INPUT_DOWN,
        _ => return None,
    })
}

fn update_input(input: &AtomicU8, mask: u8, pressed: bool) {
    if pressed {
        input.fetch_or(mask, Ordering::Relaxed);
    } else {
        input.fetch_and(!mask, Ordering::Relaxed);
    }
}

fn move_menu_selection(selected: usize, option_count: usize, pressed: u8) -> usize {
    if option_count == 0 {
        return 0;
    }
    if pressed & INPUT_UP != 0 {
        (selected + option_count - 1) % option_count
    } else if pressed & INPUT_DOWN != 0 {
        (selected + 1) % option_count
    } else {
        selected.min(option_count - 1)
    }
}

fn menu_row_at(
    x: usize,
    y: usize,
    option_count: usize,
    selected: usize,
    panel_x: usize,
    top: usize,
    panel_width: usize,
    row_height: usize,
) -> Option<usize> {
    if option_count == 0 || !(panel_x..panel_x.saturating_add(panel_width)).contains(&x) {
        return None;
    }
    let row_height = row_height.clamp(16, 24);
    let screen_height = uk2::pdt::PDT34_HEIGHT as usize;
    let capacity = ((screen_height.saturating_sub(top + 12)) / row_height).max(1);
    let line_count = option_count.min(capacity);
    let first_row = if selected >= line_count {
        (selected + 1 - line_count).min(option_count - line_count)
    } else {
        0
    };
    let panel_height = line_count * row_height + 12;
    let panel_y = top.min(screen_height.saturating_sub(panel_height));
    let rows_top = panel_y + 6;
    let rows_bottom = rows_top + line_count * row_height;
    if !(rows_top..rows_bottom).contains(&y) {
        return None;
    }
    let row = (y - rows_top) / row_height;
    (first_row + row < option_count).then_some(first_row + row)
}

impl DesktopHost {
    fn stop_music(&mut self) {
        if let Some(mut handle) = self.current_music_handle.take() {
            let _ = handle.stop(Tween::default());
        }
    }

    fn transition_palette(&mut self, frame_delay: u16, bank_index: u16) -> Result<()> {
        let target = *self.color_table.bank(bank_index)?;
        let Some(base_frame) = self.current_frame.clone() else {
            return Ok(());
        };
        for step in 1..=16 {
            let mut frame = base_frame.clone();
            frame.rgba = uk2::color::transition_rgba(
                &base_frame.rgba,
                &base_frame.indexed,
                &target,
                step,
                16,
            )?;
            self.current_frame = Some(frame.clone());
            self.message_tx
                .send(RuntimeMessage::Frame(frame))
                .context("send palette transition frame to winit event loop")?;
            if frame_delay != 0 && step != 16 {
                std::thread::sleep(std::time::Duration::from_millis(
                    u64::from(frame_delay).saturating_mul(16),
                ));
            }
        }
        Ok(())
    }

    fn wait_for_text_advance(&self) {
        let mut previous = self.input.load(Ordering::Relaxed);
        loop {
            let current = self.input.load(Ordering::Relaxed);
            let pressed = current & !previous;
            if pressed & (INPUT_CONFIRM | INPUT_CANCEL) != 0 {
                return;
            }
            previous = current;
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }

    fn wait_for_menu(
        &self,
        choices: &[Uk2MenuChoice],
        panel_x: usize,
        top: usize,
        width: usize,
        row_height: usize,
    ) -> Result<u16> {
        let mut selected = 0usize;
        let labels = choices
            .iter()
            .map(|choice| choice.label.clone())
            .collect::<Vec<_>>();
        self.message_tx
            .send(RuntimeMessage::MenuUpdate {
                choices: labels.clone(),
                selected,
                x: panel_x,
                top,
                width,
                row_height,
            })
            .context("show UK2 menu")?;
        let mut previous = self.input.load(Ordering::Relaxed);
        loop {
            let current = self.input.load(Ordering::Relaxed);
            let pressed = current & !previous;
            previous = current;
            if pressed & INPUT_MOUSE_LEFT != 0 {
                let packed = self.pointer.load(Ordering::Relaxed);
                let pointer_x = (packed & 0xffff) as usize;
                let y = (packed >> 16) as usize;
                if let Some(clicked) = menu_row_at(
                    pointer_x,
                    y,
                    choices.len(),
                    selected,
                    panel_x,
                    top,
                    width,
                    row_height,
                ) {
                    selected = clicked;
                    let _ = self.message_tx.send(RuntimeMessage::MenuClosed);
                    return Ok((choices[selected].script_index + 1) as u16);
                }
                continue;
            }
            if pressed & INPUT_CONFIRM != 0 {
                let _ = self.message_tx.send(RuntimeMessage::MenuClosed);
                return Ok((choices[selected].script_index + 1) as u16);
            }
            if pressed & INPUT_CANCEL != 0 {
                let _ = self.message_tx.send(RuntimeMessage::MenuClosed);
                return Ok(0);
            }
            if pressed & INPUT_UP != 0 {
                selected = move_menu_selection(selected, choices.len(), pressed);
                self.message_tx
                    .send(RuntimeMessage::MenuUpdate {
                        choices: labels.clone(),
                        selected,
                        x: panel_x,
                        top,
                        width,
                        row_height,
                    })
                    .context("update UK2 menu selection")?;
            } else if pressed & INPUT_DOWN != 0 {
                selected = move_menu_selection(selected, choices.len(), pressed);
                self.message_tx
                    .send(RuntimeMessage::MenuUpdate {
                        choices: labels.clone(),
                        selected,
                        x: panel_x,
                        top,
                        width,
                        row_height,
                    })
                    .context("update UK2 menu selection")?;
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }
}

fn command_number(command: &ServiceCommand, index: usize) -> Result<u16> {
    let argument = command
        .arguments
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("uk2: W8 is missing numeric argument #{index}"))?;
    match argument {
        ResolvedArg::Value(value) => value.number(),
        _ => bail!("uk2: W8 argument #{index} is not an evaluated number"),
    }
}

fn five_values(command: &ServiceCommand) -> Result<[u16; 5]> {
    let Some(ResolvedArg::Values(values)) = command.arguments.first() else {
        bail!("uk2: {} requires five values", command.opcode);
    };
    if values.len() != 5 {
        bail!(
            "uk2: {} has {} values, expected five",
            command.opcode,
            values.len()
        );
    }
    let mut numbers = [0u16; 5];
    for (number, value) in numbers.iter_mut().zip(values) {
        *number = value.number()?;
    }
    Ok(numbers)
}

fn window_from_w1(command: &ServiceCommand) -> Result<Uk2Window> {
    let Some(ResolvedArg::Values(values)) = command.arguments.first() else {
        bail!("uk2: W1 requires seven window values");
    };
    if values.len() != 7 {
        bail!("uk2: W1 has {} values, expected seven", values.len());
    }
    let values = values
        .iter()
        .map(RuntimeValue::number)
        .collect::<Result<Vec<_>>>()?;
    let (left, top, right, bottom) = (values[0], values[1], values[2], values[3]);
    if right < left || bottom < top {
        bail!("uk2: W1 window bounds are reversed");
    }
    let width = usize::from(right - left) * 16;
    let height = usize::from(bottom - top) * 16;
    // sub_1669B scales the horizontal coordinates by two eight-pixel units
    // and the vertical coordinates by sixteen pixels.
    Ok(Uk2Window {
        object_id: values[6],
        x: usize::from(left) * 16,
        y: usize::from(top) * 16,
        width: if (80..=640).contains(&width) {
            width
        } else {
            80
        },
        height: if (16..=400).contains(&height) {
            height
        } else {
            16
        },
    })
}

fn clamp_music_volume(value: u16) -> u8 {
    if !(0..=0x7f).contains(&value) {
        // UK2's helper replaces either an out-of-range positive value or a
        // negative signed value with 0x7f before calling the driver.
        0x7f
    } else {
        value as u8
    }
}

fn menu_choices(command: &ServiceCommand) -> Result<Vec<Uk2MenuChoice>> {
    let Some(ResolvedArg::Values(values)) = command.arguments.get(4) else {
        bail!("uk2: W8 is missing its option string list");
    };
    let mut choices = Vec::new();
    for (script_index, value) in values.iter().enumerate() {
        let bytes = value.string()?;
        // The original dialog parser skips entries prefixed by "\\0".
        if bytes.starts_with(b"\\0") {
            continue;
        }
        let (text, _, had_errors) = SHIFT_JIS.decode(bytes);
        if had_errors {
            bail!("uk2: W8 option contains invalid Shift-JIS");
        }
        choices.push(Uk2MenuChoice {
            script_index,
            label: strip_menu_markup(&text),
        });
    }
    Ok(choices)
}

fn command_text(command: &ServiceCommand) -> Result<String> {
    let values = command
        .arguments
        .iter()
        .find_map(|argument| match argument {
            ResolvedArg::Values(values) => Some(values),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("uk2: U2 has no text value list"))?;
    let mut output = String::new();
    let mut substitutions = Vec::new();
    for value in values {
        match value {
            RuntimeValue::String(bytes) => {
                let (text, _, had_errors) = SHIFT_JIS.decode(bytes);
                if had_errors {
                    bail!("uk2: U2 text contains invalid Shift-JIS");
                }
                output.push_str(&strip_dialog_markup(&text));
            }
            RuntimeValue::Number { value, .. } => substitutions.push(value),
        }
    }
    let mut result = String::with_capacity(output.len());
    let mut values = substitutions.into_iter();
    for character in output.chars() {
        if character == '\u{e000}' {
            if let Some(value) = values.next() {
                result.push_str(&value.to_string());
            }
        } else {
            result.push(character);
        }
    }
    Ok(result.trim().to_owned())
}

fn command_direct_text(command: &ServiceCommand, index: usize) -> Result<String> {
    let argument = command
        .arguments
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("uk2: W6 is missing direct text argument #{index}"))?;
    let ResolvedArg::Direct { value, .. } = argument else {
        bail!("uk2: W6 argument #{index} is not a direct string");
    };
    let (text, _, had_errors) = SHIFT_JIS.decode(value.string()?);
    if had_errors {
        bail!("uk2: W6 text contains invalid Shift-JIS");
    }
    Ok(strip_dialog_markup(&text).trim().to_owned())
}

fn strip_dialog_markup(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            output.push('\n');
            continue;
        }
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        let Some(command) = chars.next() else {
            break;
        };
        match command {
            'r' => output.push('\n'),
            'k' => {
                let mut payload = String::new();
                while let Some(value) = chars.next() {
                    if value == '\\' && chars.peek() == Some(&'K') {
                        chars.next();
                        break;
                    }
                    payload.push(value);
                }
                if payload.contains("%d") {
                    output.push('\u{e000}');
                }
            }
            'C' => {
                if chars.peek().is_some_and(|value| value.is_ascii_digit()) {
                    chars.next();
                }
            }
            '\\' => output.push('\\'),
            _ => {}
        }
    }
    output
}

fn strip_menu_markup(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        let Some(command) = chars.next() else {
            break;
        };
        if chars.peek() == Some(&'(') {
            chars.next();
            for value in chars.by_ref() {
                if value == ')' {
                    break;
                }
            }
        } else if command == '\\' {
            output.push('\\');
        }
    }
    output.trim().to_owned()
}

fn direct_string(command: &ServiceCommand, index: usize) -> Result<String> {
    let ResolvedArg::Direct { value, .. } = command
        .arguments
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("uk2: missing direct argument #{index}"))?
    else {
        bail!("uk2: argument #{index} is not direct");
    };
    let bytes = value.string()?;
    let (text, _, had_errors) = SHIFT_JIS.decode(bytes);
    if had_errors {
        bail!("uk2: direct argument #{index} is not valid Shift-JIS");
    }
    Ok(text.into_owned())
}

fn blank_frame() -> Pdt34Image {
    Pdt34Image {
        width: uk2::pdt::PDT34_WIDTH,
        height: uk2::pdt::PDT34_HEIGHT,
        indexed: vec![0; (uk2::pdt::PDT34_WIDTH * uk2::pdt::PDT34_HEIGHT / 2) as usize],
        rgba: vec![0; (uk2::pdt::PDT34_WIDTH * uk2::pdt::PDT34_HEIGHT * 4) as usize],
    }
}

struct Graphics {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    current_frame: Option<Vec<u8>>,
    menu: Option<MenuOverlay>,
    text_overlays: Vec<TextOverlay>,
    windows: BTreeMap<u16, Uk2Window>,
    menu_font: Option<FontVec>,
}

impl Graphics {
    async fn new(window: &'static dyn Window) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = instance.create_surface(window)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("request GPU adapter")?;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("uk2-player-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .context("request GPU device")?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.surface_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: caps.present_modes[0],
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("uk2-frame"),
            size: wgpu::Extent3d {
                width: uk2::pdt::PDT34_WIDTH,
                height: uk2::pdt::PDT34_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("uk2-frame-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uk2-frame-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uk2-frame-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("uk2-present-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("uk2-present-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("uk2-present-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        Ok(Self {
            surface,
            device,
            queue,
            config,
            texture,
            bind_group,
            pipeline,
            current_frame: None,
            menu: None,
            text_overlays: Vec::new(),
            windows: BTreeMap::new(),
            menu_font: load_menu_font(),
        })
    }

    fn upload(&mut self, image: Option<&Pdt34Image>) {
        self.current_frame = image.map(|image| image.rgba.clone());
        self.refresh_texture();
    }

    fn set_menu(&mut self, menu: Option<MenuOverlay>) {
        self.menu = menu;
        self.refresh_texture();
    }

    fn set_text(&mut self, text_overlay: TextOverlay) {
        if let Some(existing) = self
            .text_overlays
            .iter_mut()
            .find(|existing| existing.object_id == text_overlay.object_id)
        {
            *existing = text_overlay;
        } else {
            self.text_overlays.push(text_overlay);
        }
        self.refresh_texture();
    }

    fn close_text(&mut self, object_id: u16) {
        self.text_overlays
            .retain(|overlay| overlay.object_id != object_id);
        self.refresh_texture();
    }

    fn define_window(&mut self, window: Uk2Window) {
        self.windows.insert(window.object_id, window);
        self.refresh_texture();
    }

    fn refresh_texture(&mut self) {
        let Some(mut frame) = self.current_frame.clone() else {
            return;
        };
        if let (Some(menu), Some(font)) = (&self.menu, &self.menu_font) {
            draw_menu_overlay(&mut frame, menu, font);
        }
        if let Some(font) = &self.menu_font {
            for text in &self.text_overlays {
                draw_text_overlay(&mut frame, text, self.windows.get(&text.object_id), font);
            }
        }
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(uk2::pdt::PDT34_WIDTH * 4),
                rows_per_image: Some(uk2::pdt::PDT34_HEIGHT),
            },
            wgpu::Extent3d {
                width: uk2::pdt::PDT34_WIDTH,
                height: uk2::pdt::PDT34_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(wgpu::SurfaceError::OutOfMemory) => bail!("GPU surface out of memory"),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("uk2-present-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("uk2-present-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

fn load_menu_font() -> Option<FontVec> {
    let candidates = [
        "/System/Library/Fonts/Supplemental/NISC18030.ttf",
        "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "C:\\Windows\\Fonts\\msgothic.ttc",
        "C:\\Windows\\Fonts\\YuGothR.ttc",
    ];
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if let Ok(font) = FontVec::try_from_vec_and_index(bytes, 0) {
            return Some(font);
        }
    }
    None
}

fn draw_menu_overlay(frame: &mut [u8], menu: &MenuOverlay, font: &FontVec) {
    if frame.len() != (uk2::pdt::PDT34_WIDTH * uk2::pdt::PDT34_HEIGHT * 4) as usize
        || menu.choices.is_empty()
    {
        return;
    }
    let width = uk2::pdt::PDT34_WIDTH as usize;
    let row_height = menu.row_height.clamp(16, 24);
    let panel_x = menu.x.min(width);
    let panel_width = menu.width.min(width.saturating_sub(panel_x));
    let screen_height = uk2::pdt::PDT34_HEIGHT as usize;
    let capacity = ((screen_height.saturating_sub(menu.top + 12)) / row_height).max(1);
    let line_count = menu.choices.len().min(capacity);
    let first_row = if menu.selected >= line_count {
        (menu.selected + 1 - line_count).min(menu.choices.len() - line_count)
    } else {
        0
    };
    let panel_height = line_count * row_height + 12;
    let panel_y = menu.top.min(screen_height.saturating_sub(panel_height));
    fill_rect(
        frame,
        panel_x,
        panel_y,
        panel_width,
        panel_height,
        [3, 5, 18, 225],
    );
    stroke_rect(
        frame,
        panel_x,
        panel_y,
        panel_width,
        panel_height,
        [206, 188, 120, 255],
    );

    let font_size = row_height as f32;
    let scale = font.as_scaled(PxScale::from(font_size));
    for (row, label) in menu
        .choices
        .iter()
        .skip(first_row)
        .take(line_count)
        .enumerate()
    {
        let y = panel_y + 6 + row * row_height;
        if row + first_row == menu.selected {
            fill_rect(
                frame,
                panel_x + 5,
                y,
                panel_width - 10,
                row_height,
                [47, 59, 92, 230],
            );
        }
        let color = if row + first_row == menu.selected {
            [255, 240, 172, 255]
        } else {
            [238, 238, 232, 255]
        };
        draw_menu_label(
            frame,
            label,
            (panel_x + 12) as f32,
            (y + row_height - 2) as f32,
            font,
            &scale,
            color,
        );
    }
}

fn draw_text_overlay(
    frame: &mut [u8],
    overlay: &TextOverlay,
    window: Option<&Uk2Window>,
    font: &FontVec,
) {
    if frame.len() != (uk2::pdt::PDT34_WIDTH * uk2::pdt::PDT34_HEIGHT * 4) as usize {
        return;
    }
    let (panel_x, panel_y, panel_width, panel_height) = window.map_or_else(
        || (30, uk2::pdt::PDT34_HEIGHT as usize - 122, 580, 98),
        |window| (window.x, window.y, window.width, window.height),
    );
    let screen_width = uk2::pdt::PDT34_WIDTH as usize;
    let screen_height = uk2::pdt::PDT34_HEIGHT as usize;
    let panel_width = panel_width.min(screen_width.saturating_sub(panel_x));
    let panel_height = panel_height.min(screen_height.saturating_sub(panel_y));
    if panel_width == 0 || panel_height == 0 {
        return;
    }
    fill_rect(
        frame,
        panel_x,
        panel_y,
        panel_width,
        panel_height,
        [2, 4, 16, 232],
    );
    stroke_rect(
        frame,
        panel_x,
        panel_y,
        panel_width,
        panel_height,
        [194, 184, 154, 255],
    );
    let scale = font.as_scaled(PxScale::from(16.0));
    let columns = panel_width.saturating_sub(20).div_ceil(8).max(1);
    let max_lines = panel_height.saturating_sub(18).div_ceil(20).max(1);
    let lines = wrap_text(&overlay.text, columns);
    let first_line = lines.len().saturating_sub(max_lines);
    for (line_index, line) in lines.iter().skip(first_line).take(max_lines).enumerate() {
        draw_menu_label(
            frame,
            line,
            (panel_x + 12) as f32,
            (panel_y + 23 + line_index * 20) as f32,
            font,
            &scale,
            [241, 238, 218, 255],
        );
    }
}

fn wrap_text(text: &str, max_columns: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut columns = 0usize;
    for character in text.chars() {
        if character == '\n' {
            lines.push(std::mem::take(&mut line));
            columns = 0;
            continue;
        }
        let char_columns = if character.is_ascii() || ('\u{ff61}'..='\u{ff9f}').contains(&character)
        {
            1
        } else {
            2
        };
        if columns + char_columns > max_columns && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            columns = 0;
        }
        line.push(character);
        columns += char_columns;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn draw_menu_label(
    frame: &mut [u8],
    label: &str,
    mut x: f32,
    baseline: f32,
    font: &FontVec,
    scale: &ab_glyph::PxScaleFont<&FontVec>,
    color: [u8; 4],
) {
    for character in label.chars() {
        let glyph = scale.scaled_glyph(character);
        let advance = scale.h_advance(glyph.id);
        let mut positioned = glyph;
        positioned.position = point(x, baseline);
        if let Some(outline) = font.outline_glyph(positioned) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px >= 0
                    && py >= 0
                    && px < uk2::pdt::PDT34_WIDTH as i32
                    && py < uk2::pdt::PDT34_HEIGHT as i32
                {
                    let alpha = (coverage * f32::from(color[3])).round() as u8;
                    blend_pixel(
                        frame,
                        px as usize,
                        py as usize,
                        [color[0], color[1], color[2], alpha],
                    );
                }
            });
        }
        x += advance;
        if x >= (uk2::pdt::PDT34_WIDTH - 48) as f32 {
            break;
        }
    }
}

fn fill_rect(frame: &mut [u8], x: usize, y: usize, width: usize, height: usize, color: [u8; 4]) {
    let max_x = (x + width).min(uk2::pdt::PDT34_WIDTH as usize);
    let max_y = (y + height).min(uk2::pdt::PDT34_HEIGHT as usize);
    for py in y.min(max_y)..max_y {
        for px in x.min(max_x)..max_x {
            blend_pixel(frame, px, py, color);
        }
    }
}

fn stroke_rect(frame: &mut [u8], x: usize, y: usize, width: usize, height: usize, color: [u8; 4]) {
    if width < 2 || height < 2 {
        return;
    }
    fill_rect(frame, x, y, width, 1, color);
    fill_rect(frame, x, y + height - 1, width, 1, color);
    fill_rect(frame, x, y, 1, height, color);
    fill_rect(frame, x + width - 1, y, 1, height, color);
}

fn blend_pixel(frame: &mut [u8], x: usize, y: usize, color: [u8; 4]) {
    let offset = (y * uk2::pdt::PDT34_WIDTH as usize + x) * 4;
    let alpha = u32::from(color[3]);
    let inverse = 255 - alpha;
    for channel in 0..3 {
        frame[offset + channel] = ((u32::from(frame[offset + channel]) * inverse
            + u32::from(color[channel]) * alpha)
            / 255) as u8;
    }
    frame[offset + 3] = 255;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wopen_window_24_uses_reference_pixel_coordinates() {
        // WOPEN.MES defines object 24 with these seven W1 values.
        let command = ServiceCommand {
            offset: 0x105,
            opcode: uk2::TwoOpcode::W1,
            arguments: vec![ResolvedArg::Values(
                [3, 18, 20, 24, 0, 1, 24]
                    .into_iter()
                    .map(|value| RuntimeValue::Number {
                        value,
                        width: uk2::vm::NumericWidth::Word,
                    })
                    .collect(),
            )],
        };
        let window = window_from_w1(&command).unwrap();
        assert_eq!(window.object_id, 24);
        assert_eq!(
            (window.x, window.y, window.width, window.height),
            (48, 288, 272, 96)
        );
    }

    #[test]
    fn clamps_music_volume_like_the_reference_driver_helper() {
        assert_eq!(clamp_music_volume(0), 0);
        assert_eq!(clamp_music_volume(100), 100);
        assert_eq!(clamp_music_volume(u16::MAX), 0x7f);
        assert_eq!(clamp_music_volume(128), 0x7f);
    }

    #[test]
    fn strips_uk2_display_control_sequences_from_menu_labels() {
        assert_eq!(strip_menu_markup("\\k(15)\\Kはじめから\\r"), "はじめから");
    }

    #[test]
    fn menu_cursor_wraps_while_preserving_original_option_indices() {
        assert_eq!(move_menu_selection(0, 4, INPUT_UP), 3);
        assert_eq!(move_menu_selection(3, 4, INPUT_DOWN), 0);
        assert_eq!(move_menu_selection(9, 0, INPUT_DOWN), 0);
        let visible_options = [
            Uk2MenuChoice {
                script_index: 0,
                label: "one".into(),
            },
            Uk2MenuChoice {
                script_index: 2,
                label: "three".into(),
            },
        ];
        assert_eq!(visible_options[1].script_index + 1, 3);
    }

    #[test]
    fn maps_mouse_coordinates_to_visible_menu_rows() {
        assert_eq!(menu_row_at(0, 6, 3, 0, 0, 0, 144, 20), Some(0));
        assert_eq!(menu_row_at(100, 26, 3, 0, 0, 0, 144, 20), Some(1));
        assert_eq!(menu_row_at(143, 46, 3, 0, 0, 0, 144, 20), Some(2));
        assert_eq!(menu_row_at(144, 46, 3, 0, 0, 0, 144, 20), None);
        assert_eq!(menu_row_at(100, 6, 0, 0, 0, 0, 144, 20), None);
    }

    #[test]
    fn menu_parser_skips_disabled_options_without_renumbering_results() {
        let command = ServiceCommand {
            offset: 0,
            opcode: uk2::TwoOpcode::W8,
            arguments: vec![
                ResolvedArg::Value(RuntimeValue::Number {
                    value: 0,
                    width: uk2::vm::NumericWidth::Word,
                }),
                ResolvedArg::Value(RuntimeValue::Number {
                    value: 0,
                    width: uk2::vm::NumericWidth::Word,
                }),
                ResolvedArg::Value(RuntimeValue::Number {
                    value: 0,
                    width: uk2::vm::NumericWidth::Word,
                }),
                ResolvedArg::Value(RuntimeValue::Number {
                    value: 0,
                    width: uk2::vm::NumericWidth::Word,
                }),
                ResolvedArg::Values(vec![
                    RuntimeValue::String(b"first".to_vec()),
                    RuntimeValue::String(b"\\0disabled".to_vec()),
                    RuntimeValue::String(b"third".to_vec()),
                ]),
            ],
        };
        let choices = menu_choices(&command).unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[1].script_index + 1, 3);
    }

    #[test]
    fn strips_uk2_dialog_control_codes_and_keeps_line_breaks() {
        assert_eq!(
            strip_dialog_markup("\\C7本文\\k(15)\\K続き\\r次の行"),
            "本文続き\n次の行"
        );
    }

    #[test]
    fn decodes_w6_direct_dialog_string_from_its_operand() {
        let command = ServiceCommand {
            offset: 0,
            opcode: uk2::TwoOpcode::W6,
            arguments: vec![
                ResolvedArg::Value(RuntimeValue::Number {
                    value: 24,
                    width: uk2::vm::NumericWidth::Word,
                }),
                ResolvedArg::Direct {
                    operand: uk2::Operand::FixedString(0),
                    value: RuntimeValue::String(b"\\C7Hello\\rWorld".to_vec()),
                },
                ResolvedArg::Value(RuntimeValue::Number {
                    value: 1,
                    width: uk2::vm::NumericWidth::Word,
                }),
            ],
        };
        assert_eq!(command_number(&command, 0).unwrap(), 24);
        assert_eq!(command_direct_text(&command, 1).unwrap(), "Hello\nWorld");
    }

    #[test]
    fn wraps_japanese_and_ascii_dialog_text_by_display_columns() {
        assert_eq!(
            wrap_text("abc日本語", 7),
            vec!["abc日本".to_owned(), "語".to_owned()]
        );
        assert_eq!(
            wrap_text("first\nsecond", 20),
            vec!["first".to_owned(), "second".to_owned()]
        );
    }

    #[test]
    fn fills_uk2_percent_d_markers_from_following_numeric_arguments() {
        let command = ServiceCommand {
            offset: 0,
            opcode: uk2::TwoOpcode::U2,
            arguments: vec![
                ResolvedArg::Value(RuntimeValue::Number {
                    value: 27,
                    width: uk2::vm::NumericWidth::Word,
                }),
                ResolvedArg::Values(vec![
                    RuntimeValue::String(b"HP=\\k%d\\K/\\k%d\\K".to_vec()),
                    RuntimeValue::Number {
                        value: 12,
                        width: uk2::vm::NumericWidth::Word,
                    },
                    RuntimeValue::Number {
                        value: 34,
                        width: uk2::vm::NumericWidth::Word,
                    },
                ]),
            ],
        };
        assert_eq!(command_text(&command).unwrap(), "HP=12/34");
    }
}
