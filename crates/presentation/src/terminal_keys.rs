//! Turning key presses into the bytes a terminal program expects.

/// Modifier state of a key press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct KeyMods {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

impl KeyMods {
    /// xterm's modifier parameter: 1 plus shift, alt and control bits. `None` when unmodified.
    fn parameter(self) -> Option<u8> {
        let bits = u8::from(self.shift) | u8::from(self.alt) << 1 | u8::from(self.control) << 2;
        (bits != 0).then_some(1 + bits)
    }
}

/// Bytes for the key named `key` (gpui's names: `enter`, `a`, `f5`, ...). `None` for keys that
/// produce text, which arrives through the text input path instead.
pub(crate) fn encode_key(key: &str, mods: KeyMods, application_cursor: bool) -> Option<Vec<u8>> {
    let modified = mods.parameter();

    // Keys that move the cursor or edit: ESC [ or ESC O, with the modifier in the middle.
    let letter = |final_byte: char| -> Vec<u8> {
        match modified {
            Some(m) => format!("\x1b[1;{m}{final_byte}").into_bytes(),
            None if application_cursor => format!("\x1bO{final_byte}").into_bytes(),
            None => format!("\x1b[{final_byte}").into_bytes(),
        }
    };
    let tilde = |number: u8| -> Vec<u8> {
        match modified {
            Some(m) => format!("\x1b[{number};{m}~").into_bytes(),
            None => format!("\x1b[{number}~").into_bytes(),
        }
    };
    // F1 to F4 use ESC O P to S, or ESC [ 1 ; mod P to S with modifiers.
    let function = |final_byte: char| -> Vec<u8> {
        match modified {
            Some(m) => format!("\x1b[1;{m}{final_byte}").into_bytes(),
            None => format!("\x1bO{final_byte}").into_bytes(),
        }
    };

    let bytes = match key {
        "up" => letter('A'),
        "down" => letter('B'),
        "right" => letter('C'),
        "left" => letter('D'),
        "home" => letter('H'),
        "end" => letter('F'),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "delete" => tilde(3),
        "insert" => tilde(2),
        "enter" => b"\r".to_vec(),
        "tab" if mods.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" if mods.control => vec![0x08],
        "backspace" => b"\x7f".to_vec(),
        "f1" => function('P'),
        "f2" => function('Q'),
        "f3" => function('R'),
        "f4" => function('S'),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "space" if mods.control => vec![0],
        _ => return encode_character(key, mods),
    };
    // Alt sends escape first for the keys that have no modifier parameter of their own.
    if mods.alt && matches!(key, "enter" | "tab" | "escape" | "backspace") {
        let mut prefixed = vec![0x1b];
        prefixed.extend(bytes);
        return Some(prefixed);
    }
    Some(bytes)
}

/// Control and alt combinations of a single character key.
fn encode_character(key: &str, mods: KeyMods) -> Option<Vec<u8>> {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else { return None };
    if mods.control {
        let byte = match c.to_ascii_lowercase() {
            letter @ 'a'..='z' => letter as u8 - b'a' + 1,
            '[' => 0x1b,
            '\\' => 0x1c,
            ']' => 0x1d,
            '^' | '6' => 0x1e,
            '_' | '-' => 0x1f,
            '@' | '2' => 0,
            _ => return None,
        };
        let mut out = Vec::new();
        if mods.alt {
            out.push(0x1b);
        }
        out.push(byte);
        return Some(out);
    }
    if mods.alt {
        let mut out = vec![0x1b];
        let shown = if mods.shift { c.to_uppercase().next().unwrap_or(c) } else { c };
        let mut buffer = [0u8; 4];
        out.extend(shown.encode_utf8(&mut buffer).as_bytes());
        return Some(out);
    }
    None
}

