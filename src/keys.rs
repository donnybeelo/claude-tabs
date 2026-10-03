use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn modifier_param(m: KeyModifiers) -> u8 {
    1 + m.contains(KeyModifiers::SHIFT) as u8
        + 2 * m.contains(KeyModifiers::ALT) as u8
        + 4 * m.contains(KeyModifiers::CONTROL) as u8
        + 8 * m.contains(KeyModifiers::SUPER) as u8
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 & 0x1f,
        ' ' | '2' | '@' => 0,
        '3' | '[' => 0x1b,
        '4' | '\\' => 0x1c,
        '5' | ']' => 0x1d,
        '6' | '^' => 0x1e,
        '7' | '-' | '_' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Encodes a key press the way the program inside the tab asked for
pub fn encode_key(key: &KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let param = modifier_param(m);
    let kitty = mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL);

    let tilde = |n: u8| {
        if param > 1 { format!("\x1b[{n};{param}~") } else { format!("\x1b[{n}~") }
    };
    let cursor = |c: char| {
        if param > 1 {
            format!("\x1b[1;{param}{c}")
        } else if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    let ss3 = |c: char| {
        if param > 1 { format!("\x1b[1;{param}{c}") } else { format!("\x1bO{c}") }
    };
    let special = |code: u32, legacy: &[u8]| -> Vec<u8> {
        if kitty && param > 1 {
            format!("\x1b[{code};{param}u").into_bytes()
        } else {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            out.extend_from_slice(legacy);
            out
        }
    };

    let bytes = match key.code {
        KeyCode::Char(c) => {
            if kitty && (ctrl || alt || m.contains(KeyModifiers::SUPER)) {
                return Some(format!("\x1b[{};{param}u", c.to_ascii_lowercase() as u32).into_bytes());
            }
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            match ctrl.then(|| ctrl_byte(c)).flatten() {
                Some(b) => out.push(b),
                None => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
            }
            out
        },
        KeyCode::Enter => special(13, b"\r"),
        KeyCode::Tab => special(9, b"\t"),
        KeyCode::BackTab => {
            if kitty && param > 2 { format!("\x1b[9;{param}u").into_bytes() } else { b"\x1b[Z".to_vec() }
        },
        KeyCode::Backspace => special(127, if ctrl { b"\x08" } else { b"\x7f" }),
        KeyCode::Esc => {
            if kitty && param == 1 { b"\x1b[27u".to_vec() } else { special(27, b"\x1b") }
        },
        KeyCode::Up => cursor('A').into_bytes(),
        KeyCode::Down => cursor('B').into_bytes(),
        KeyCode::Right => cursor('C').into_bytes(),
        KeyCode::Left => cursor('D').into_bytes(),
        KeyCode::Home => cursor('H').into_bytes(),
        KeyCode::End => cursor('F').into_bytes(),
        KeyCode::Insert => tilde(2).into_bytes(),
        KeyCode::Delete => tilde(3).into_bytes(),
        KeyCode::PageUp => tilde(5).into_bytes(),
        KeyCode::PageDown => tilde(6).into_bytes(),
        KeyCode::F(n @ 1..=4) => ss3((b'P' + n - 1) as char).into_bytes(),
        KeyCode::F(n) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => return None,
            };
            tilde(code).into_bytes()
        },
        _ => return None,
    };
    Some(bytes)
}

/// Encodes a mouse event for the program inside the tab, if it asked for mouse input
pub fn encode_mouse(event: &MouseEvent, mode: TermMode, row: u16) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, pressed) = match event.kind {
        MouseEventKind::Down(b) => (button(b), true),
        MouseEventKind::Up(b) => (button(b), false),
        MouseEventKind::Drag(b) if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) => {
            (button(b) + 32, true)
        },
        MouseEventKind::Moved if mode.contains(TermMode::MOUSE_MOTION) => (35, true),
        MouseEventKind::ScrollUp => (64, true),
        MouseEventKind::ScrollDown => (65, true),
        MouseEventKind::ScrollLeft => (66, true),
        MouseEventKind::ScrollRight => (67, true),
        _ => return None,
    };
    let m = event.modifiers;
    code += 4 * m.contains(KeyModifiers::SHIFT) as u8
        + 8 * m.contains(KeyModifiers::ALT) as u8
        + 16 * m.contains(KeyModifiers::CONTROL) as u8;
    let (x, y) = (event.column as u32 + 1, row as u32 + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    if x > 223 || y > 223 {
        return None;
    }
    let code = if pressed { code } else { 3 + (code & !3) };
    Some(vec![0x1b, b'[', b'M', 32 + code, 32 + x as u8, 32 + y as u8])
}
