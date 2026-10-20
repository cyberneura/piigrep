//! Names, addresses and birthdays, which no pattern can find, from Apple's
//! on-device Foundation Model through the `fm` command of macOS 27. Nothing
//! leaves the machine: `fm` uses the on-device model unless told otherwise.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use regex::Regex;
use serde::Deserialize;

use crate::detect::my_number_check_digit_ok;
use crate::text::Normalized;
use crate::{Hit, Kind, Source};

/// The on-device model has a 4,096-token context, shared by the instructions,
/// the text and the answer. Japanese runs at about 0.6 tokens a character, so
/// 2,000 characters leave room for the rest. Text that tokenizes worse is split
/// further when the model says it is too long.
pub const CHUNK_CHARS: usize = 2000;
const MIN_CHUNK_CHARS: usize = 200;
/// Where text has to be cut mid-line, the pieces share this many characters, so
/// a name or address on the cut is seen whole by at least one of them.
const OVERLAP_CHARS: usize = 100;
const TIMEOUT: Duration = Duration::from_secs(180);

const INSTRUCTIONS: &str = "You are a strict PII detector for Japanese and English text. \
Report only information that identifies a real individual: person names (real human names, \
usually Japanese kanji/kana or Western first+last name), phone numbers, email addresses, \
postal addresses, Japanese My Number (exactly 12 digits), birthdays. \
Do NOT report: numeric IDs, user IDs, usernames, login handles, nicknames, UUIDs, hashes, \
URLs, IP addresses, version numbers, config values, company or product names, \
dates that are not birthdays. Copy each value exactly as written.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    pub line: usize,
    pub text: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub first_line: usize,
    pub last_line: usize,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub hits: Vec<Hit>,
    pub failures: Vec<Failure>,
}

enum AskError {
    TooLong,
    Other(String),
}

// Fields are required: an answer missing them is a failed check, not a clean one.
#[derive(Deserialize)]
struct Response {
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    kind: String,
    value: String,
}

pub struct Fm {
    schema: tempfile::NamedTempFile,
}

impl Fm {
    pub fn new() -> anyhow::Result<Self> {
        let available = match Command::new("fm").arg("available").output() {
            Ok(output) => output,
            Err(error) if error.kind() == ErrorKind::NotFound => bail!(
                "the `fm` command was not found. --fm needs macOS 27 or later with Apple Intelligence turned on"
            ),
            Err(error) => return Err(error).context("could not run `fm available`"),
        };
        if !available.status.success() {
            bail!(
                "the on-device model is not available: {}",
                String::from_utf8_lossy(&available.stdout).trim()
            );
        }

        // Built with `fm schema` on every run rather than kept as a file: `fm`
        // reads only the format it writes itself, and that may change with macOS.
        let item = fm_schema(&[
            "--name",
            "Item",
            "--string",
            "kind",
            "--description",
            "one of: name, phone, email, address, national_id, birthday, other",
            "--string",
            "value",
            "--description",
            "the exact string as it appears in the text",
        ])?;
        let root = fm_schema(&[
            "--name",
            "PiiResult",
            "--boolean",
            "contains_pii",
            "--description",
            "true only if the text contains personal information about a real individual",
            "--object",
            "items",
            "--schema",
            &item,
            "--array",
        ])?;
        let mut schema = tempfile::Builder::new()
            .prefix("piigrep-schema-")
            .suffix(".json")
            .tempfile()?;
        schema.write_all(root.as_bytes())?;
        schema.flush()?;
        Ok(Fm { schema })
    }

