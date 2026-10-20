use regex::Regex;

use crate::text::Normalized;
use crate::{Hit, Kind, Source};

/// What may not touch a match, checked in code rather than with lookaround: the
/// `regex` crate has none, and the engine that does (fancy-regex) backtracks at
/// every position and ran at about 5 MB/s.
#[derive(Clone, Copy)]
enum Boundary {
    None,
    /// Digits and hyphens: `1090-1234-5678` is not a phone number.
    Phone,
    /// Digits and ID-like characters: `CP2026090106010023` is a campaign code,
    /// not a card ending in its last 16 digits. ASCII only, because a number right
    /// after `マイナンバー` is exactly the case to catch.
    Id,
}

impl Boundary {
    fn blocks(self, c: char) -> bool {
        match self {
            Boundary::None => false,
            Boundary::Phone => c.is_ascii_digit() || c == '-',
            Boundary::Id => c.is_ascii_alphanumeric() || c == '_' || c == '-',
        }
    }

    /// A space is always a boundary: `090 1234 5678 1990/01/01` is a phone number
    /// and a date. The one number a space-separated run hides, a My Number in
    /// the first 12 digits of a card, is dropped after matching instead.
    fn clear(self, next: Option<char>) -> bool {
        !next.is_some_and(|c| self.blocks(c))
    }
}

struct Pattern {
    kind: Kind,
    regex: Regex,
    boundary: Boundary,
}

