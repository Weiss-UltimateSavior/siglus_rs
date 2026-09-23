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
        // select_btnobjinitall / select_btnobjend
        21 | 23 => {
            if let Some(mut polled) = machine.sys.polled_buttons.take() {
                polled.finish(&mut machine.sys);
            }
        }
        // select_btnobjstart(group[, ...])
        22 => {
            let group = machine.int_param_or(command, 0, 0)?;
            if let Some(mut polled) = machine.sys.polled_buttons.take() {
                polled.finish(&mut machine.sys);
            }
            let mut polled = crate::select::PolledButtons::new(group);
            polled.update(&mut machine.sys);
            machine.sys.polled_buttons = Some(polled);
        }
        // select_btnobjnow_hit / select_btnobjnow_decide
        30 => {
            machine.store = machine.sys.polled_buttons.as_ref().map_or(-1, |p| p.hovered());
        }
        32 => {
            machine.store = machine
                .sys
                .polled_buttons
                .as_mut()
                .map_or(-1, crate::select::PolledButtons::take_decided);
        }
        _ => return machine.unimplemented(command),
    }
    Ok(Next::Advance)
}