    pub fn scan(&self, text: &str, progress: &mut dyn FnMut(usize, usize)) -> Scan {
        let mut queue: VecDeque<Vec<Segment>> = chunks(text, CHUNK_CHARS, OVERLAP_CHARS).into();
        let mut total = queue.len();
        let mut done = 0;
        let mut scan = Scan::default();
        while let Some(chunk) = queue.pop_front() {
            progress(done, total);
            match self.ask(&chunk) {
                Ok(items) => {
                    for item in items {
                        if let Some((kind, value)) = accept(&item.kind, &item.value) {
                            scan.hits.extend(locate(&chunk, kind, &value));
                        }
                    }
                }
                Err(AskError::TooLong) if char_count(&chunk) > MIN_CHUNK_CHARS => {
                    let (first, second) = split(chunk);
                    queue.push_front(second);
                    queue.push_front(first);
                    total += 1;
                    continue;
                }
                Err(error) => scan.failures.push(Failure {
                    first_line: chunk[0].line,
                    last_line: chunk[chunk.len() - 1].line,
                    reason: match error {
                        AskError::TooLong => "too long for the model".to_string(),
                        AskError::Other(reason) => reason,
                    },
                }),
            }
            done += 1;
        }
        progress(done, total);
        scan
    }

    fn ask(&self, chunk: &[Segment]) -> Result<Vec<Item>, AskError> {
        let body: Vec<&str> = chunk.iter().map(|segment| segment.text).collect();
        let body = body.join("\n");
        if body.trim().is_empty() {
            return Ok(Vec::new());
        }
        // Through stdin, not an argument: a text starting with `-` (an SQL comment)
        // is read by `fm` as an option.
        let prompt = format!("Extract all PII from the text.\n\nText:\n{body}");
        let mut child = Command::new("fm")
            .arg("respond")
            .arg("--no-stream")
            .arg("--greedy")
            .arg("--schema")
            .arg(self.schema.path())
            .arg("--instructions")
            .arg(INSTRUCTIONS)
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| AskError::Other(format!("could not run fm: {error}")))?;

        let mut stdin = child.stdin.take().expect("piped");
        let writer = thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        let mut stdout = child.stdout.take().expect("piped");
        let stdout_reader = thread::spawn(move || {
            let mut buffer = String::new();
            let _ = stdout.read_to_string(&mut buffer);
            buffer
        });
        let mut stderr = child.stderr.take().expect("piped");
        let stderr_reader = thread::spawn(move || {
            let mut buffer = String::new();
            let _ = stderr.read_to_string(&mut buffer);
            buffer
        });

        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let _ = writer.join();
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();

        let Some(status) = status else {
            return Err(AskError::Other(format!(
                "fm did not answer within {} seconds",
                TIMEOUT.as_secs()
            )));
        };
        let stdout = strip_ansi(&stdout);
        let stderr = strip_ansi(&stderr);
        if !status.success() {
            let message = format!("{stderr}\n{stdout}");
            if message.contains("context size") {
                return Err(AskError::TooLong);
            }
            return Err(AskError::Other(first_line(&message)));
        }
        parse(&stdout).ok_or_else(|| AskError::Other(format!("not JSON: {}", first_line(&stdout))))
    }
}

