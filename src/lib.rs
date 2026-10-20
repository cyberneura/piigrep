pub mod detect;
pub mod fm;
pub mod notices;
pub mod output;
pub mod text;

use clap::ValueEnum;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Email,
    Phone,
    #[value(name = "postal_code")]
    PostalCode,
    #[value(name = "my_number")]
    MyNumber,
    #[value(name = "credit_card")]
    CreditCard,
    Name,
    Address,
    Birthday,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Email => "email",
            Kind::Phone => "phone",
            Kind::PostalCode => "postal_code",
            Kind::MyNumber => "my_number",
            Kind::CreditCard => "credit_card",
            Kind::Name => "name",
            Kind::Address => "address",
            Kind::Birthday => "birthday",
        }
    }

    /// Whether the patterns can find this kind. The rest come only from the model.
    pub fn has_pattern(self) -> bool {
        !matches!(self, Kind::Name | Kind::Address | Kind::Birthday)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Regex,
    Fm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// 1-based.
    pub line: usize,
    /// Set when the model returned a value that could not be found verbatim in the
    /// text it was given: the value is somewhere in `line..=end_line`.
    pub end_line: Option<usize>,
    pub kind: Kind,
    pub value: String,
    pub source: Source,
}

/// Pattern hits come first so that, for a value both found, the cheaper and
/// exact answer is the one kept.
pub fn merge(mut hits: Vec<Hit>) -> Vec<Hit> {
    let mut seen = std::collections::HashSet::new();
    hits.retain(|hit| seen.insert((hit.line, hit.value.clone())));
    hits.sort_by_key(|hit| hit.line);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(line: usize, kind: Kind, value: &str, source: Source) -> Hit {
        Hit {
            line,
            end_line: None,
            kind,
            value: value.to_string(),
            source,
        }
    }

    #[test]
    fn merge_keeps_the_first_of_a_value_found_twice_on_a_line() {
        // Arrange
        let hits = vec![
            hit(3, Kind::Phone, "090-1234-5678", Source::Regex),
            hit(1, Kind::Email, "a@example.jp", Source::Regex),
            hit(3, Kind::Phone, "090-1234-5678", Source::Fm),
            hit(3, Kind::Name, "山田太郎", Source::Fm),
        ];

        // Act
        let merged = merge(hits);

        // Assert
        assert_eq!(
            merged,
            vec![
                hit(1, Kind::Email, "a@example.jp", Source::Regex),
                hit(3, Kind::Phone, "090-1234-5678", Source::Regex),
                hit(3, Kind::Name, "山田太郎", Source::Fm),
            ]
        );
    }
}
