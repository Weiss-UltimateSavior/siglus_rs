//! Stateful AVG32 message-window model.

use std::time::{Duration, Instant};

use crate::vm::VmAction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageWindow {
    lines: Vec<String>,
    current: String,
    /// How many characters of `lines` + `current` (concatenated, in order)
    /// are currently revealed. Mirrors the reference's `MesWin_PrintMes`
    /// character-at-a-time reveal, driven by `tick`.
    revealed_chars: usize,
    reveal_due: Instant,
    pub visible: bool,
    pub doubled_font: bool,
    pub color_index: i32,
    /// `0x72:0x11` override of `Avg32Config::message_position`; `None` means
    /// the title never repositions its (static-layout) message window.
    pub position_override: Option<[i32; 2]>,
    /// `0x73:6` override of `Avg32Config::message_font_size`.
    pub font_size_override: Option<[i32; 2]>,
}

impl Default for MessageWindow {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            current: String::new(),
            revealed_chars: 0,
            reveal_due: Instant::now(),
            visible: true,
            doubled_font: false,
            color_index: 0,
            position_override: None,
            font_size_override: None,
        }
    }
}

impl MessageWindow {
    /// Lines truncated to what's actually been revealed so far — what a
    /// frontend should draw. Use [`Self::apply`] + [`Self::tick`] to drive
    /// the reveal; [`Self::reveal_all`] jumps straight to the end (an
    /// advance/skip input while text is still revealing completes it
    /// instantly instead of dismissing the line, matching every common VN
    /// convention and the reference's own `MesWin_PrintMesAll`).
    pub fn lines(&self) -> Vec<String> {
        let mut remaining = self.revealed_chars;
        let mut out = Vec::with_capacity(self.lines.len() + 1);
        for line in self
            .lines
            .iter()
            .chain((!self.current.is_empty()).then_some(&self.current))
        {
            let len = line.chars().count();
            if remaining >= len {
                out.push(line.clone());
                remaining -= len;
            } else {
                if remaining > 0 {
                    out.push(line.chars().take(remaining).collect());
                }
                break;
            }
        }
        out
    }

    pub fn clear(&mut self, hide: bool) {
        self.lines.clear();
        self.current.clear();
        self.revealed_chars = 0;
        self.visible = !hide;
    }

    fn total_chars(&self) -> usize {
        self.lines
            .iter()
            .map(|line| line.chars().count())
            .sum::<usize>()
            + self.current.chars().count()
    }

    pub fn is_revealing(&self) -> bool {
        self.revealed_chars < self.total_chars()
    }

    /// Shows the rest of the current text immediately (a click/advance
    /// while text is still typing out).
    pub fn reveal_all(&mut self) {
        self.revealed_chars = self.total_chars();
    }

    /// Advances the reveal by one character every `interval`, or instantly
    /// reveals everything when `interval` is zero — mirroring
    /// `MesWin_PrintMes`'s `if (!ini.meswait) return MesWin_PrintMesAll();`
    /// short-circuit for `MSG_SPEED=0` titles (e.g. AIR).
    pub fn tick(&mut self, interval: Duration) {
        if interval.is_zero() {
            self.reveal_all();
            return;
        }
        if !self.is_revealing() || Instant::now() < self.reveal_due {
            return;
        }
        self.revealed_chars += 1;
        self.reveal_due = Instant::now() + interval;
    }

    pub fn apply(&mut self, action: &VmAction) {
        match action {
            VmAction::Text(text) => {
                self.visible = true;
                self.current.push_str(text);
            }
            VmAction::LineBreak => {
                self.lines.push(std::mem::take(&mut self.current));
            }
            VmAction::ClearText { hide_window } => self.clear(*hide_window),
            VmAction::SetFontSize { doubled } => self.doubled_font = *doubled,
            VmAction::SetFontColor(color) => self.color_index = *color,
            VmAction::SetMessagePosition(position) => self.position_override = Some(*position),
            VmAction::SetMessageFontSize(font_size) => self.font_size_override = Some(*font_size),
            VmAction::WaitForInput { clears_text: true } => self.clear(false),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveals_one_character_at_a_time_and_stops_at_the_end() {
        let mut window = MessageWindow::default();
        window.apply(&VmAction::Text("AIR".into()));
        assert_eq!(window.lines(), Vec::<String>::new());
        assert!(window.is_revealing());

        // A long interval never elapses within the test, so each tick can
        // reveal at most the one character already due from construction.
        let interval = Duration::from_secs(3600);
        window.tick(interval);
        assert_eq!(window.lines(), vec!["A".to_string()]);
        window.tick(interval);
        assert_eq!(window.lines(), vec!["A".to_string()], "no due tick yet");

        window.reveal_all();
        assert!(!window.is_revealing());
        assert_eq!(window.lines(), vec!["AIR".to_string()]);
    }

    #[test]
    fn zero_interval_reveals_instantly() {
        let mut window = MessageWindow::default();
        window.apply(&VmAction::Text("AIR".into()));
        window.tick(Duration::ZERO);
        assert_eq!(window.lines(), vec!["AIR".to_string()]);
    }

    #[test]
    fn clear_resets_reveal_progress() {
        let mut window = MessageWindow::default();
        window.apply(&VmAction::Text("AIR".into()));
        window.reveal_all();
        window.clear(false);
        window.apply(&VmAction::Text("B".into()));
        assert_eq!(window.lines(), Vec::<String>::new());
    }
}
