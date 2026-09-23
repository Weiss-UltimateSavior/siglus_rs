//! Opcode modules, addressed by `(modtype, module)`.

pub mod grp;
pub mod jmp;
pub mod mem;
pub mod menu;
pub mod msg;
pub mod sel;
pub mod shk;
pub mod sound;
pub mod str;
pub mod sys;

use anyhow::Result;

use crate::bytecode::Command;
use crate::machine::{Machine, Next};

pub fn dispatch(machine: &mut Machine, command: &Command) -> Result<Next> {
    let op = command.op;
    match (op.modtype, op.module) {
        (0, 1 | 5 | 6) => jmp::dispatch(machine, command),
        (0, 2) => sel::dispatch(machine, command),
        (0, 3) => msg::dispatch(machine, command),
        (0, 4) => sys::dispatch_event_loop(machine, command),
        (1, 4) => sys::dispatch(machine, command),
        (1, 10) => str::dispatch(machine, command),
        (1, 11) => mem::dispatch(machine, command),
        (1, 12 | 13) => shk::dispatch(machine, command),
        (1, 20) => sound::bgm(machine, command),
        (1, 21) => sound::pcm(machine, command),
        (1, 22) => sound::se(machine, command),
        (1, 23) => sound::koe(machine, command),
        (1, 33) => grp::dispatch(machine, command),
        _ => machine.unimplemented(command),
    }
}
