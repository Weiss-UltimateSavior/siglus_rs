//! Block memory operations (module 1:11).

use anyhow::Result;

use crate::bytecode::Command;
use crate::machine::{IntTarget, Machine, Next};

pub fn dispatch(machine: &mut Machine, command: &Command) -> Result<Next> {
    let op = command.op;
    match (op.opcode, op.overload) {
        // setarray(origin, values+)
        (0, _) => {
            let origin = machine.int_target_param(command, 0)?;
            let values = machine.int_params_from(command, 1)?;
            for (target, value) in Machine::int_range(origin, values.len()).into_iter().zip(values)
            {
                machine.set_target(target, value)?;
            }
        }
        // setrng(first, last[, value])
        (1, _) => {
            let first = machine.int_target_param(command, 0)?;
            let last = machine.int_target_param(command, 1)?;
            let value = machine.int_param_or(command, 2, 0)?;
            for target in Machine::int_span(first, last)? {
                machine.set_target(target, value)?;
            }
        }
        // cpyrng(source, dest, count)
        (2, _) => {
            let source = machine.int_target_param(command, 0)?;
            let dest = machine.int_target_param(command, 1)?;
            let count = machine.int_param(command, 2)?.max(0) as usize;
            let values = Machine::int_range(source, count)
                .into_iter()
                .map(|target| machine.get_target(target))
                .collect::<Result<Vec<_>>>()?;
            for (target, value) in Machine::int_range(dest, count).into_iter().zip(values) {
                machine.set_target(target, value)?;
            }
        }
        // setarray_stepped(origin, step, values+)
        (3, _) => {
            let origin = machine.int_target_param(command, 0)?;
            let step = machine.int_param(command, 1)?;
            let values = machine.int_params_from(command, 2)?;
            for (i, value) in values.into_iter().enumerate() {
                machine.set_target(offset(origin, step * i as i32), value)?;
            }
        }
        // setrng_stepped(origin, step, count[, value])
        (4, _) => {
            let origin = machine.int_target_param(command, 0)?;
            let step = machine.int_param(command, 1)?;
            let count = machine.int_param(command, 2)?;
            let value = machine.int_param_or(command, 3, 0)?;
            for i in 0..count.max(0) {
                machine.set_target(offset(origin, step * i), value)?;
            }
        }
        // cpyvars(dest, offset, sources+)
        (6, _) => {
            let dest = machine.int_target_param(command, 0)?;
            let shift = machine.int_param(command, 1)?;
            let mut values = Vec::new();
            for index in 2..command.params.len() {
                let source = machine.int_target_param(command, index)?;
                values.push(machine.get_target(offset(source, shift))?);
            }
            for (target, value) in Machine::int_range(dest, values.len()).into_iter().zip(values) {
                machine.set_target(target, value)?;
            }
        }
        // sum(first, last)
        (100, _) => {
            let first = machine.int_target_param(command, 0)?;
            let last = machine.int_target_param(command, 1)?;
            machine.store = sum(machine, first, last)?;
        }
        // sums((first, last)+)
        (101, _) => {
            let mut total = 0i32;
            for index in 0..command.params.len() {
                let pair = machine.complex_param(command, index)?;
                let (Some(first), Some(last)) = (pair.first(), pair.get(1)) else {
                    continue;
                };
                let first = machine.int_target(first)?;
                let last = machine.int_target(last)?;
                total = total.wrapping_add(sum(machine, first, last)?);
            }
            machine.store = total;
        }
        _ => return machine.unimplemented(command),
    }
    Ok(Next::Advance)
}

fn offset(target: IntTarget, by: i32) -> IntTarget {
    match target {
        IntTarget::Mem(mut reference) => {
            reference.index += by;
            IntTarget::Mem(reference)
        }
        IntTarget::Store => IntTarget::Store,
    }
}

fn sum(machine: &Machine, first: IntTarget, last: IntTarget) -> Result<i32> {
    Machine::int_span(first, last)?
        .into_iter()
        .try_fold(0i32, |total, target| Ok(total.wrapping_add(machine.get_target(target)?)))
}
