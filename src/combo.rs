use std::mem::size_of;
use std::thread;
use std::time::Duration;

use device_query::Keycode;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
};

const KEY_HOLD_MS: u64 = 40;

pub struct Combo {
    mods: Vec<u16>,
    key: u16,
}

fn scancode(name: &str) -> Option<u16> {
    Some(match name {
        "space" => 0x39,
        "tab" => 0x0F,
        "`" => 0x29,
        "\\" => 0x2B,
        "1" => 0x02, "2" => 0x03, "3" => 0x04, "4" => 0x05, "5" => 0x06,
        "6" => 0x07, "7" => 0x08, "8" => 0x09, "9" => 0x0A, "0" => 0x0B,
        "q" => 0x10, "w" => 0x11, "e" => 0x12, "r" => 0x13, "t" => 0x14,
        "y" => 0x15, "u" => 0x16, "i" => 0x17, "o" => 0x18, "p" => 0x19,
        "a" => 0x1E, "s" => 0x1F, "d" => 0x20, "f" => 0x21, "g" => 0x22,
        "h" => 0x23, "j" => 0x24, "k" => 0x25, "l" => 0x26,
        "z" => 0x2C, "x" => 0x2D, "c" => 0x2E, "v" => 0x2F, "b" => 0x30,
        "n" => 0x31, "m" => 0x32,
        "f1" => 0x3B, "f2" => 0x3C, "f3" => 0x3D, "f4" => 0x3E, "f5" => 0x3F,
        "f6" => 0x40, "f7" => 0x41, "f8" => 0x42, "f9" => 0x43, "f10" => 0x44,
        "f11" => 0x57, "f12" => 0x58,
        _ => return None,
    })
}

pub fn parse(s: &str) -> Result<Combo, String> {
    let parts: Vec<String> = s
        .split('+')
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("empty".into());
    }

    let mut mods = Vec::new();
    for m in &parts[..parts.len() - 1] {
        match m.as_str() {
            "ctrl" | "control" => mods.push(0x1D),
            "shift" => mods.push(0x2A),
            "alt" => mods.push(0x38),
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }

    let main = parts.last().unwrap().as_str();
    let main: String = match main.chars().next() {
        Some(ch) if main.chars().count() == 1 => latin_letter(ch).to_string(),
        _ => main.to_string(),
    };
    match scancode(&main) {
        Some(key) => Ok(Combo { mods, key }),
        None => Err(format!("unknown key '{main}'")),
    }
}

fn send_scan(scan: u16, up: bool) {
    let mut flags = KEYEVENTF_SCANCODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(1, &input, size_of::<INPUT>() as i32);
    }
}

pub fn press(combo: &Combo, fast: bool) {
    if fast {
        for &m in &combo.mods {
            send_scan(m, false);
        }
        send_scan(combo.key, false);
        send_scan(combo.key, true);
        for &m in combo.mods.iter().rev() {
            send_scan(m, true);
        }
        return;
    }
    for &m in &combo.mods {
        send_scan(m, false);
    }
    if !combo.mods.is_empty() {
        thread::sleep(Duration::from_millis(15));
    }
    send_scan(combo.key, false);
    thread::sleep(Duration::from_millis(KEY_HOLD_MS));
    send_scan(combo.key, true);
    if !combo.mods.is_empty() {
        thread::sleep(Duration::from_millis(15));
    }
    for &m in combo.mods.iter().rev() {
        send_scan(m, true);
    }
}

pub struct Hotkey {
    mods: Vec<Vec<Keycode>>,
    key: Keycode,
}

impl Hotkey {
    pub fn is_down(&self, down: &[Keycode]) -> bool {
        down.contains(&self.key)
            && self
                .mods
                .iter()
                .all(|alts| alts.iter().any(|k| down.contains(k)))
    }
}