pub struct Detector {
    patterns: Vec<Pattern>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        let phone = [
            // Mobile and IP phones: 090 / 080 / 070 / 050, with or without separators.
            r"0[5789]0[- ]?[0-9]{4}[- ]?[0-9]{4}",
            r"0[0-9]{1,4}-[0-9]{1,4}-[0-9]{4}",
            r"\(0[0-9]{1,4}\) ?[0-9]{1,4}-[0-9]{4}",
            r"0[0-9]{1,4}\([0-9]{1,4}\)[0-9]{4}",
            r"\+81[- ]?[0-9]{1,4}[- ]?[0-9]{1,4}[- ]?[0-9]{4}",
        ]
        .join("|");
        let patterns = [
            (
                Kind::Email,
                r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}".to_string(),
                Boundary::None,
            ),
            (Kind::Phone, phone, Boundary::Phone),
            (
                Kind::PostalCode,
                r"〒 ?[0-9]{3}-?[0-9]{4}|[0-9]{3}-[0-9]{4}".to_string(),
                Boundary::Id,
            ),
            (
                Kind::CreditCard,
                [
                    // Visa, Mastercard, JCB, Discover and the like: 16 digits.
                    r"[2-6][0-9]{3}(?:[ -]?[0-9]{4}){3}",
                    // American Express: 15 digits, grouped 4-6-5.
                    r"3[47][0-9]{2}[ -]?[0-9]{6}[ -]?[0-9]{5}",
                    // Diners Club: 14 digits, grouped 4-6-4.
                    r"3[0689][0-9]{2}[ -]?[0-9]{6}[ -]?[0-9]{4}",
                ]
                .join("|"),
                Boundary::Id,
            ),
            // After the cards, which it needs (see scan_line).
            (
                Kind::MyNumber,
                r"[0-9]{4}[ -]?[0-9]{4}[ -]?[0-9]{4}".to_string(),
                Boundary::Id,
            ),
        ];
        Detector {
            patterns: patterns
                .into_iter()
                .map(|(kind, pattern, boundary)| Pattern {
                    kind,
                    regex: Regex::new(&pattern).expect("valid pattern"),
                    boundary,
                })
                .collect(),
        }
    }

    /// The PII on one line, as written in it.
    pub fn scan_line(&self, line: &str) -> Vec<(Kind, String)> {
        let mut found: Vec<(Kind, usize, usize)> = Vec::new();
        // In the normalized text, in order; filled before My Numbers are looked for.
        let mut cards: Vec<(usize, usize)> = Vec::new();
        let normalized = Normalized::new(line);
        let text = normalized.text.as_ref();
        // Every pattern but email needs at least 7 digits.
        let has_at = text.contains('@');
        let has_digits = text.bytes().filter(u8::is_ascii_digit).nth(6).is_some();
        if !has_at && !has_digits {
            return Vec::new();
        }
        for pattern in &self.patterns {
            if !(if pattern.kind == Kind::Email {
                has_at
            } else {
                has_digits
            }) {
                continue;
            }
            let mut position = 0;
            let mut card = 0;
            while let Some(m) = pattern.regex.find_at(text, position) {
                // 〒 marks a postal code whatever comes before it.
                let before_clear = m.as_str().starts_with('〒')
                    || pattern
                        .boundary
                        .clear(text[..m.start()].chars().next_back());
                let after_clear = pattern.boundary.clear(text[m.end()..].chars().next());
                let valid = match pattern.kind {
                    // The first 12 digits of a card number can pass the check digit
                    // on their own (`4111 1111 1111 1111`, `400000000009 0003`).
                    // Rejected here rather than dropped afterwards, so the search
                    // goes on from the next character: a My Number right after the
                    // card must not be skipped over. Starts only move forward, so
                    // one pass over the cards serves every candidate.
                    Kind::MyNumber => {
                        while card < cards.len() && cards[card].1 <= m.start() {
                            card += 1;
                        }
                        let on_card = card < cards.len() && cards[card].0 < m.end();
                        !on_card && my_number_check_digit_ok(m.as_str())
                    }
                    Kind::CreditCard => luhn_ok(m.as_str()),
                    _ => true,
                };
                if before_clear && after_clear && valid {
                    if pattern.kind == Kind::CreditCard {
                        cards.push((m.start(), m.end()));
                    }
                    let (start, end) = normalized.original_range(m.start(), m.end());
                    found.push((pattern.kind, start, end));
                    position = m.end();
                } else if !before_clear {
                    // Every later start inside the same run is blocked the same
                    // way, so jump past it. Retrying each one would be quadratic in
                    // a long run of digits.
                    let rest = &text[m.start()..];
                    let run = rest
                        .char_indices()
                        .find(|&(_, c)| !pattern.boundary.blocks(c))
                        .map_or(rest.len(), |(i, _)| i);
                    position = m.start() + run.max(rest.chars().next().map_or(1, char::len_utf8));
                } else {
                    // What lookaround would have done: try the next start rather
                    // than skipping the whole rejected match, so a valid number
                    // inside it is still found.
                    position =
                        m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
                }
                if position > text.len() {
                    break;
                }
            }
        }
        // In the order they are on the line, whatever the order of the patterns.
        found.sort_by_key(|&(_, start, _)| start);
        found
            .into_iter()
            .map(|(kind, start, end)| (kind, line[start..end].to_string()))
            .collect()
    }

    pub fn scan(&self, text: &str) -> Vec<Hit> {
        text.lines()
            .enumerate()
            .flat_map(|(index, line)| {
                self.scan_line(line)
                    .into_iter()
                    .map(move |(kind, value)| Hit {
                        line: index + 1,
                        end_line: None,
                        kind,
                        value,
                        source: Source::Regex,
                    })
            })
            .collect()
    }
}

fn digits(s: &str) -> Vec<u32> {
    s.chars().filter_map(|c| c.to_digit(10)).collect()
}

/// The My Number check digit: weights 6 5 4 3 2 7 6 5 4 3 2 over the first 11
/// digits, then 11 minus the remainder mod 11, or 0 when the remainder is 0 or 1.
pub fn my_number_check_digit_ok(s: &str) -> bool {
    let d = digits(s);
    if d.len() != 12 {
        return false;
    }
    let total: u32 = [6, 5, 4, 3, 2, 7, 6, 5, 4, 3, 2]
        .iter()
        .zip(&d[..11])
        .map(|(w, x)| w * x)
        .sum();
    let remainder = total % 11;
    let check = if remainder <= 1 { 0 } else { 11 - remainder };
    check == d[11]
}

