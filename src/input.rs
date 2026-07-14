//! Translation of crossterm key events into terminal input byte sequences
//! that get written to a session's PTY.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Convert a key event into the byte sequence a terminal would send to the
/// application running inside the PTY. Returns `None` for keys that have no
/// terminal representation.
pub fn key_to_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut out: Vec<u8> = Vec::new();
    if alt {
        out.push(0x1b);
    }
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let lower = c.to_ascii_lowercase();
                match lower {
                    'a'..='z' => out.push(lower as u8 - b'a' + 1),
                    '@' | ' ' => out.push(0x00),
                    '[' => out.push(0x1b),
                    '\\' => out.push(0x1c),
                    ']' => out.push(0x1d),
                    '^' => out.push(0x1e),
                    '_' | '/' => out.push(0x1f),
                    _ => return None,
                }
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up => out.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => out.extend_from_slice(b"\x1b[B"),
        KeyCode::Right => out.extend_from_slice(b"\x1b[C"),
        KeyCode::Left => out.extend_from_slice(b"\x1b[D"),
        KeyCode::Home => out.extend_from_slice(b"\x1b[H"),
        KeyCode::End => out.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n) => out.extend_from_slice(f_key_bytes(n)?),
        _ => return None,
    }
    Some(out)
}

fn f_key_bytes(n: u8) -> Option<&'static [u8]> {
    Some(match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_char() {
        assert_eq!(key_to_bytes(&key(KeyCode::Char('a'), KeyModifiers::NONE)), Some(vec![b'a']));
    }

    #[test]
    fn utf8_char() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('é'), KeyModifiers::NONE)),
            Some("é".as_bytes().to_vec())
        );
    }

    #[test]
    fn ctrl_chars() {
        assert_eq!(key_to_bytes(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(vec![0x03]));
        assert_eq!(key_to_bytes(&key(KeyCode::Char('a'), KeyModifiers::CONTROL)), Some(vec![0x01]));
        assert_eq!(key_to_bytes(&key(KeyCode::Char('['), KeyModifiers::CONTROL)), Some(vec![0x1b]));
        assert_eq!(key_to_bytes(&key(KeyCode::Char(' '), KeyModifiers::CONTROL)), Some(vec![0x00]));
    }

    #[test]
    fn ctrl_uppercase_normalizes() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('C'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            Some(vec![0x03])
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('b'), KeyModifiers::ALT)),
            Some(vec![0x1b, b'b'])
        );
    }

    #[test]
    fn special_keys() {
        assert_eq!(key_to_bytes(&key(KeyCode::Enter, KeyModifiers::NONE)), Some(vec![b'\r']));
        assert_eq!(key_to_bytes(&key(KeyCode::Backspace, KeyModifiers::NONE)), Some(vec![0x7f]));
        assert_eq!(key_to_bytes(&key(KeyCode::Up, KeyModifiers::NONE)), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            key_to_bytes(&key(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(b"\x1b[6~".to_vec())
        );
        assert_eq!(key_to_bytes(&key(KeyCode::Esc, KeyModifiers::NONE)), Some(vec![0x1b]));
    }

    #[test]
    fn function_keys() {
        assert_eq!(key_to_bytes(&key(KeyCode::F(1), KeyModifiers::NONE)), Some(b"\x1bOP".to_vec()));
        assert_eq!(
            key_to_bytes(&key(KeyCode::F(5), KeyModifiers::NONE)),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(key_to_bytes(&key(KeyCode::F(13), KeyModifiers::NONE)), None);
    }

    #[test]
    fn unmapped_key() {
        assert_eq!(key_to_bytes(&key(KeyCode::CapsLock, KeyModifiers::NONE)), None);
    }
}