pub fn parse_hotkey(s: &str) -> Result<Hotkey, String> {
    const LETTERS: [Keycode; 26] = [
        Keycode::A, Keycode::B, Keycode::C, Keycode::D, Keycode::E, Keycode::F, Keycode::G,
        Keycode::H, Keycode::I, Keycode::J, Keycode::K, Keycode::L, Keycode::M, Keycode::N,
        Keycode::O, Keycode::P, Keycode::Q, Keycode::R, Keycode::S, Keycode::T, Keycode::U,
        Keycode::V, Keycode::W, Keycode::X, Keycode::Y, Keycode::Z,
    ];
    const DIGITS: [Keycode; 10] = [
        Keycode::Key0, Keycode::Key1, Keycode::Key2, Keycode::Key3, Keycode::Key4,
        Keycode::Key5, Keycode::Key6, Keycode::Key7, Keycode::Key8, Keycode::Key9,
    ];
    const FKEYS: [Keycode; 12] = [
        Keycode::F1, Keycode::F2, Keycode::F3, Keycode::F4, Keycode::F5, Keycode::F6,
        Keycode::F7, Keycode::F8, Keycode::F9, Keycode::F10, Keycode::F11, Keycode::F12,
    ];

    let parts: Vec<String> = s
        .split('+')
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("empty".into());
    }

    let mut mods = Vec::new();
    for m in &parts[..parts.len() - 1] {
        match m.as_str() {
            "ctrl" | "control" => mods.push(vec![Keycode::LControl, Keycode::RControl]),
            "shift" => mods.push(vec![Keycode::LShift, Keycode::RShift]),
            "alt" => mods.push(vec![Keycode::LAlt, Keycode::RAlt]),
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }

    let main = parts.last().unwrap().as_str();
    let key = match main {
        "space" => Keycode::Space,
        "tab" => Keycode::Tab,
        "pageup" | "pgup" => Keycode::PageUp,
        "pagedown" | "pgdn" => Keycode::PageDown,
        "end" => Keycode::End,
        s if s.starts_with('f') && s.len() > 1 && s[1..].parse::<usize>().is_ok() => {
            let n: usize = s[1..].parse().unwrap();
            if (1..=12).contains(&n) {
                FKEYS[n - 1]
            } else {
                return Err(format!("unknown key '{s}'"));
            }
        }
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap();
            match c {
                'a'..='z' => LETTERS[(c as u8 - b'a') as usize],
                '0'..='9' => DIGITS[(c as u8 - b'0') as usize],
                '\\' => Keycode::BackSlash,
                '`' => Keycode::Grave,
                _ => return Err(format!("key '{c}' is not supported")),
            }
        }
        other => return Err(format!("unknown key '{other}'")),
    };
    Ok(Hotkey { mods, key })
}

pub fn latin_letter(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'й' => 'q',
        'ц' => 'w',
        'у' => 'e',
        'к' => 'r',
        'е' => 't',
        'н' => 'y',
        'г' => 'u',
        'ш' => 'i',
        'щ' => 'o',
        'з' => 'p',
        'ф' => 'a',
        'ы' => 's',
        'в' => 'd',
        'а' => 'f',
        'п' => 'g',
        'р' => 'h',
        'о' => 'j',
        'л' => 'k',
        'д' => 'l',
        'я' => 'z',
        'ч' => 'x',
        'с' => 'c',
        'м' => 'v',
        'и' => 'b',
        'т' => 'n',
        'ь' => 'm',
        'ё' => '`',
        other => other,
    }
}

fn unshift(c: char) -> char {
    match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '|' => '\\',
        '~' => '`',
        other => other,
    }
}

pub fn from_event(key: KeyEvent) -> Result<String, String> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let main = match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => {
            let mut c = latin_letter(c);
            if shift {
                c = unshift(c);
            }
            if c == '+' {
                return Err("The '+' key is not supported".into());
            }
            c.to_string()
        }
        KeyCode::Tab => "tab".to_string(),
        KeyCode::F(n) if (1..=12).contains(&n) => format!("f{n}"),
        _ => return Err("This key is not supported".into()),
    };

    let mut parts: Vec<String> = Vec::new();
    if ctrl {
        parts.push("ctrl".into());
    }
    if shift {
        parts.push("shift".into());
    }
    if alt {
        parts.push("alt".into());
    }
    parts.push(main);
    Ok(parts.join("+"))
}