/// Text to send for a paste. Programs that asked for it get it wrapped so they can tell a paste
/// from typing.
pub(crate) fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let mut out = b"\x1b[200~".to_vec();
        out.extend(text.bytes());
        out.extend(b"\x1b[201~");
        out
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: KeyMods = KeyMods { shift: false, alt: false, control: false };
    const CTRL: KeyMods = KeyMods { shift: false, alt: false, control: true };
    const ALT: KeyMods = KeyMods { shift: false, alt: true, control: false };
    const SHIFT: KeyMods = KeyMods { shift: true, alt: false, control: false };

    fn enc(key: &str, mods: KeyMods) -> Option<Vec<u8>> {
        encode_key(key, mods, false)
    }

    #[test]
    fn control_letters_become_control_bytes() {
        assert_eq!(enc("c", CTRL), Some(vec![3]));
        assert_eq!(enc("a", CTRL), Some(vec![1]));
        assert_eq!(enc("z", CTRL), Some(vec![26]));
        assert_eq!(enc("[", CTRL), Some(vec![0x1b]));
        assert_eq!(enc("\\", CTRL), Some(vec![0x1c]));
        assert_eq!(enc("space", CTRL), Some(vec![0]));
        assert_eq!(enc("1", CTRL), None);
    }

    #[test]
    fn plain_characters_are_left_to_the_text_path() {
        assert_eq!(enc("a", NONE), None);
        assert_eq!(enc("a", SHIFT), None);
        assert_eq!(enc("1", NONE), None);
        assert_eq!(enc("space", NONE), None);
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(enc("b", ALT), Some(b"\x1bb".to_vec()));
        assert_eq!(enc("f", KeyMods { shift: true, alt: true, control: false }), Some(b"\x1bF".to_vec()));
        assert_eq!(enc("c", KeyMods { shift: false, alt: true, control: true }), Some(vec![0x1b, 3]));
        assert_eq!(enc("backspace", ALT), Some(vec![0x1b, 0x7f]));
    }

    #[test]
    fn editing_keys() {
        assert_eq!(enc("enter", NONE), Some(b"\r".to_vec()));
        assert_eq!(enc("tab", NONE), Some(b"\t".to_vec()));
        assert_eq!(enc("tab", SHIFT), Some(b"\x1b[Z".to_vec()));
        assert_eq!(enc("backspace", NONE), Some(vec![0x7f]));
        assert_eq!(enc("escape", NONE), Some(vec![0x1b]));
        assert_eq!(enc("delete", NONE), Some(b"\x1b[3~".to_vec()));
    }

    #[test]
    fn arrows_follow_the_cursor_key_mode_and_carry_modifiers() {
        assert_eq!(encode_key("up", NONE, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key("up", NONE, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode_key("left", CTRL, false), Some(b"\x1b[1;5D".to_vec()));
        // A modified arrow ignores the application mode.
        assert_eq!(encode_key("right", CTRL, true), Some(b"\x1b[1;5C".to_vec()));
        assert_eq!(encode_key("down", SHIFT, false), Some(b"\x1b[1;2B".to_vec()));
        assert_eq!(encode_key("home", NONE, false), Some(b"\x1b[H".to_vec()));
        assert_eq!(encode_key("end", NONE, true), Some(b"\x1bOF".to_vec()));
    }

    #[test]
    fn page_and_function_keys() {
        assert_eq!(enc("pageup", NONE), Some(b"\x1b[5~".to_vec()));
        assert_eq!(enc("pagedown", CTRL), Some(b"\x1b[6;5~".to_vec()));
        assert_eq!(enc("f1", NONE), Some(b"\x1bOP".to_vec()));
        assert_eq!(enc("f5", NONE), Some(b"\x1b[15~".to_vec()));
        assert_eq!(enc("f12", NONE), Some(b"\x1b[24~".to_vec()));
        assert_eq!(enc("f3", SHIFT), Some(b"\x1b[1;2R".to_vec()));
    }

    #[test]
    fn pastes_use_carriage_returns_and_optional_brackets() {
        assert_eq!(encode_paste("a\nb\r\nc", false), b"a\rb\rc".to_vec());
        assert_eq!(encode_paste("x", true), b"\x1b[200~x\x1b[201~".to_vec());
    }
}
