//! Music (1:20), PCM channels (1:21), sound effects (1:22) and voices
//! (1:23).

use anyhow::Result;

use crate::bytecode::Command;
use crate::longop::{Wait, WaitEvent};
use crate::machine::{Machine, Next};
use crate::sound::WAV_CHANNELS;

fn ms(value: i32) -> u64 {
    value.max(0) as u64
}

fn wait(machine: &mut Machine, event: WaitEvent, cancellable: bool) {
    let wait = Wait::event(event);
    let wait = if cancellable { wait.cancellable() } else { wait };
    machine.push_long_op(Box::new(wait));
}

/// Errors from missing or undecodable files are reported, not fatal.
fn report(machine: &mut Machine, result: Result<()>) {
    if let Err(error) = result {
        machine.report(format!("SEEN{:04} line {}: {error:#}", machine.scene_number(), machine.line));
    }
}

pub fn bgm(machine: &mut Machine, command: &Command) -> Result<Next> {
    let now = machine.sys.now();
    match command.op.opcode {
        // bgmLoop, bgmPlayEx, bgmPlay
        0..=2 => {
            let name = machine.str_param(command, 0)?;
            let fade_in = ms(machine.int_param_or(command, 1, 0)?);
            let fade_out = ms(machine.int_param_or(command, 2, 0)?);
            let looped = command.op.opcode != 2;
            let sys = &mut machine.sys;
            let result = sys.sound.bgm_play(&sys.resources, &sys.settings, now, &name, looped, fade_in, fade_out);
            report(machine, result);
            if command.op.opcode == 1 {
                wait(machine, WaitEvent::Bgm, false);
            }
        }
        3 => wait(machine, WaitEvent::Bgm, false),
        4 => machine.store = i32::from(machine.sys.sound.bgm_status(now) == 1),
        5 | 6 => machine.sys.sound.bgm_stop(),
        7 | 107 => machine.store = machine.sys.sound.bgm_status(now),
        8 => {
            let sys = &mut machine.sys;
            let result = sys.sound.bgm_rewind(&sys.resources, &sys.settings, now);
            report(machine, result);
        }
        9 => machine.sys.sound.bgm_pause(now),
        10 => machine.sys.sound.bgm_resume(now),
        11 => machine.store = machine.sys.sound.bgm_volume(now),
        12 => {
            let volume = machine.int_param(command, 0)?;
            let fade = ms(machine.int_param_or(command, 1, 0)?);
            machine.sys.sound.set_bgm_volume(now, volume, fade);
        }
        13 => {
            let fade = ms(machine.int_param_or(command, 0, 0)?);
            machine.sys.sound.set_bgm_volume(now, 255, fade);
        }
        14 => {
            let fade = ms(machine.int_param_or(command, 0, 0)?);
            machine.sys.sound.set_bgm_volume(now, 0, fade);
        }
        105 => {
            let fade = ms(machine.int_param_or(command, 0, 1000)?);
            machine.sys.sound.bgm_fade_out(now, fade);
        }
        106 => {
            let fade = ms(machine.int_param_or(command, 0, 1000)?);
            machine.sys.sound.bgm_fade_out(now, fade);
            wait(machine, WaitEvent::Bgm, false);
        }
        // bgmTimer: milliseconds since the track started.
        200 => machine.store = 0,
        _ => return machine.unimplemented(command),
    }
    Ok(Next::Advance)
}

fn channel(machine: &mut Machine, command: &Command, index: usize) -> Result<usize> {
    Ok(machine.int_param(command, index)?.clamp(0, WAV_CHANNELS as i32 - 1) as usize)
}