pub fn luhn_ok(s: &str) -> bool {
    let total: u32 = digits(s)
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &x)| match (i % 2 == 1, x * 2) {
            (true, doubled) if doubled > 9 => doubled - 9,
            (true, doubled) => doubled,
            (false, _) => x,
        })
        .sum();
    total.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds_and_values(line: &str) -> Vec<(Kind, String)> {
        Detector::new().scan_line(line)
    }

    fn one(kind: Kind, value: &str) -> Vec<(Kind, String)> {
        vec![(kind, value.to_string())]
    }

    // A valid My Number made for these tests: 12345678901 + check digit 8.
    const MY_NUMBER: &str = "123456789018";
    // A test card number published by the card brands (Visa).
    const CARD: &str = "4111111111111111";

    #[test]
    fn check_digit_accepts_a_valid_my_number_and_rejects_a_changed_one() {
        // Assert
        assert!(my_number_check_digit_ok(MY_NUMBER));
        assert!(my_number_check_digit_ok("1234 5678 9018"));
        assert!(!my_number_check_digit_ok("123456789017"));
        assert!(!my_number_check_digit_ok("12345678901"));
    }

    #[test]
    fn luhn_accepts_a_test_card_and_rejects_a_changed_one() {
        // Assert
        assert!(luhn_ok(CARD));
        assert!(luhn_ok("4111-1111-1111-1111"));
        assert!(!luhn_ok("4111111111111112"));
    }

    #[test]
    fn finds_an_email() {
        assert_eq!(
            kinds_and_values("連絡先: taro.yamada+news@example.co.jp まで"),
            one(Kind::Email, "taro.yamada+news@example.co.jp")
        );
    }

    #[test]
    fn finds_phone_numbers_in_the_usual_japanese_forms() {
        for phone in [
            "090-1234-5678",
            "09012345678",
            "090 1234 5678",
            "03-1234-5678",
            "06-6123-4567",
            "(03) 1234-5678",
            "03(1234)5678",
            "+81-90-1234-5678",
        ] {
            let line = format!("TEL: {phone}");
            assert_eq!(kinds_and_values(&line), one(Kind::Phone, phone), "{phone}");
        }
    }

    #[test]
    fn finds_a_full_width_phone_number_and_reports_it_as_written() {
        assert_eq!(
            kinds_and_values("電話番号：０９０ー１２３４ー５６７８"),
            one(Kind::Phone, "０９０ー１２３４ー５６７８")
        );
    }

    #[test]
    fn does_not_take_part_of_a_longer_number_for_a_phone() {
        assert_eq!(kinds_and_values("id=1090-1234-56789"), vec![]);
    }

    #[test]
    fn finds_postal_codes_with_and_without_the_mark() {
        assert_eq!(
            kinds_and_values("〒100-0005 東京都"),
            one(Kind::PostalCode, "〒100-0005")
        );
        assert_eq!(
            kinds_and_values("〒1000005 東京都"),
            one(Kind::PostalCode, "〒1000005")
        );
        assert_eq!(
            kinds_and_values("郵便番号 100-0005"),
            one(Kind::PostalCode, "100-0005")
        );
    }

    #[test]
    fn does_not_take_a_product_code_for_a_postal_code() {
        assert_eq!(kinds_and_values("SKU AB123-4567"), vec![]);
    }

    #[test]
    fn finds_a_my_number_right_after_japanese_text() {
        assert_eq!(
            kinds_and_values(&format!("マイナンバー{MY_NUMBER}")),
            one(Kind::MyNumber, MY_NUMBER)
        );
    }

    #[test]
    fn ignores_twelve_digits_whose_check_digit_is_wrong() {
        assert_eq!(kinds_and_values("order 123456789017"), vec![]);
    }

    #[test]
    fn finds_a_card_number_that_passes_luhn() {
        assert_eq!(
            kinds_and_values("card: 4111 1111 1111 1111"),
            one(Kind::CreditCard, "4111 1111 1111 1111")
        );
        assert_eq!(
            kinds_and_values("amex 378282246310005"),
            one(Kind::CreditCard, "378282246310005")
        );
    }

    #[test]
    fn ignores_the_tail_of_an_alphanumeric_id_that_happens_to_pass_luhn() {
        // Arrange: the digits alone are a Luhn-valid card number.
        let line = format!("campaign CP{CARD}");

        // Assert
        assert!(luhn_ok(CARD));
        assert_eq!(kinds_and_values(&line), vec![]);
    }

    #[test]
    fn ignores_a_masked_card_number() {
        assert_eq!(kinds_and_values("35**********1087"), vec![]);
    }

    #[test]
    fn finds_a_number_after_a_rejected_candidate_on_the_same_line() {
        // Arrange: the first 12 digits fail the check digit, a valid one follows.
        let line = format!("123456789017 / {MY_NUMBER}");

        // Act
        let found = kinds_and_values(&line);

        // Assert
        assert_eq!(found, one(Kind::MyNumber, MY_NUMBER));
    }

    #[test]
    fn a_long_run_of_digits_is_scanned_in_linear_time() {
        // Arrange
        let line = "1".repeat(1_000_000);
        let started = std::time::Instant::now();

        // Act
        let found = kinds_and_values(&line);

        // Assert
        assert_eq!(found, vec![]);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn finds_ungrouped_values_separated_by_a_space() {
        assert_eq!(
            kinds_and_values("TEL 03-1234-5678 090-1234-5678"),
            vec![
                (Kind::Phone, "03-1234-5678".to_string()),
                (Kind::Phone, "090-1234-5678".to_string())
            ]
        );
        assert_eq!(
            kinds_and_values("山田 090-1234-5678 1990/01/01"),
            one(Kind::Phone, "090-1234-5678")
        );
        assert_eq!(
            kinds_and_values("100-0005 2-7-2"),
            one(Kind::PostalCode, "100-0005")
        );
        assert_eq!(
            kinds_and_values(&format!("{MY_NUMBER} {MY_NUMBER}")),
            vec![
                (Kind::MyNumber, MY_NUMBER.to_string()),
                (Kind::MyNumber, MY_NUMBER.to_string())
            ]
        );
    }

    #[test]
    fn a_space_separated_value_followed_by_another_number_is_found() {
        assert_eq!(
            kinds_and_values("090 1234 5678 1990/01/01"),
            one(Kind::Phone, "090 1234 5678")
        );
        assert_eq!(
            kinds_and_values("1234 5678 9018 090-1234-5678"),
            vec![
                (Kind::MyNumber, "1234 5678 9018".to_string()),
                (Kind::Phone, "090-1234-5678".to_string())
            ]
        );
        assert_eq!(
            kinds_and_values("4111 1111 1111 1111 090-1234-5678"),
            vec![
                (Kind::CreditCard, "4111 1111 1111 1111".to_string()),
                (Kind::Phone, "090-1234-5678".to_string())
            ]
        );
    }

    #[test]
    fn finds_a_diners_card_number() {
        assert_eq!(
            kinds_and_values("36259600000004"),
            one(Kind::CreditCard, "36259600000004")
        );
        assert_eq!(
            kinds_and_values("3625 960000 0004"),
            one(Kind::CreditCard, "3625 960000 0004")
        );
    }

    #[test]
    fn my_numbers_beside_cards_on_one_line_are_kept() {
        // Arrange
        let line = format!("{MY_NUMBER},4111111111111111,{MY_NUMBER} 4111 1111 1111 1111");

        // Act
        let found = kinds_and_values(&line);

        // Assert
        let kinds: Vec<Kind> = found.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            vec![
                Kind::MyNumber,
                Kind::CreditCard,
                Kind::MyNumber,
                Kind::CreditCard
            ]
        );
    }

    #[test]
    fn a_my_number_right_after_a_card_is_found() {
        // Arrange: `1111 1234 5678`, across the end of the card, passes the check
        // digit too, and must not hide the real one.
        let line = "4111 1111 1111 1111 1234 5678 9018";

        // Assert
        assert!(my_number_check_digit_ok("1111 1234 5678"));
        assert_eq!(
            kinds_and_values(line),
            vec![
                (Kind::CreditCard, "4111 1111 1111 1111".to_string()),
                (Kind::MyNumber, "1234 5678 9018".to_string())
            ]
        );
    }

    #[test]
    fn a_card_number_is_not_also_a_my_number() {
        // Arrange: Luhn-valid as a whole, and the first 12 digits pass the My
        // Number check digit.
        let card = "400000000009 0003";

        // Assert
        assert!(my_number_check_digit_ok("400000000009"));
        assert_eq!(kinds_and_values(card), one(Kind::CreditCard, card));
    }

    #[test]
    fn scan_gives_one_based_line_numbers() {
        // Act
        let hits = Detector::new().scan("nothing\n\nmail: a@example.jp\r\n");

        // Assert
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 3);
        assert_eq!(hits[0].value, "a@example.jp");
    }
}
