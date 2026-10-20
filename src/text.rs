use std::borrow::Cow;

use encoding_rs::{Encoding, SHIFT_JIS};

#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    Text(String),
    Binary,
    Undecodable,
}

/// UTF-8, then Shift_JIS (Windows-31J), which is what Japanese files that are not
/// UTF-8 are almost always in. A BOM wins over both, so UTF-16 exports from Excel
/// are read rather than taken for binary because of their NUL bytes.
pub fn decode(bytes: &[u8]) -> Decoded {
    if let Some((encoding, bom_length)) = Encoding::for_bom(bytes) {
        return decode_strict(encoding, &bytes[bom_length..]);
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Decoded::Binary;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Decoded::Text(text.to_owned()),
        Err(_) => decode_strict(SHIFT_JIS, bytes),
    }
}

fn decode_strict(encoding: &'static Encoding, bytes: &[u8]) -> Decoded {
    let (text, had_errors) = encoding.decode_without_bom_handling(bytes);
    if had_errors {
        Decoded::Undecodable
    } else {
        Decoded::Text(text.into_owned())
    }
}

/// Full-width digits, letters and dashes are common in Japanese forms
/// (`０９０ー１２３４ー５６７８`). Each is mapped to one ASCII character so the
/// patterns only have to know ASCII, and a match can be mapped back to the text as
/// it was written.
fn ascii_equivalent(c: char) -> Option<char> {
    match c {
        '０'..='９' | 'Ａ'..='Ｚ' | 'ａ'..='ｚ' => {
            char::from_u32(c as u32 - 0xFF10 + '0' as u32)
        }
        '＠' => Some('@'),
        '＋' => Some('+'),
        '．' => Some('.'),
        '＿' => Some('_'),
        '（' => Some('('),
        '）' => Some(')'),
        '\u{3000}' => Some(' '),
        '－' | '‐' | '‑' | '‒' | '–' | '—' | '―' | '−' => Some('-'),
        _ => None,
    }
}

fn is_digit(c: Option<char>) -> bool {
    matches!(c, Some('0'..='9' | '０'..='９'))
}

/// The long vowel mark is typed for a dash in phone numbers (`090ー1234ー5678`),
/// but after katakana it is a letter: `マイナンバー123…` must keep the number
/// separate from the word.
fn ascii_in_context(previous: Option<char>, c: char, next: Option<char>) -> Option<char> {
    match c {
        'ー' | 'ｰ' if is_digit(previous) && is_digit(next) => Some('-'),
        _ => ascii_equivalent(c),
    }
}

pub struct Normalized<'a> {
    pub text: Cow<'a, str>,
    /// Byte offsets of each character in `text` and in the original, with the
    /// end of each appended. `None` when nothing was rewritten.
    offsets: Option<(Vec<usize>, Vec<usize>)>,
}

impl<'a> Normalized<'a> {
    pub fn new(line: &'a str) -> Self {
        if line.is_ascii() {
            return Normalized {
                text: Cow::Borrowed(line),
                offsets: None,
            };
        }
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        let at = |i: usize| chars.get(i).map(|&(_, c)| c);
        let mapped: Vec<Option<char>> = (0..chars.len())
            .map(|i| ascii_in_context(i.checked_sub(1).and_then(at), chars[i].1, at(i + 1)))
            .collect();
        if mapped.iter().all(Option::is_none) {
            return Normalized {
                text: Cow::Borrowed(line),
                offsets: None,
            };
        }
        let mut text = String::with_capacity(line.len());
        let mut normalized_offsets = Vec::new();
        let mut original_offsets = Vec::new();
        for (&(offset, c), mapped) in chars.iter().zip(mapped) {
            normalized_offsets.push(text.len());
            original_offsets.push(offset);
            text.push(mapped.unwrap_or(c));
        }
        normalized_offsets.push(text.len());
        original_offsets.push(line.len());
        Normalized {
            text: Cow::Owned(text),
            offsets: Some((normalized_offsets, original_offsets)),
        }
    }

    /// The byte range in the original line of `start..end` in `text`. Both ends
    /// must be character boundaries of `text`, which a regex match always gives.
    pub fn original_range(&self, start: usize, end: usize) -> (usize, usize) {
        match &self.offsets {
            None => (start, end),
            Some((normalized, original)) => {
                let at = |offset| {
                    let index = normalized
                        .binary_search(&offset)
                        .expect("a match ends on a character boundary");
                    original[index]
                };
                (at(start), at(end))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_reads_utf8() {
        // Act
        let decoded = decode("山田太郎".as_bytes());

        // Assert
        assert_eq!(decoded, Decoded::Text("山田太郎".to_string()));
    }

    #[test]
    fn decode_falls_back_to_shift_jis() {
        // Arrange
        let (bytes, _, _) = SHIFT_JIS.encode("東京都千代田区");

        // Act
        let decoded = decode(&bytes);

        // Assert
        assert_eq!(decoded, Decoded::Text("東京都千代田区".to_string()));
    }

    #[test]
    fn decode_reads_utf16_with_a_bom_despite_its_nul_bytes() {
        // Arrange
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "a@b.jp".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }

        // Act
        let decoded = decode(&bytes);

        // Assert
        assert_eq!(decoded, Decoded::Text("a@b.jp".to_string()));
    }

    #[test]
    fn decode_calls_nul_bytes_without_a_bom_binary() {
        // Act
        let decoded = decode(b"PK\x03\x04\x00\x00abc");

        // Assert
        assert_eq!(decoded, Decoded::Binary);
    }

    #[test]
    fn decode_gives_up_on_bytes_that_are_neither_utf8_nor_shift_jis() {
        // Act
        let decoded = decode(&[0x82, 0xFF, 0xFF, 0x41]);

        // Assert
        assert_eq!(decoded, Decoded::Undecodable);
    }

    #[test]
    fn normalized_maps_full_width_characters_to_ascii() {
        // Act
        let normalized = Normalized::new("電話：０９０ー１２３４－５６７８　ｔａｒｏ＠ｅｘ．ｊｐ");

        // Assert
        assert_eq!(normalized.text, "電話：090-1234-5678 taro@ex.jp");
    }

    #[test]
    fn original_range_maps_a_match_back_to_the_full_width_text() {
        // Arrange
        let line = "電話：０９０ー１２３４ー５６７８です";
        let normalized = Normalized::new(line);
        let start = normalized.text.find("090").unwrap();
        let end = start + "090-1234-5678".len();

        // Act
        let (from, to) = normalized.original_range(start, end);

        // Assert
        assert_eq!(&line[from..to], "０９０ー１２３４ー５６７８");
    }

    #[test]
    fn the_long_vowel_mark_is_a_dash_only_between_digits() {
        // Act
        let normalized = Normalized::new("マイナンバー123 と 03ー1234");

        // Assert
        assert_eq!(normalized.text, "マイナンバー123 と 03-1234");
    }

    #[test]
    fn an_ascii_line_is_not_copied() {
        // Act
        let normalized = Normalized::new("plain 090-1234-5678");

        // Assert
        assert!(matches!(normalized.text, Cow::Borrowed(_)));
        assert_eq!(normalized.original_range(6, 19), (6, 19));
    }
}
