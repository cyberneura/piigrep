use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

use piigrep::detect::Detector;
use piigrep::fm::Fm;
use piigrep::output::{Format, Printer, printable};
use piigrep::text::{Decoded, decode};
use piigrep::{Hit, Kind, merge};

/// Generated, vendored or version-control directories. Searching them is slow
/// and finds test fixtures rather than anybody's data.
const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
    "dist",
    "build",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

/// Find personal information in files, on-device.
///
/// Email addresses, Japanese phone numbers, postal codes, My Numbers and card
/// numbers are found with patterns (My Numbers and card numbers must pass their
/// check digit). With --fm, Apple's on-device model (the `fm` command of macOS 27)
/// is asked for names, addresses and birthdays as well. Nothing leaves the machine.
///
/// Exits 0 if anything was found, 1 if nothing was, and 2 on an error, including
/// a part of a file the model could not check.
#[derive(Parser, Debug)]
#[command(name = "piigrep", version)]
struct Args {
    /// Files or directories to search; `-` reads standard input. Defaults to the
    /// current directory.
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Also ask the on-device model for names, addresses and birthdays. Slow: about
    /// 4 seconds per 2,000 characters.
    #[arg(long)]
    fm: bool,

    /// Ask only the model; skip the patterns.
    #[arg(long, conflicts_with_all = ["fm", "fm_unmatched"])]
    fm_only: bool,

    /// With --fm, ask the model only about files the patterns found nothing in.
    #[arg(long, requires = "fm")]
    fm_unmatched: bool,

    /// Report only these kinds (comma-separated).
    #[arg(short, long, value_enum, value_delimiter = ',', value_name = "KIND")]
    kind: Vec<Kind>,

    /// Print only the paths of files with something found.
    #[arg(short = 'l', long)]
    files_with_matches: bool,

    #[arg(long, value_enum, default_value_t = Format::Grep)]
    format: Format,

    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN")]
    color: ColorChoice,

    /// Search only what matches the glob, or skip it with a leading `!`. Repeatable.
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    globs: Vec<String>,

    /// Skip what .gitignore and .ignore files list. Off by default, because
    /// ignored files are where exports and dumps usually sit.
    #[arg(long)]
    gitignore: bool,

    /// Also search .git, node_modules, .venv, dist, build and the like.
    #[arg(long)]
    no_default_excludes: bool,

    /// Skip files larger than this (suffixes K, M, G).
    #[arg(long, default_value = "50M", value_parser = parse_size, value_name = "SIZE")]
    max_filesize: u64,

    /// Report skipped files and each part the model could not check.
    #[arg(short, long)]
    verbose: bool,

    /// Print the license of piigrep and of the libraries it is built from.
    #[arg(long, exclusive = true)]
    license: bool,
}

fn parse_size(s: &str) -> Result<u64, String> {
    let (number, unit) = match s.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&s[..i], c.to_ascii_uppercase()),
        _ => (s, ' '),
    };
    let multiplier: u64 = match unit {
        ' ' => 1,
        'K' => 1 << 10,
        'M' => 1 << 20,
        'G' => 1 << 30,
        _ => return Err(format!("unknown unit in {s:?}; use K, M or G")),
    };
    let number: u64 = number.parse().map_err(|_| format!("not a size: {s:?}"))?;
    number
        .checked_mul(multiplier)
        .ok_or_else(|| format!("too large: {s:?}"))
}

fn main() -> ExitCode {
    let args = Args::parse();
    if args.license {
        return print_license();
    }
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("piigrep: {}", printable(&format!("{error:#}")));
            ExitCode::from(2)
        }
    }
}

/// A closed pipe is the reader having seen enough (`| head`), not a failure.
fn print_license() -> ExitCode {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(piigrep::notices::license_text().as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("piigrep: could not print the license: {error}");
            ExitCode::from(2)
        }
    }
}

struct Scanner {
    detector: Detector,
    fm: Option<Fm>,
    use_patterns: bool,
    fm_unmatched: bool,
    kinds: Vec<Kind>,
    verbose: bool,
    progress: bool,
    found: bool,
    errors: usize,
    fm_failed_chunks: usize,
}

impl Scanner {
    /// `label` is the path as it will be printed; on stderr it is escaped here.
    fn scan(&mut self, label: &str, bytes: &[u8]) -> Vec<Hit> {
        let shown = printable(label);
        let text = match decode(bytes) {
            Decoded::Text(text) => text,
            Decoded::Binary => {
                self.skip(label, "binary");
                return Vec::new();
            }
            Decoded::Undecodable => {
                self.skip(label, "neither UTF-8 nor Shift_JIS");
                return Vec::new();
            }
        };
        let mut hits = if self.use_patterns {
            self.detector.scan(&text)
        } else {
            Vec::new()
        };
        if let Some(fm) = &self.fm
            && !(self.fm_unmatched && !hits.is_empty())
        {
            let progress = self.progress;
            let scan = fm.scan(&text, &mut |done, total| {
                if progress {
                    eprint!("\r\x1b[Kpiigrep: asking fm about {shown} ({done}/{total})");
                }
            });
            if progress {
                eprint!("\r\x1b[K");
            }
            for failure in &scan.failures {
                self.fm_failed_chunks += 1;
                if self.verbose {
                    eprintln!(
                        "piigrep: {shown}:{}-{}: fm could not check these lines: {}",
                        failure.first_line,
                        failure.last_line,
                        printable(&failure.reason)
                    );
                }
            }
            hits.extend(scan.hits);
        }
        let mut hits = merge(hits);
        if !self.kinds.is_empty() {
            hits.retain(|hit| self.kinds.contains(&hit.kind));
        }
        self.found |= !hits.is_empty();
        hits
    }