pub fn pcm(machine: &mut Machine, command: &Command) -> Result<Next> {
    let now = machine.sys.now();
    match command.op.opcode {
        // wavPlay, wavPlayEx, wavLoop
        0..=2 => {
            let name = machine.str_param(command, 0)?;
            let channel = match Machine::param_count(command) {
                0 | 1 => None,
                _ => Some(channel(machine, command, 1)?),
            };
            let fade_in = ms(machine.int_param_or(command, 2, 0)?);
            let looped = command.op.opcode == 2;
            let sys = &mut machine.sys;
            let played = sys.sound.wav_play(&sys.resources, &sys.settings, now, &name, channel, looped, fade_in);
            match played {
                Ok(channel) if command.op.opcode == 1 => wait(machine, WaitEvent::Wav(channel), false),
                Ok(_) => {}
                Err(error) => report(machine, Err(error)),
            }
        }
        3 => {
            let channel = channel(machine, command, 0)?;
            wait(machine, WaitEvent::Wav(channel), false);
        }
        4 | 7 => {
            let channel = channel(machine, command, 0)?;
            machine.store = i32::from(machine.sys.sound.wav_playing(channel, now));
        }
        5 if Machine::param_count(command) == 0 => machine.sys.sound.wav_stop_all(0, now),
        5 | 9 | 10 => {
            let channel = channel(machine, command, 0)?;
            machine.sys.sound.wav_stop(channel, 0, now);
        }
        11 => {
            let channel = channel(machine, command, 0)?;
            machine.store = machine.sys.sound.wav_volume(channel, now);
        }
        12 => {
            let channel = channel(machine, command, 0)?;
            let volume = machine.int_param(command, 1)?;
            let fade = ms(machine.int_param_or(command, 2, 0)?);
            machine.sys.sound.set_wav_volume(channel, now, volume, fade);
        }
        13 | 14 => {
            let channel = channel(machine, command, 0)?;
            let fade = ms(machine.int_param_or(command, 1, 0)?);
            let volume = if command.op.opcode == 13 { 255 } else { 0 };
            machine.sys.sound.set_wav_volume(channel, now, volume, fade);
        }
        20 => machine.sys.sound.wav_stop_all(0, now),
        105 | 106 => {
            let channel = channel(machine, command, 0)?;
            let fade = ms(machine.int_param_or(command, 1, 1000)?);
            machine.sys.sound.wav_stop(channel, fade, now);
        }
        _ => return machine.unimplemented(command),
    }
    Ok(Next::Advance)
}

pub fn se(machine: &mut Machine, command: &Command) -> Result<Next> {
    match command.op.opcode {
        0 => {
            let number = machine.int_param(command, 0)?;
            machine.sys.play_se(number);
        }
        _ => return machine.unimplemented(command),
    }
    Ok(Next::Advance)
}

/// Starts a voice and records it for the backlog.
fn koe_play(machine: &mut Machine, id: i32, character: Option<i32>) {
    let active = machine.sys.text.active;
    machine.sys.text.current_page.voices.push(id);
    let state = &mut machine.sys.text.states[active];
    let (x, y) = (state.x, state.y);
    state.koe_markers.push((id, x, y));
    let sys = &mut machine.sys;
    if sys.should_fast_forward() {
        return;
    }
    if let Some(character) = character {
        if !sys.sound.character_enabled(&sys.settings, character) {
            return;
        }
    }
    let now = sys.now();
    let result = sys.sound.koe_play(&sys.resources, &sys.settings, now, id);
    report(machine, result);
}

pub fn koe(machine: &mut Machine, command: &Command) -> Result<Next> {
    let now = machine.sys.now();
    match command.op.opcode {
        // koePlay, koePlayEx, koePlayExC and the "Do" variants, which play
        // whatever the character switches say.
        0 | 1 | 7..=10 => {
            let id = machine.int_param(command, 0)?;
            let character = match Machine::param_count(command) {
                0 | 1 => None,
                _ => Some(machine.int_param(command, 1)?),
            };
            let honour_switches = matches!(command.op.opcode, 0 | 1 | 7);
            koe_play(machine, id, character.filter(|_| honour_switches));
            match command.op.opcode {
                1 | 9 => wait(machine, WaitEvent::Koe, false),
                7 | 10 => wait(machine, WaitEvent::Koe, true),
                _ => {}
            }
        }
        3 => wait(machine, WaitEvent::Koe, false),
        4 => machine.store = i32::from(machine.sys.sound.koe_playing(now)),
        5 => machine.sys.sound.koe_stop(),
        6 => wait(machine, WaitEvent::Koe, true),
        11 => machine.store = machine.sys.sound.koe_volume(now),
        12 => {
            let volume = machine.int_param(command, 0)?;
            let fade = ms(machine.int_param_or(command, 1, 0)?);
            machine.sys.sound.set_koe_volume(now, volume, fade);
        }
        13 | 14 => {
            let fade = ms(machine.int_param_or(command, 0, 0)?);
            let volume = if command.op.opcode == 13 { 255 } else { 0 };
            machine.sys.sound.set_koe_volume(now, volume, fade);
        }
        _ => return machine.unimplemented(command),
    }
    Ok(Next::Advance)
}
