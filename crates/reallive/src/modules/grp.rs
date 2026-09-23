//! Graphics functions (module 1:33).
//!
//! Every `grp` function has a `rec` twin 1000 opcodes higher; `grp`
//! functions take rectangles as inclusive corners `x1, y1, x2, y2`, `rec`
//! functions as `x, y, width, height`. Overloads are told apart by their
//! parameter counts.

use anyhow::{Result, anyhow, bail};

use crate::bytecode::Command;
use crate::effects::Transition;
use crate::expr::Expr;
use crate::longop::TransitionOp;
use crate::machine::{Machine, Next};
use crate::surface::{Blend, ExternalMask, Rect, Surface};

/// Parameter reader for one command.
struct Args<'a> {
    command: &'a Command,
    rec: bool,
}

impl Args<'_> {
    fn count(&self) -> usize {
        self.command.params.len()
    }

    fn int(&self, m: &mut Machine, i: usize) -> Result<i32> {
        m.int_param(self.command, i)
    }

    fn str(&self, m: &mut Machine, i: usize) -> Result<String> {
        m.str_param(self.command, i)
    }

    fn rect(&self, m: &mut Machine, i: usize) -> Result<Rect> {
        let (a, b, c, d) = (
            self.int(m, i)?,
            self.int(m, i + 1)?,
            self.int(m, i + 2)?,
            self.int(m, i + 3)?,
        );
        Ok(if self.rec {
            Rect::new(a, b, c, d)
        } else {
            Rect::from_corners(a, b, c, d)
        })
    }

    fn rgb(&self, m: &mut Machine, i: usize) -> Result<[u8; 4]> {
        Ok([
            self.int(m, i)?.clamp(0, 255) as u8,
            self.int(m, i + 1)?.clamp(0, 255) as u8,
            self.int(m, i + 2)?.clamp(0, 255) as u8,
            255,
        ])
    }
}

