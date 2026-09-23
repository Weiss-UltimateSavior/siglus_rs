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
            for (target, value) in Machine::int_range(origin, values.len())
                .into_iter()
                .zip(values)
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
            for (target, value) in Machine::int_range(dest, values.len())
                .into_iter()
                .zip(values)
            {
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
        // FLAGSORT_LARGE / FLAGSORT_SMALL(count, keys, values...): sorts the
        // keys (descending / ascending) and reorders the values with them.
        (200 | 201, overload) => {
            let count = machine.int_param(command, 0)?.max(0) as usize;
            let keys_at = machine.int_target_param(command, 1)?;
            let keys = read_ints(machine, keys_at, count)?;
            let order = sorted_order(&keys, op.opcode == 200);
            write_ints(machine, keys_at, &order.iter().map(|&i| keys[i]).collect::<Vec<_>>())?;
            let strings = matches!(overload, 1 | 3)
                || (overload == 0 && command.params.get(2).is_some_and(|p| p.value.is_string()));
            let (source, dest) = if overload >= 2 { (2, 3) } else { (2, 2) };
            if strings {
                let from = machine.str_target_param(command, source)?;
                let to = machine.str_target_param(command, dest)?;
                let values = (0..count)
                    .map(|i| machine.read_string(offset_str(from, i)))
                    .collect::<Result<Vec<_>>>()?;
                for (slot, &i) in order.iter().enumerate() {
                    machine.write_string(offset_str(to, slot), values[i].clone())?;
                }
            } else {
                let from = machine.int_target_param(command, source)?;
                let to = machine.int_target_param(command, dest)?;
                let values = read_ints(machine, from, count)?;
                write_ints(machine, to, &order.iter().map(|&i| values[i]).collect::<Vec<_>>())?;
            }
        }
        // FLAGINDEXSORT_LARGE / _SMALL(count, keys[, dest[, first index]]):
        // the key indices in sorted order.
        (210 | 211, _) => {
            let count = machine.int_param(command, 0)?.max(0) as usize;
            let keys_at = machine.int_target_param(command, 1)?;
            let keys = read_ints(machine, keys_at, count)?;
            let order = sorted_order(&keys, op.opcode == 210);
            let top = machine.int_param_or(command, 3, 0)?;
            let dest = if command.params.len() > 2 {
                machine.int_target_param(command, 2)?
            } else {
                keys_at
            };
            let indices: Vec<i32> = order.iter().map(|&i| i as i32 + top).collect();
            write_ints(machine, dest, &indices)?;
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
        .try_fold(0i32, |total, target| {
            Ok(total.wrapping_add(machine.get_target(target)?))
        })
}

/// Indices of `keys` in sorted order (stable).
fn sorted_order(keys: &[i32], descending: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    if descending {
        order.sort_by_key(|&i| std::cmp::Reverse(keys[i]));
    } else {
        order.sort_by_key(|&i| keys[i]);
    }
    order
}

fn read_ints(machine: &mut Machine, first: IntTarget, count: usize) -> Result<Vec<i32>> {
    Machine::int_range(first, count)
        .into_iter()
        .map(|target| machine.get_target(target))
        .collect()
}

fn write_ints(machine: &mut Machine, first: IntTarget, values: &[i32]) -> Result<()> {
    for (target, &value) in Machine::int_range(first, values.len()).into_iter().zip(values) {
        machine.set_target(target, value)?;
    }
    Ok(())
}

fn offset_str(target: crate::machine::StrTarget, offset: usize) -> crate::machine::StrTarget {
    crate::machine::StrTarget {
        index: target.index + offset as i32,
        ..target
    }
}
