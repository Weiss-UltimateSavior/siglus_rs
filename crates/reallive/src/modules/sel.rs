//! Selections (module 0:2).

use anyhow::Result;

use crate::bytecode::{Command, CommandKind};
use crate::machine::{Machine, Next, SavepointKind};
use crate::select::{self, ObjectButtonSelect};

fn savepoint(machine: &mut Machine) {
    if machine.should_set_savepoint(SavepointKind::Selcom) {
        machine.mark_savepoint();
    }
}

/// The special-case select elements (`select_w`, `select`, `select_s`).
pub fn select(machine: &mut Machine, command: &Command) -> Result<Next> {
    let CommandKind::Select(element) = &command.kind else {
        return machine.unimplemented(command);
    };
    savepoint(machine);
    match command.op.opcode {
        0 => select::window_select(machine, element, true)?,
        1 => select::window_select(machine, element, false)?,
        2 | 3 => select::button_select(machine, element)?,
        _ => {
            // `select_??` (10): treat as a plain window selection.
            machine.note_unimplemented(format!("select variant {}", command.op));
            select::window_select(machine, element, false)?;
        }
    }
    Ok(Next::Advance)
}

pub fn dispatch(machine: &mut Machine, command: &Command) -> Result<Next> {
    if matches!(command.kind, CommandKind::Select(_)) {
        return select(machine, command);
    }
    match command.op.opcode {
        4 | 14 => {
            savepoint(machine);
            let group = machine.int_param_or(command, 0, 0)?;
            let op = ObjectButtonSelect::new(group, command.op.opcode == 14);
            machine.push_long_op(Box::new(op));
        }
        // objbtn_init: nothing to prepare.
        20 => {}
        _ => return machine.unimplemented(command),
    }
    Ok(Next::Advance)
}
