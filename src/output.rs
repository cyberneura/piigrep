use std::borrow::Cow;
use std::io::{self, Write};

use clap::ValueEnum;
use serde::Serialize;

use crate::{Hit, Kind, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// path:line:kind:value
    Grep,
    /// Tab-separated, with a header row
    Tsv,
    /// One JSON object per line
    Json,
}

pub struct Printer<W: Write> {
    out: W,
    format: Format,
    color: bool,
    files_only: bool,
}

#[derive(Serialize)]
struct JsonHit<'a> {
    path: &'a str,
    line: usize,
    end_line: Option<usize>,
    kind: Kind,
    value: &'a str,
    source: Source,
}

const MAGENTA: &str = "\x1b[35m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const RED_BOLD: &str = "\x1b[1;31m";
const RESET: &str = "\x1b[0m";

impl<W: Write> Printer<W> {
    pub fn new(mut out: W, format: Format, color: bool, files_only: bool) -> io::Result<Self> {
        if format == Format::Tsv && !files_only {
            writeln!(out, "path\tline\tkind\tvalue\tsource")?;
        }
        Ok(Printer {
            out,
            format,
            color,
            files_only,
        })
    }

    fn paint(&self, color: &str, text: &str) -> String {
        if self.color {
            format!("{color}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    /// Writes the hits of one file and flushes, so a long scan shows its results
    /// as it goes.
    pub fn file(&mut self, path: &str, hits: &[Hit]) -> io::Result<()> {
        if hits.is_empty() {
            return Ok(());
        }
        if self.files_only {
            writeln!(self.out, "{}", self.paint(MAGENTA, &printable(path)))?;
            return self.out.flush();
        }
        for hit in hits {
            let lines = match hit.end_line {
                Some(end) => format!("{}-{end}", hit.line),
                None => hit.line.to_string(),
            };
            let source = match hit.source {
                Source::Regex => "regex",
                Source::Fm => "fm",
            };
            match self.format {
                Format::Grep => writeln!(
                    self.out,
                    "{}:{}:{}:{}",
                    self.paint(MAGENTA, &printable(path)),
                    self.paint(GREEN, &lines),
                    self.paint(CYAN, hit.kind.as_str()),
                    self.paint(RED_BOLD, &printable(&hit.value))
                )?,
                Format::Tsv => writeln!(
                    self.out,
                    "{}\t{lines}\t{}\t{}\t{source}",
                    printable(path),
                    hit.kind.as_str(),
                    printable(&hit.value)
                )?,
                Format::Json => {
                    let json = JsonHit {
                        path,
                        line: hit.line,
                        end_line: hit.end_line,
                        kind: hit.kind,
                        value: &hit.value,
                        source: hit.source,
                    };
                    let line = serde_json::to_string(&json).map_err(io::Error::other)?;
                    writeln!(self.out, "{}", escape_json_controls(&line))?;
                }
            }
        }
        self.out.flush()
    }
}

/// serde_json escapes U+0000–U+001F but writes DEL and the C1 controls
/// (U+007F–U+009F) as they are, and a terminal acts on C1 controls too. They
/// can only occur inside strings, so `\uXXXX` keeps the JSON valid and the value
/// unchanged.
fn escape_json_controls(json: &str) -> Cow<'_, str> {
    if !json.chars().any(|c| ('\u{7f}'..='\u{9f}').contains(&c)) {
        return Cow::Borrowed(json);
    }
    Cow::Owned(
        json.chars()
            .map(|c| {
                if ('\u{7f}'..='\u{9f}').contains(&c) {
                    format!("\\u{:04x}", c as u32)
                } else {
                    c.to_string()
                }
            })
            .collect(),
    )
}

/// File names and the model's answers come from what is being searched. Written
/// raw, an escape sequence in either is run by the terminal: it can clear the
/// screen or forge lines of output.
pub fn printable(s: &str) -> Cow<'_, str> {
    if !s.chars().any(char::is_control) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(
        s.chars()
            .map(|c| {
                if c.is_control() {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits() -> Vec<Hit> {
        vec![
            Hit {
                line: 3,
                end_line: None,
                kind: Kind::Email,
                value: "a@example.jp".into(),
                source: Source::Regex,
            },
            Hit {
                line: 5,
                end_line: Some(9),
                kind: Kind::Address,
                value: "東京都\t千代田区".into(),
                source: Source::Fm,
            },
        ]
    }

    fn print(format: Format, files_only: bool) -> String {
        let mut printer = Printer::new(Vec::new(), format, false, files_only).unwrap();
        printer.file("dir/a.txt", &hits()).unwrap();
        printer.file("dir/empty.txt", &[]).unwrap();
        String::from_utf8(printer.out).unwrap()
    }

    #[test]
    fn grep_format() {
        assert_eq!(
            print(Format::Grep, false),
            "dir/a.txt:3:email:a@example.jp\ndir/a.txt:5-9:address:東京都\\t千代田区\n"
        );
    }

    #[test]
    fn tsv_format_has_a_header_and_escapes_tabs_in_fields() {
        assert_eq!(
            print(Format::Tsv, false),
            "path\tline\tkind\tvalue\tsource\n\
             dir/a.txt\t3\temail\ta@example.jp\tregex\n\
             dir/a.txt\t5-9\taddress\t東京都\\t千代田区\tfm\n"
        );
    }

    #[test]
    fn json_format() {
        // Act
        let out = print(Format::Json, false);

        // Assert
        let rows: Vec<serde_json::Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["path"], "dir/a.txt");
        assert_eq!(rows[0]["end_line"], serde_json::Value::Null);
        assert_eq!(rows[0]["source"], "regex");
        assert_eq!(rows[1]["kind"], "address");
        assert_eq!(rows[1]["end_line"], 9);
    }

    #[test]
    fn files_only_prints_each_path_once_and_no_header() {
        assert_eq!(print(Format::Tsv, true), "dir/a.txt\n");
    }

    #[test]
    fn control_characters_in_a_path_are_escaped() {
        // Arrange
        let mut printer = Printer::new(Vec::new(), Format::Grep, false, true).unwrap();

        // Act
        printer.file("evil\x1b[2Jname", &hits()[..1]).unwrap();

        // Assert
        assert_eq!(
            String::from_utf8(printer.out).unwrap(),
            "evil\\u{1b}[2Jname\n"
        );
    }

    #[test]
    fn json_escapes_del_and_c1_controls_and_keeps_the_value() {
        // Arrange
        let mut printer = Printer::new(Vec::new(), Format::Json, false, false).unwrap();

        // Act
        printer.file("a\u{9b}2Jb\u{7f}", &hits()[..1]).unwrap();

        // Assert
        let out = String::from_utf8(printer.out).unwrap();
        assert!(
            !out.contains('\u{9b}') && !out.contains('\u{7f}'),
            "{out:?}"
        );
        let row: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(row["path"], "a\u{9b}2Jb\u{7f}");
    }

    #[test]
    fn colour_wraps_each_field() {
        // Arrange
        let mut printer = Printer::new(Vec::new(), Format::Grep, true, false).unwrap();

        // Act
        printer.file("a", &hits()[..1]).unwrap();

        // Assert
        let out = String::from_utf8(printer.out).unwrap();
        assert!(
            out.starts_with("\x1b[35ma\x1b[0m:\x1b[32m3\x1b[0m:"),
            "{out:?}"
        );
    }
}