    fn skip(&self, label: &str, why: &str) {
        if self.verbose {
            eprintln!("piigrep: {}: skipped ({why})", printable(label));
        }
    }

    fn error(&mut self, message: impl std::fmt::Display) {
        eprintln!("piigrep: {}", printable(&message.to_string()));
        self.errors += 1;
    }
}

fn display(path: &Path) -> String {
    let shown = path.display().to_string();
    match shown.strip_prefix("./") {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => shown,
    }
}

fn run(args: Args) -> anyhow::Result<ExitCode> {
    let use_fm = args.fm || args.fm_only;
    let model_only: Vec<&str> = args
        .kind
        .iter()
        .filter(|kind| !kind.has_pattern())
        .map(|kind| kind.as_str())
        .collect();
    if !use_fm && !model_only.is_empty() {
        if model_only.len() == args.kind.len() {
            anyhow::bail!("{} can be found only with --fm", model_only.join(", "));
        }
        eprintln!(
            "piigrep: {} can be found only with --fm",
            model_only.join(", ")
        );
    }

    let color = match args.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    let mut scanner = Scanner {
        detector: Detector::new(),
        fm: if use_fm { Some(Fm::new()?) } else { None },
        use_patterns: !args.fm_only,
        fm_unmatched: args.fm_unmatched,
        kinds: args.kind.clone(),
        verbose: args.verbose,
        progress: use_fm && io::stderr().is_terminal(),
        found: false,
        errors: 0,
        fm_failed_chunks: 0,
    };
    let mut printer = Printer::new(
        io::BufWriter::new(io::stdout().lock()),
        args.format,
        color,
        args.files_with_matches,
    )?;

    let mut paths = args.paths.clone();
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }
    let read_stdin = paths.iter().any(|path| path.as_os_str() == "-");
    paths.retain(|path| path.as_os_str() != "-");

    let written = (|| -> io::Result<()> {
        if read_stdin {
            let mut bytes = Vec::new();
            io::stdin().read_to_end(&mut bytes)?;
            let hits = scanner.scan("<stdin>", &bytes);
            printer.file("<stdin>", &hits)?;
        }
        let Some((first, rest)) = paths.split_first() else {
            return Ok(());
        };
        for entry in walker(first, rest, &args).map_err(io::Error::other)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    scanner.error(error);
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let label = display(entry.path());
            match entry.metadata() {
                Ok(metadata) if metadata.len() > args.max_filesize => {
                    scanner.skip(&label, "larger than --max-filesize");
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    scanner.error(error);
                    continue;
                }
            }
            let bytes = match std::fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(error) => {
                    scanner.error(format_args!("{label}: {error}"));
                    continue;
                }
            };
            let hits = scanner.scan(&label, &bytes);
            printer.file(&label, &hits)?;
        }
        Ok(())
    })();
    match written {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            return Ok(ExitCode::from(if scanner.found { 0 } else { 1 }));
        }
        Err(error) => return Err(error.into()),
    }

    if scanner.fm_failed_chunks > 0 {
        eprintln!(
            "piigrep: fm could not check {} part(s) of the input{}",
            scanner.fm_failed_chunks,
            if args.verbose {
                ""
            } else {
                "; rerun with -v to see which"
            }
        );
    }
    Ok(ExitCode::from(
        if scanner.errors > 0 || scanner.fm_failed_chunks > 0 {
            2
        } else if scanner.found {
            0
        } else {
            1
        },
    ))
}

fn walker(first: &Path, rest: &[PathBuf], args: &Args) -> anyhow::Result<ignore::Walk> {
    let mut builder = WalkBuilder::new(first);
    for path in rest {
        builder.add(path);
    }
    let gitignore = args.gitignore;
    builder
        .hidden(false)
        .ignore(gitignore)
        .git_ignore(gitignore)
        .git_global(gitignore)
        .git_exclude(gitignore)
        .parents(gitignore)
        .sort_by_file_name(|a, b| a.cmp(b));
    if !args.no_default_excludes {
        builder.filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|t| t.is_dir())
                || !DEFAULT_EXCLUDES
                    .iter()
                    .any(|name| entry.file_name() == *name)
        });
    }
    if !args.globs.is_empty() {
        let mut overrides = OverrideBuilder::new(std::env::current_dir()?);
        for glob in &args.globs {
            overrides.add(glob)?;
        }
        builder.overrides(overrides.build()?);
    }
    Ok(builder.build())
}