fn opacity(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

/// `#SEL.n` (corners) or `#SELR.n` (position and size), normalised to the
/// 16-value corner form.
pub fn sel_values(machine: &Machine, index: i32, rec: bool) -> Vec<i32> {
    let exe = &machine.gameexe;
    let key = |prefix: &str| format!("{prefix}.{index:03}");
    let (values, sizes) = if rec {
        match exe.get(&key("SELR")) {
            Some(entry) => (entry.ints(), true),
            None => (exe.ints(&key("SEL")), false),
        }
    } else {
        match exe.get(&key("SEL")) {
            Some(entry) => (entry.ints(), false),
            None => (exe.ints(&key("SELR")), true),
        }
    };
    let mut values = if values.is_empty() {
        // An undefined effect: the whole screen, instantly.
        let (w, h) = (machine.sys.gfx.width, machine.sys.gfx.height);
        vec![0, 0, w - 1, h - 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0]
    } else {
        values
    };
    if sizes && values.len() >= 4 {
        values[2] = values[0] + values[2] - 1;
        values[3] = values[1] + values[3] - 1;
    }
    values.resize(16, 0);
    values
}

/// Source area, destination and transition of an open/display command.
struct Display {
    src: Option<Rect>,
    dest: (i32, i32),
    transition: Transition,
}

/// Reads `sel[, alpha]`, `sel, area, dx, dy[, alpha]` or the long form
/// from the parameters `at..end`.
fn read_display(machine: &mut Machine, args: &Args, at: usize, end: usize) -> Result<Display> {
    let rest = end.saturating_sub(at);
    if rest == 0 {
        return Ok(Display {
            src: None,
            dest: (0, 0),
            transition: Transition::instant(),
        });
    }
    if rest >= 16 {
        let values = (at..at + 16)
            .map(|i| args.int(machine, i))
            .collect::<Result<Vec<_>>>()?;
        let src = if args.rec {
            Rect::new(values[0], values[1], values[2], values[3])
        } else {
            Rect::from_corners(values[0], values[1], values[2], values[3])
        };
        return Ok(Display {
            src: Some(src),
            dest: (values[4], values[5]),
            transition: Transition::from_sel(&values),
        });
    }
    let sel = args.int(machine, at)?;
    let values = sel_values(machine, sel, args.rec);
    let mut transition = Transition::from_sel(&values);
    let mut src = Some(Rect::from_corners(values[0], values[1], values[2], values[3]));
    let mut dest = (values[4], values[5]);
    match rest {
        2 => transition.opacity = args.int(machine, at + 1)?,
        6 | 7 => {
            src = Some(args.rect(machine, at + 1)?);
            dest = (args.int(machine, at + 5)?, args.int(machine, at + 6)?);
            if rest == 7 {
                transition.opacity = args.int(machine, at + 7)?;
            }
        }
        _ => {}
    }
    // Rects that cover nothing mean "the whole picture".
    if src.is_some_and(|r| r.w <= 1 && r.h <= 1 && r.x == 0 && r.y == 0) {
        src = None;
    }
    Ok(Display {
        src,
        dest,
        transition,
    })
}

/// Loads an image into a surface (`???` is the default grp).
fn load(machine: &mut Machine, name: &str) -> Result<Surface> {
    let name = if name == "???" {
        machine.sys.default_grp.clone()
    } else {
        name.to_owned()
    };
    let sys = &mut machine.sys;
    let image = sys.gfx.load_image(&sys.resources, &name)?;
    crate::cgtable::mark_viewed(machine, &name);
    Ok(image.surface.clone())
}

fn push_stack(machine: &mut Machine, what: &str) {
    let stack = &mut machine.sys.gfx.stack;
    stack.push(what.to_owned());
    if stack.len() > 256 {
        stack.remove(0);
    }
}

/// Composites `image` onto DC 1 (a copy of DC 0) and runs the transition
/// to the resulting screen. `promote` clears the foreground layer and
/// promotes the background one (the `Bg`, display and multi variants).
fn open_onto_screen(
    machine: &mut Machine,
    image: Option<&Surface>,
    display: Display,
    mask: bool,
    promote: bool,
) -> Result<()> {
    let before = crate::screen::compose_scene(&mut machine.sys);
    let gfx = &mut machine.sys.gfx;
    if let Some(image) = image {
        let dc0 = gfx.dc(0)?;
        let dc1 = gfx.dc_mut(1)?;
        dc1.blit(&dc0, dc0.rect(), 0, 0, 255, Blend::Copy, None);
        let src = display.src.unwrap_or(image.rect());
        let blend = if mask { Blend::Mask } else { Blend::Copy };
        dc1.blit(
            image,
            src,
            display.dest.0,
            display.dest.1,
            opacity(display.transition.opacity),
            blend,
            None,
        );
    }
    let dc1 = gfx.dc(1)?;
    let dc0 = gfx.dc_mut(0)?;
    let rect = dc0.rect();
    dc0.blit(&dc1, rect, 0, 0, 255, Blend::Copy, None);
    if promote {
        gfx.promote_objects();
    }
    let after = crate::screen::compose_scene(&mut machine.sys);
    start_transition(machine, display.transition, before, after);
    Ok(())
}

pub fn start_transition(machine: &mut Machine, transition: Transition, before: Surface, after: Surface) {
    if transition.time > 0 && transition.style != 1 {
        let op = TransitionOp::new(machine, transition, before, after);
        machine.push_long_op(Box::new(op));
    }
}

/// Applies `grpMulti` compositors (specials) starting at `at` to DC 1.
fn apply_compositors(machine: &mut Machine, args: &Args, at: usize) -> Result<()> {
    for index in at..args.count() {
        let Expr::Special { tag, pieces } = &args.command.params[index].value else {
            continue;
        };
        let values_from = |machine: &mut Machine, from: usize| -> Result<Vec<i32>> {
            pieces[from..].iter().map(|piece| machine.eval_int(piece)).collect()
        };
        let Some(name) = pieces.first() else {
            continue;
        };
        let name = machine.eval_str(name)?;
        let image = load(machine, &name)?;
        let numbers = values_from(machine, 1)?;
        let (src, dest, alpha) = match (tag, numbers.as_slice()) {
            (0, _) => (image.rect(), (0, 0), 255),
            (1 | 2, [effect, rest @ ..]) => {
                let values = sel_values(machine, *effect, args.rec);
                let src = Rect::from_corners(values[0], values[1], values[2], values[3]);
                let alpha = rest.first().copied().unwrap_or(values[14]);
                (src, (values[4], values[5]), alpha)
            }
            (3 | 4, [a, b, c, d, dx, dy, rest @ ..]) => {
                let src = if args.rec {
                    Rect::new(*a, *b, *c, *d)
                } else {
                    Rect::from_corners(*a, *b, *c, *d)
                };
                (src, (*dx, *dy), rest.first().copied().unwrap_or(255))
            }
            _ => continue,
        };
        machine.sys.gfx.dc_mut(1)?.blit(
            &image,
            src,
            dest.0,
            dest.1,
            opacity(alpha),
            Blend::Mask,
            None,
        );
    }
    Ok(())
}

fn leading_plain(command: &Command) -> usize {
    command
        .params
        .iter()
        .take_while(|param| !matches!(param.value, Expr::Special { .. }))
        .count()
}

/// Reads the `(src, dst[, alpha])` or `(area, src, dx, dy, dst[, alpha])`
/// operand forms of copy-like functions. Returns (src dc, src rect, dest
/// point, dst dc, remaining index).
fn copy_operands(
    machine: &mut Machine,
    args: &Args,
) -> Result<(i32, Option<Rect>, (i32, i32), i32, usize)> {
    if args.count() >= 8 {
        let rect = args.rect(machine, 0)?;
        let src = args.int(machine, 4)?;
        let dx = args.int(machine, 5)?;
        let dy = args.int(machine, 6)?;
        let dst = args.int(machine, 7)?;
        Ok((src, Some(rect), (dx, dy), dst, 8))
    } else {
        let src = args.int(machine, 0)?;
        let dst = args.int(machine, 1)?;
        Ok((src, None, (0, 0), dst, 2))
    }
}

fn blit_between(
    machine: &mut Machine,
    src_dc: i32,
    src_rect: Option<Rect>,
    dest: (i32, i32),
    dst_dc: i32,
    alpha: u8,
    blend: Blend,
    mask: Option<(i32, bool, (i32, i32), i32, i32)>,
) -> Result<()> {
    let gfx = &mut machine.sys.gfx;
    let src = gfx.dc(src_dc)?;
    let rect = src_rect.unwrap_or(src.rect());
    let mask_surface = match mask {
        Some((index, ..)) => Some(
            gfx.masks
                .get(&index)
                .cloned()
                .ok_or_else(|| anyhow!("reallive: mask buffer {index} is empty"))?,
        ),
        None => None,
    };
    let external = match (mask, &mask_surface) {
        (Some((_, invert, offset, levels, threshold)), Some(surface)) => Some(ExternalMask {
            mask: surface,
            invert,
            offset,
            levels,
            threshold,
        }),
        _ => None,
    };
    let dst = gfx.dc_mut(dst_dc)?;
    if src_dc == dst_dc {
        let copy = (*src).clone();
        dst.blit(&copy, rect, dest.0, dest.1, alpha, blend, external);
    } else {
        dst.blit(&src, rect, dest.0, dest.1, alpha, blend, external);
    }
    Ok(())
}

/// The blend of a copy-family opcode (`opcode % 1000`).
fn copy_blend(opcode: u16) -> Option<(Blend, bool, bool)> {
    // (blend, uses external mask, inverted external mask)
    Some(match opcode {
        100 => (Blend::Copy, false, false),
        101 | 102 => (Blend::Mask, false, false),
        120 => (Blend::Copy, true, false),
        121 => (Blend::Mask, true, false),
        140 => (Blend::Copy, true, true),
        141 => (Blend::Mask, true, true),
        501 => (Blend::And, false, false),
        502 => (Blend::Or, false, false),
        600 => (Blend::Add, false, false),
        601 => (Blend::MaskAdd, false, false),
        620 => (Blend::Add, true, false),
        621 => (Blend::MaskAdd, true, false),
        640 => (Blend::Add, true, true),
        641 => (Blend::MaskAdd, true, true),
        700 => (Blend::Sub, false, false),
        701 => (Blend::MaskSub, false, false),
        720 => (Blend::Sub, true, false),
        721 => (Blend::MaskSub, true, false),
        740 => (Blend::Sub, true, true),
        741 => (Blend::MaskSub, true, true),
        _ => return None,
    })
}

pub fn dispatch(machine: &mut Machine, command: &Command) -> Result<Next> {
    let opcode = command.op.opcode % 1000;
    let args = Args {
        command,
        rec: command.op.opcode >= 1000,
    };
    let n = args.count();
    match opcode {
        // allocDC(dc, w, h) / freeDC(dc)
        15 => {
            let dc = args.int(machine, 0)?;
            let w = args.int(machine, 1)?;
            let h = args.int(machine, 2)?;
            machine.sys.gfx.alloc_dc(dc, w, h)?;
            push_stack(machine, "allocDC");
        }
        16 => {
            let dc = args.int(machine, 0)?;
            machine.sys.gfx.free_dc(dc)?;
        }
        // grpLoadMask(filename, mask)
        20 => {
            let name = args.str(machine, 0)?;
            let index = args.int(machine, 1)?;
            let surface = load(machine, &name)?;
            machine.sys.gfx.masks.insert(index, std::rc::Rc::new(surface));
        }
        // grpTextout(text, x, y, dc, size, r, g, b)
        30 => {
            let text = args.str(machine, 0)?;
            let x = args.int(machine, 1)?;
            let y = args.int(machine, 2)?;
            let dc = args.int(machine, 3)?;
            let size = args.int(machine, 4)?;
            let colour = args.rgb(machine, 5)?;
            crate::textout::draw_string_to_dc(&mut machine.sys, dc, x, y, &text, size, colour)?;
        }
        // wipe(dc, r, g, b)
        31 => {
            let dc = args.int(machine, 0)?;
            let colour = args.rgb(machine, 1)?;
            let surface = machine.sys.gfx.dc_mut(dc)?;
            let rect = surface.rect();
            surface.fill(rect, colour, 255);
            push_stack(machine, "wipe");
        }
        // shake(index)
        32 => {
            let index = args.int(machine, 0)?;
            crate::modules::shk::shake_from_gameexe(machine, index)?;
        }
        // grpLoad / grpMaskLoad / grpBuffer / grpMaskBuffer
        50 | 51 | 70 | 71 => {
            let name = args.str(machine, 0)?;
            let dc = args.int(machine, 1)?;
            let image = load(machine, &name)?;
            let (src, dest, alpha) = match n {
                3 => (image.rect(), (0, 0), args.int(machine, 2)?),
                8 | 9 => (
                    args.rect(machine, 2)?,
                    (args.int(machine, 6)?, args.int(machine, 7)?),
                    if n == 9 { args.int(machine, 8)? } else { 255 },
                ),
                _ => (image.rect(), (0, 0), 255),
            };
            let mask = matches!(opcode, 51 | 71);
            if !mask && dc > 1 && n <= 3 {
                // A full load reallocates the DC to the picture's size.
                let mut surface = Surface::new(image.width, image.height);
                surface.blit(&image, src, 0, 0, opacity(alpha), Blend::Copy, None);
                machine.sys.gfx.set_dc(dc, surface)?;
            } else {
                if dc > 1 {
                    machine
                        .sys
                        .gfx
                        .ensure_dc_size(dc, dest.0 + src.w, dest.1 + src.h)?;
                }
                let blend = if mask { Blend::Mask } else { Blend::Copy };
                machine
                    .sys
                    .gfx
                    .dc_mut(dc)?
                    .blit(&image, src, dest.0, dest.1, opacity(alpha), blend, None);
            }
            push_stack(machine, "grpLoad");
        }
        // grpDisplay(dc, ...)
        72 => {
            let dc = args.int(machine, 0)?;
            let display = read_display(machine, &args, 1, n)?;
            let source = (*machine.sys.gfx.dc(dc)?).clone();
            open_onto_screen(machine, Some(&source), display, false, true)?;
            push_stack(machine, "display");
        }
        // grpOpenBg / grpMaskOpen / grpOpen
        73 | 74 | 76 => {
            let name = args.str(machine, 0)?;
            let display = read_display(machine, &args, 1, n)?;
            let image = if name == "?" {
                None
            } else {
                Some(load(machine, &name)?)
            };
            if opcode == 73 && name != "?" && name != "???" {
                machine.sys.default_grp = name.clone();
            }
            open_onto_screen(machine, image.as_ref(), display, opcode == 74, opcode == 73)?;
            push_stack(machine, if opcode == 73 { "grpOpenBg" } else { "grpOpen" });
        }
        // grpMulti(filename | dc, effect..., compositors...)
        75 | 77 => {
            let plain = leading_plain(command);
            let before = crate::screen::compose_scene(&mut machine.sys);
            let base = if opcode == 75 {
                let name = args.str(machine, 0)?;
                if name != "???" {
                    machine.sys.default_grp = name.clone();
                }
                load(machine, &name)?
            } else {
                let dc = args.int(machine, 0)?;
                (*machine.sys.gfx.dc(dc)?).clone()
            };
            // The effect part is everything between the source and the
            // compositors.
            let display = read_display(machine, &args, 1, plain)?;
            {
                let dc1 = machine.sys.gfx.dc_mut(1)?;
                let rect = dc1.rect();
                dc1.fill(rect, [0, 0, 0, 255], 255);
                let src = display.src.unwrap_or(base.rect());
                dc1.blit(&base, src, display.dest.0, display.dest.1, 255, Blend::Copy, None);
            }
            apply_compositors(machine, &args, plain)?;
            let gfx = &mut machine.sys.gfx;
            let dc1 = gfx.dc(1)?;
            let dc0 = gfx.dc_mut(0)?;
            let rect = dc0.rect();
            dc0.blit(&dc1, rect, 0, 0, opacity(display.transition.opacity), Blend::Copy, None);
            gfx.promote_objects();
            let after = crate::screen::compose_scene(&mut machine.sys);
            start_transition(machine, display.transition, before, after);
            push_stack(machine, "grpMulti");
        }
        // Copies and filtered blits.
        100..=102 | 120 | 121 | 140 | 141 | 501 | 502 | 600 | 601 | 620 | 621 | 640 | 641
        | 700 | 701 | 720 | 721 | 740 | 741 => {
            let (blend, with_mask, inverted) =
                copy_blend(opcode).ok_or_else(|| anyhow!("unreachable blend"))?;
            let (src, rect, dest, dst, mut at) = copy_operands(machine, &args)?;
            let mut mask = None;
            if with_mask && at < n {
                let index = args.int(machine, at)?;
                at += 1;
                let (mut offset, mut levels, mut threshold) = ((0, 0), 0, 0);
                if n - at >= 4 {
                    offset = (args.int(machine, at)?, args.int(machine, at + 1)?);
                    levels = args.int(machine, at + 2)?;
                    threshold = args.int(machine, at + 3)?;
                    at += 4;
                }
                mask = Some((index, inverted, offset, levels, threshold));
            }
            let alpha = if at < n { args.int(machine, at)? } else { 255 };
            blit_between(machine, src, rect, dest, dst, opacity(alpha), blend, mask)?;
            if dst == 0 {
                push_stack(machine, "copy");
            }
        }
        // grpRotate family
        160..=165 => {
            let src_rect = args.rect(machine, 0)?;
            let origin = (args.int(machine, 4)?, args.int(machine, 5)?);
            let src_dc = args.int(machine, 6)?;
            let dst_rect = args.rect(machine, 7)?;
            let dest_origin = (args.int(machine, 11)?, args.int(machine, 12)?);
            let dst_dc = args.int(machine, 13)?;
            let angle = args.int(machine, 14)?;
            let scale = (args.int(machine, 15)?, args.int(machine, 16)?);
            let alpha = args.int(machine, 17)?;
            let blend = match opcode {
                160 => Blend::Copy,
                161 => Blend::Mask,
                162 => Blend::Add,
                163 => Blend::MaskAdd,
                164 => Blend::Sub,
                _ => Blend::MaskSub,
            };
            let gfx = &mut machine.sys.gfx;
            let src = (*gfx.dc(src_dc)?).clone();
            gfx.dc_mut(dst_dc)?.rotate_blit(
                &src,
                src_rect,
                (f64::from(origin.0), f64::from(origin.1)),
                (f64::from(dest_origin.0), f64::from(dest_origin.1)),
                dst_rect,
                angle,
                (f64::from(scale.0), f64::from(scale.1)),
                opacity(alpha),
                blend,
            );
        }
        // grpOutline / grpFill
        200 | 201 => {
            let (rect, at) = if n >= 8 {
                (Some(args.rect(machine, 0)?), 4)
            } else {
                (None, 0)
            };
            let dc = args.int(machine, at)?;
            let colour = args.rgb(machine, at + 1)?;
            let alpha = if n > at + 4 { args.int(machine, at + 4)? } else { 255 };
            let surface = machine.sys.gfx.dc_mut(dc)?;
            let rect = rect.unwrap_or(surface.rect());
            if opcode == 200 {
                surface.outline(rect, colour, opacity(alpha));
            } else {
                surface.fill(rect, colour, opacity(alpha));
            }
        }
        // grpInvert / grpMono
        300 | 301 => {
            let (rect, at) = if n >= 5 {
                (Some(args.rect(machine, 0)?), 4)
            } else {
                (None, 0)
            };
            let dc = args.int(machine, at)?;
            let alpha = if n > at + 1 { args.int(machine, at + 1)? } else { 255 };
            let surface = machine.sys.gfx.dc_mut(dc)?;
            let rect = rect.unwrap_or(surface.rect());
            if opcode == 300 {
                surface.invert(rect, opacity(alpha));
            } else {
                surface.mono(rect, opacity(alpha));
            }
        }
        // grpColour(dc, r, g, b) / grpLight(dc, level)
        302 | 303 => {
            let width = if opcode == 302 { 4 } else { 2 };
            let (rect, at) = if n >= width + 4 {
                (Some(args.rect(machine, 0)?), 4)
            } else {
                (None, 0)
            };
            let dc = args.int(machine, at)?;
            let rgb = if opcode == 302 {
                [
                    args.int(machine, at + 1)?,
                    args.int(machine, at + 2)?,
                    args.int(machine, at + 3)?,
                ]
            } else {
                [args.int(machine, at + 1)?; 3]
            };
            let surface = machine.sys.gfx.dc_mut(dc)?;
            let rect = rect.unwrap_or(surface.rect());
            surface.colour(rect, rgb, 255);
        }
        // grpSwap(dc1, dc2) / grpSwap(area, dc1, dx, dy, dc2)
        400 => {
            let (a, rect, dest, b, _) = copy_operands(machine, &args)?;
            let gfx = &mut machine.sys.gfx;
            let mut first = (*gfx.dc(a)?).clone();
            let rect = rect.unwrap_or(first.rect());
            let second = gfx.dc_mut(b)?;
            second.swap_with(&mut first, rect, dest.0, dest.1);
            gfx.set_dc(a, first)?;
        }
        // grpStretchBlt / grpMaskStretchBlt
        401 | 409 => {
            let src_rect = args.rect(machine, 0)?;
            let src_dc = args.int(machine, 4)?;
            let dst_rect = args.rect(machine, 5)?;
            let dst_dc = args.int(machine, 9)?;
            let alpha = if n > 10 { args.int(machine, 10)? } else { 255 };
            let blend = if opcode == 409 { Blend::Mask } else { Blend::Copy };
            let gfx = &mut machine.sys.gfx;
            let src = (*gfx.dc(src_dc)?).clone();
            gfx.dc_mut(dst_dc)?
                .stretch_blit(&src, src_rect, dst_rect, opacity(alpha), blend);
        }
        // grpZoom(from, to, src, dest, time)
        402 => {
            let from = args.rect(machine, 0)?;
            let to = args.rect(machine, 4)?;
            let src = args.int(machine, 8)?;
            let dest = args.rect(machine, 9)?;
            let time = args.int(machine, 13)?;
            let op = crate::longop::ZoomOp::new(machine, src, from, to, dest, time)?;
            machine.push_long_op(Box::new(op));
        }
        // grpFade(...)
        403 => {
            let (rect, colour, time) = read_fade(machine, &args)?;
            let before = crate::screen::compose_scene(&mut machine.sys);
            let surface = machine.sys.gfx.dc_mut(0)?;
            let rect = rect.unwrap_or(surface.rect());
            surface.fill(rect, colour, 255);
            let after = crate::screen::compose_scene(&mut machine.sys);
            let transition = Transition {
                time,
                opacity: 255,
                ..Transition::default()
            };
            start_transition(machine, transition, before, after);
        }
        // grpFlash(r, g, b[, time]) / (area, r, g, b[, time])
        404 => {
            let (rect, at) = if n >= 7 {
                (Some(args.rect(machine, 0)?), 4)
            } else {
                (None, 0)
            };
            let colour = args.rgb(machine, at)?;
            let time = if n > at + 3 { args.int(machine, at + 3)? } else { 50 };
            let op = crate::longop::FlashOp::new(machine, rect, colour, time);
            machine.push_long_op(Box::new(op));
        }
        // grpPan / grpShift / grpSlide
        406..=408 => {
            let a = (args.int(machine, 0)?, args.int(machine, 1)?);
            let b = (args.int(machine, 2)?, args.int(machine, 3)?);
            let src = args.int(machine, 4)?;
            let window = args.rect(machine, 5)?;
            let (direction, time) = if opcode == 406 {
                (0, args.int(machine, 9)?)
            } else {
                (args.int(machine, 9)?, args.int(machine, 10)?)
            };
            let op = crate::longop::PanOp::new(machine, opcode, args.rec, src, a, b, window, direction, time)?;
            machine.push_long_op(Box::new(op));
        }
        // grpNumber / grpMaskNumber
        410 | 411 => {
            let v = (0..n)
                .map(|i| args.int(machine, i))
                .collect::<Result<Vec<_>>>()?;
            if v.len() < 16 {
                bail!("reallive: grpNumber needs 16 parameters");
            }
            let (value, digits, pad, sign) = (v[0], v[1], v[2], v[3]);
            let cell = (v[4], v[5], v[6], v[7]);
            let (xmod, ymod) = (v[8], v[9]);
            let src_dc = v[10];
            let (mut dx, mut dy) = (v[11], v[12]);
            let (dxmod, dymod) = (v[13], v[14]);
            let dst_dc = v[15];
            let alpha = v.get(16).copied().unwrap_or(255);
            let mut glyphs: Vec<i32> = value
                .unsigned_abs()
                .to_string()
                .bytes()
                .map(|b| i32::from(b - b'0'))
                .collect();
            if pad != 0 {
                while (glyphs.len() as i32) < digits {
                    glyphs.insert(0, 0);
                }
            }
            if value < 0 {
                glyphs.insert(0, 10);
            } else if sign != 0 {
                glyphs.insert(0, 11);
            }
            let (cw, ch) = if args.rec {
                (cell.2, cell.3)
            } else {
                (cell.2 - cell.0 + 1, cell.3 - cell.1 + 1)
            };
            let blend = if opcode == 411 { Blend::Mask } else { Blend::Copy };
            let gfx = &mut machine.sys.gfx;
            let src = (*gfx.dc(src_dc)?).clone();
            let dst = gfx.dc_mut(dst_dc)?;
            for glyph in glyphs {
                let rect = Rect::new(cell.0 + glyph * xmod, cell.1 + glyph * ymod, cw, ch);
                dst.blit(&src, rect, dx, dy, opacity(alpha), blend, None);
                dx += dxmod;
                dy += dymod;
            }
        }
        // grpCMaskCopy(src, dst, r, g, b[, alpha])
        500 => {
            let src_dc = args.int(machine, 0)?;
            let dst_dc = args.int(machine, 1)?;
            let key = args.rgb(machine, 2)?;
            let alpha = if n > 5 { args.int(machine, 5)? } else { 255 };
            let gfx = &mut machine.sys.gfx;
            let mut src = (*gfx.dc(src_dc)?).clone();
            for pixel in src.rgba.chunks_exact_mut(4) {
                if pixel[..3] == key[..3] {
                    pixel[3] = 0;
                }
            }
            let rect = src.rect();
            gfx.dc_mut(dst_dc)?
                .blit(&src, rect, 0, 0, opacity(alpha), Blend::Mask, None);
        }
        _ => return machine.unimplemented(command),
    }
    Ok(Next::Advance)
}

/// `grpFade` operands: `(area?, colour, time)`.
fn read_fade(machine: &mut Machine, args: &Args) -> Result<(Option<Rect>, [u8; 4], i32)> {
    let n = args.count();
    let colour_table = |machine: &Machine, index: i32| {
        let [r, g, b] = machine
            .sys
            .gfx
            .colours
            .get(index.max(0) as usize)
            .copied()
            .unwrap_or([0, 0, 0]);
        [r, g, b, 255]
    };
    Ok(match n {
        1 | 2 => {
            let index = args.int(machine, 0)?;
            let time = if n == 2 { args.int(machine, 1)? } else { 0 };
            (None, colour_table(machine, index), time)
        }
        3 | 4 => {
            let colour = args.rgb(machine, 0)?;
            let time = if n == 4 { args.int(machine, 3)? } else { 0 };
            (None, colour, time)
        }
        5 | 6 => {
            let rect = args.rect(machine, 0)?;
            let index = args.int(machine, 4)?;
            let time = if n == 6 { args.int(machine, 5)? } else { 0 };
            (Some(rect), colour_table(machine, index), time)
        }
        _ => {
            let rect = args.rect(machine, 0)?;
            let colour = args.rgb(machine, 4)?;
            let time = if n >= 8 { args.int(machine, 7)? } else { 0 };
            (Some(rect), colour, time)
        }
    })
}