fn fm_schema(args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("fm")
        .args(["schema", "object"])
        .args(args)
        .output()
        .context("could not run `fm schema object`")?;
    if !output.status.success() {
        bail!(
            "`fm schema object` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn strip_ansi(s: &str) -> String {
    static ANSI: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("valid pattern"));
    ANSI.replace_all(s, "").into_owned()
}

fn first_line(s: &str) -> String {
    let line = s.lines().map(str::trim).find(|line| !line.is_empty());
    line.unwrap_or("(no output)").chars().take(200).collect()
}

fn parse(stdout: &str) -> Option<Vec<Item>> {
    let start = stdout.find('{')?;
    let end = stdout.rfind('}')?;
    let response: Response = serde_json::from_str(stdout.get(start..=end)?).ok()?;
    Some(response.items)
}

fn char_count(chunk: &[Segment]) -> usize {
    chunk
        .iter()
        .map(|segment| segment.text.chars().count())
        .sum()
}

/// Lines in order, packed up to `limit` characters a chunk. A line longer than
/// that is cut into pieces that each keep its line number and overlap by
/// `overlap` characters, so nothing is dropped or seen only in halves.
pub fn chunks(text: &str, limit: usize, overlap: usize) -> Vec<Vec<Segment<'_>>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut size = 0;
    for (index, line) in text.lines().enumerate() {
        for piece in pieces(line, limit, overlap) {
            let length = piece.chars().count() + 1;
            if size + length > limit && !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                size = 0;
            }
            current.push(Segment {
                line: index + 1,
                text: piece,
            });
            size += length;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn pieces(line: &str, limit: usize, overlap: usize) -> Vec<&str> {
    assert!(overlap < limit, "pieces must move forward");
    let mut pieces = Vec::new();
    let mut rest = line;
    loop {
        match rest.char_indices().nth(limit) {
            Some((cut, _)) => {
                pieces.push(&rest[..cut]);
                let next = rest
                    .char_indices()
                    .nth(limit - overlap)
                    .map_or(cut, |(i, _)| i);
                rest = &rest[next..];
            }
            None => {
                pieces.push(rest);
                return pieces;
            }
        }
    }
}

fn split(mut chunk: Vec<Segment>) -> (Vec<Segment>, Vec<Segment>) {
    if chunk.len() > 1 {
        let second = chunk.split_off(chunk.len() / 2);
        return (chunk, second);
    }
    let segment = chunk[0];
    let length = segment.text.chars().count();
    let half_overlap = OVERLAP_CHARS.min(length / 4) / 2;
    let offset = |n: usize| {
        segment
            .text
            .char_indices()
            .nth(n)
            .map_or(segment.text.len(), |(i, _)| i)
    };
    let left = &segment.text[..offset(length / 2 + half_overlap)];
    let right = &segment.text[offset(length / 2 - half_overlap)..];
    (
        vec![Segment {
            line: segment.line,
            text: left,
        }],
        vec![Segment {
            line: segment.line,
            text: right,
        }],
    )
}

fn has_japanese(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\u{3040}'..='\u{30ff}' | '\u{4e00}'..='\u{9fff}'))
}

/// `Taro Yamada`, `Mary Ann Smith`.
fn is_western_name(s: &str) -> bool {
    let words: Vec<&str> = s.split(' ').collect();
    words.len() >= 2
        && words.iter().all(|word| {
            let mut chars = word.chars();
            chars.next().is_some_and(|c| c.is_ascii_uppercase())
                && chars.clone().count() > 0
                && chars.all(|c| c.is_ascii_lowercase())
        })
}

/// A single run of ASCII letters, digits, `_` and `-`: a user ID, a handle, a
/// UUID or a column name. The model labels these as names and addresses often
/// enough to bury the real ones.
fn is_identifier(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The model's answer, kept only where it is plausible. It invents kinds
/// (`user_id`, `uuid`) for plain identifiers and calls login handles names.
pub fn accept(kind: &str, value: &str) -> Option<(Kind, String)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let digits = value
        .chars()
        .filter(|c| c.is_ascii_digit() || ('０'..='９').contains(c))
        .count();
    let has_letters = value.chars().any(|c| c.is_ascii_alphabetic());
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "name" if has_japanese(value) || is_western_name(value) => Kind::Name,
        "phone" if (10..=13).contains(&digits) => Kind::Phone,
        "email" if value.contains('@') => Kind::Email,
        "address" if !is_identifier(value) => Kind::Address,
        // `1990-01-01` is all digits and hyphens; only a token with letters in it
        // (`user_1985`) is an identifier here.
        "birthday" if digits > 0 && !(is_identifier(value) && has_letters) => Kind::Birthday,
        "national_id" | "my_number" if my_number_check_digit_ok(&Normalized::new(value).text) => {
            Kind::MyNumber
        }
        _ => return None,
    };
    Some((kind, value.to_string()))
}

/// Every line of the chunk the value is on. The model sometimes changes the
/// spacing of what it copies; failing even that, the value is reported against
/// the lines of the whole chunk rather than dropped.
pub fn locate(chunk: &[Segment], kind: Kind, value: &str) -> Vec<Hit> {
    let hit = |line, end_line| Hit {
        line,
        end_line,
        kind,
        value: value.to_string(),
        source: Source::Fm,
    };
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let squeezed = squeeze(value);
    for matches in [
        &(|text: &str| text.contains(value)) as &dyn Fn(&str) -> bool,
        &|text: &str| squeeze(text).contains(&squeezed),
    ] {
        let mut lines: Vec<usize> = chunk
            .iter()
            .filter(|segment| matches(segment.text))
            .map(|segment| segment.line)
            .collect();
        lines.dedup();
        if !lines.is_empty() {
            return lines.into_iter().map(|line| hit(line, None)).collect();
        }
    }
    let first = chunk[0].line;
    let last = chunk[chunk.len() - 1].line;
    vec![hit(first, (last != first).then_some(last))]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(line: usize, text: &str) -> Segment<'_> {
        Segment { line, text }
    }

    #[test]
    fn chunks_pack_lines_up_to_the_limit() {
        // Act
        let chunks = chunks("aaaa\nbbbb\ncccc\n", 10, 0);

        // Assert
        assert_eq!(
            chunks,
            vec![
                vec![segment(1, "aaaa"), segment(2, "bbbb")],
                vec![segment(3, "cccc")]
            ]
        );
    }

    #[test]
    fn chunks_cut_a_long_line_into_overlapping_pieces() {
        // Act
        let chunks = chunks("あいうえおかきくけこさ", 4, 1);

        // Assert
        let texts: Vec<Vec<&str>> = chunks
            .iter()
            .map(|chunk| chunk.iter().map(|s| s.text).collect())
            .collect();
        assert_eq!(
            texts,
            vec![
                vec!["あいうえ"],
                vec!["えおかき"],
                vec!["きくけこ"],
                vec!["こさ"]
            ]
        );
        assert!(chunks.iter().flatten().all(|s| s.line == 1));
    }

    #[test]
    fn split_halves_a_chunk_of_one_line_with_an_overlap() {
        // Arrange
        let text = "山".repeat(150) + "田太郎" + &"川".repeat(147);

        // Act
        let (first, second) = split(vec![segment(7, &text)]);

        // Assert: the name straddles the middle and is whole in one half.
        assert!(first[0].text.contains("山田太郎") || second[0].text.contains("山田太郎"));
        assert!(first[0].text.chars().count() < 300 && second[0].text.chars().count() < 300);
        assert_eq!((first[0].line, second[0].line), (7, 7));
    }

    #[test]
    fn split_halves_a_chunk_of_several_lines_by_line() {
        // Act
        let (first, second) = split(vec![segment(1, "a"), segment(2, "b"), segment(3, "c")]);

        // Assert
        assert_eq!(first, vec![segment(1, "a")]);
        assert_eq!(second, vec![segment(2, "b"), segment(3, "c")]);
    }

    #[test]
    fn accept_keeps_japanese_and_western_names_and_drops_handles() {
        assert_eq!(
            accept("name", "山田 太郎"),
            Some((Kind::Name, "山田 太郎".into()))
        );
        assert_eq!(
            accept("Name", "Taro Yamada"),
            Some((Kind::Name, "Taro Yamada".into()))
        );
        assert_eq!(accept("name", "koto0120"), None);
        assert_eq!(accept("name", "gionmaru"), None);
        assert_eq!(accept("name", "TORICO"), None);
    }

    #[test]
    fn accept_keeps_birthdays_in_any_form_but_drops_identifiers() {
        assert_eq!(
            accept("birthday", "1990-01-01"),
            Some((Kind::Birthday, "1990-01-01".into()))
        );
        assert_eq!(
            accept("birthday", "1990年5月3日"),
            Some((Kind::Birthday, "1990年5月3日".into()))
        );
        assert_eq!(
            accept("birthday", "May 3, 1990"),
            Some((Kind::Birthday, "May 3, 1990".into()))
        );
        assert_eq!(accept("birthday", "user_1985"), None);
        assert_eq!(accept("birthday", "May"), None);
    }

    #[test]
    fn accept_counts_full_width_digits() {
        assert_eq!(
            accept("phone", "０９０ー１２３４ー５６７８"),
            Some((Kind::Phone, "０９０ー１２３４ー５６７８".into()))
        );
        assert_eq!(
            accept("national_id", "１２３４５６７８９０１８"),
            Some((Kind::MyNumber, "１２３４５６７８９０１８".into()))
        );
    }

    #[test]
    fn accept_drops_kinds_the_model_made_up() {
        assert_eq!(accept("user_id", "123456"), None);
        assert_eq!(accept("uuid", "0f8fad5b-d9cb-469f-a165-70867728950e"), None);
    }

    #[test]
    fn accept_keeps_a_my_number_only_if_its_check_digit_is_right() {
        assert_eq!(
            accept("national_id", "1234 5678 9018"),
            Some((Kind::MyNumber, "1234 5678 9018".into()))
        );
        assert_eq!(accept("national_id", "123456"), None);
        assert_eq!(accept("national_id", "123456789017"), None);
    }

    #[test]
    fn accept_drops_column_names_given_as_phones_and_addresses() {
        assert_eq!(accept("phone", "tel01"), None);
        assert_eq!(accept("address", "address_line1"), None);
        assert_eq!(
            accept("phone", "090-1234-5678"),
            Some((Kind::Phone, "090-1234-5678".into()))
        );
        assert_eq!(
            accept("address", "大阪市北区梅田2-2-2"),
            Some((Kind::Address, "大阪市北区梅田2-2-2".into()))
        );
    }

    #[test]
    fn locate_reports_every_line_the_value_is_on() {
        // Arrange
        let chunk = [
            segment(10, "担当: 山田太郎"),
            segment(11, "-"),
            segment(12, "承認: 山田太郎"),
        ];

        // Act
        let hits = locate(&chunk, Kind::Name, "山田太郎");

        // Assert
        let lines: Vec<(usize, Option<usize>)> =
            hits.iter().map(|h| (h.line, h.end_line)).collect();
        assert_eq!(lines, vec![(10, None), (12, None)]);
    }

    #[test]
    fn locate_tolerates_changed_spacing() {
        // Arrange
        let chunk = [segment(4, "氏名：山田　太郎")];

        // Act
        let hits = locate(&chunk, Kind::Name, "山田 太郎");

        // Assert
        assert_eq!(hits[0].line, 4);
        assert_eq!(hits[0].end_line, None);
    }

    #[test]
    fn locate_falls_back_to_the_lines_of_the_chunk() {
        // Arrange
        let chunk = [segment(4, "a"), segment(5, "b"), segment(9, "c")];

        // Act
        let hits = locate(&chunk, Kind::Address, "東京都");

        // Assert
        assert_eq!((hits[0].line, hits[0].end_line), (4, Some(9)));
    }

    #[test]
    fn parse_reads_the_answer_even_with_noise_around_it() {
        // Act
        let items = parse(
            "\n{\"items\": [{\"kind\": \"name\", \"value\": \"山田\"}], \"contains_pii\": true}\n",
        )
        .expect("parsed");

        // Assert
        assert_eq!(items.len(), 1);
        assert_eq!(
            (items[0].kind.as_str(), items[0].value.as_str()),
            ("name", "山田")
        );
    }

    #[test]
    fn parse_refuses_an_answer_missing_its_fields() {
        assert!(parse("{}").is_none());
        assert!(parse("{\"items\": [{}]}").is_none());
        assert!(parse("{\"items\": [{\"kind\": \"name\"}]}").is_none());
        assert!(parse("{\"items\": []}").is_some());
    }

    #[test]
    fn strip_ansi_removes_colour_codes() {
        assert_eq!(strip_ansi("\x1b[1mError\x1b[0m: x"), "Error: x");
    }
}
