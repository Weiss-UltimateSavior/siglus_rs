//! Keyboard and mouse state, fed by the host once per frame.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Enter,
    Space,
    Escape,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Ctrl,
    Shift,
    Tab,
    Backspace,
    F(u8),
    Char(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Press(Button),
    Release(Button),
    KeyDown(Key),
    KeyUp(Key),
    /// Committed text from an input method or the keyboard.
    Text(String),
}

#[derive(Debug, Clone, Default)]
pub struct Input {
    pub mouse: (i32, i32),
    pub left_held: bool,
    pub right_held: bool,
    pub ctrl: bool,
    pub shift: bool,
    /// Events of the current frame, oldest first; long operations and
    /// handlers take the ones they use.
    pub events: Vec<InputEvent>,
    /// `KeyMouseOn` / `KeyMouseOff`: whether the cursor keys move the mouse.
    pub key_mouse: bool,
    /// Last click position for `GetClick`-style queries.
    pub last_click: Option<(i32, i32, Button)>,
}

impl Input {
    pub fn push(&mut self, event: InputEvent) {
        match &event {
            InputEvent::Press(Button::Left) => self.left_held = true,
            InputEvent::Release(Button::Left) => self.left_held = false,
            InputEvent::Press(Button::Right) => self.right_held = true,
            InputEvent::Release(Button::Right) => self.right_held = false,
            InputEvent::KeyDown(Key::Ctrl) => self.ctrl = true,
            InputEvent::KeyUp(Key::Ctrl) => self.ctrl = false,
            InputEvent::KeyDown(Key::Shift) => self.shift = true,
            InputEvent::KeyUp(Key::Shift) => self.shift = false,
            _ => {}
        }
        if let InputEvent::Press(button) = event {
            self.last_click = Some((self.mouse.0, self.mouse.1, button));
        }
        self.events.push(event);
    }

    /// Takes the first press of `button` (Enter and Space count as a left
    /// click, Escape as a right click).
    pub fn take_press(&mut self, button: Button) -> bool {
        let position = self.events.iter().position(|event| match (event, button) {
            (InputEvent::Press(pressed), _) => *pressed == button,
            (InputEvent::KeyDown(Key::Enter | Key::Space), Button::Left) => true,
            (InputEvent::KeyDown(Key::Escape), Button::Right) => true,
            (InputEvent::KeyDown(Key::PageUp), Button::WheelUp) => true,
            (InputEvent::KeyDown(Key::PageDown), Button::WheelDown) => true,
            _ => false,
        });
        match position {
            Some(index) => {
                self.events.remove(index);
                true
            }
            None => false,
        }
    }

    /// Takes the first left or right click: `1` left, `-1` right.
    pub fn take_click(&mut self) -> Option<i32> {
        let position = self.events.iter().position(|event| {
            matches!(
                event,
                InputEvent::Press(Button::Left | Button::Right)
                    | InputEvent::KeyDown(Key::Enter | Key::Space | Key::Escape)
            )
        })?;
        let event = self.events.remove(position);
        Some(match event {
            InputEvent::Press(Button::Right) | InputEvent::KeyDown(Key::Escape) => -1,
            _ => 1,
        })
    }

    pub fn take_key(&mut self, key: Key) -> bool {
        match self
            .events
            .iter()
            .position(|event| *event == InputEvent::KeyDown(key))
        {
            Some(index) => {
                self.events.remove(index);
                true
            }
            None => false,
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.events.is_empty()
    }

    /// Drops all pending clicks (`FlushClick`).
    pub fn flush_clicks(&mut self) {
        self.events.retain(|event| {
            !matches!(
                event,
                InputEvent::Press(_) | InputEvent::KeyDown(Key::Enter | Key::Space | Key::Escape)
            )
        });
        self.last_click = None;
    }

    /// End of frame: unconsumed events expire.
    pub fn end_frame(&mut self) {
        self.events.clear();
    }
}
