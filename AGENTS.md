# piigrep

Rust CLI that finds personal information in files. Patterns for fixed formats
(`src/detect.rs`), Apple's on-device model through the `fm` command for names,
addresses and birthdays (`src/fm.rs`).

## Layout

- `src/main.rs`: CLI, file walking (`ignore` crate), exit status
- `src/detect.rs`: patterns, check digit (My Number) and Luhn validation
- `src/text.rs`: decoding (UTF-8 / Shift_JIS / UTF-16 BOM) and full-width → ASCII mapping
- `src/fm.rs`: chunking, `fm respond` calls, filtering the model's answers
- `src/output.rs`: grep / tsv / json output
- `src/notices.rs`: `--license`

## Commands

```sh
cargo test                       # everything that needs no model
cargo test -- --include-ignored  # also the tests that call fm (macOS 27 + Apple Intelligence)
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
scripts/generate-third-party-notices.sh   # after adding or upgrading a dependency
scripts/release.sh [patch|minor|major]    # bump, commit and push; CI releases
```

## Things that are not obvious

- **Patterns use the `regex` crate with boundaries checked in code**, not
  lookaround. fancy-regex supports lookaround but backtracks at every position
  and ran at about 5 MB/s. When a candidate is rejected, the search resumes one
  character later (what lookaround would do), except when the left boundary
  failed, where it jumps past the whole run: retrying each position is quadratic
  in a long run of digits.
- **A space is always a boundary.** `090 1234 5678 1990/01/01` is a phone
  number and a date. The first 12 digits of a card number can pass the My Number
  check digit, so cards are matched first and a My Number candidate that overlaps
  one is rejected, the search going on from the next character. Dropping it after
  matching would skip over a real My Number right after the card.
  A long space-separated run of 4-digit groups stays ambiguous:
  `4000 0000 0190 4111 1111 1111 1111` can be read as a My Number and a card, or
  as a card starting at the first group, and both pass their checks. Any rule
  that settles it breaks `4111 1111 1111 1111 1234 5678 9018` (a card, then a My
  Number), so the leftmost card wins.
- **Boundaries are ASCII.** `マイナンバー123456789018` must match, so a Japanese
  character before a number is not a boundary violation. Python's `\w` (used by
  the prototype) is Unicode and missed this.
- **`ー` is a dash only between digits.** Elsewhere it is the katakana long
  vowel mark, and mapping it to `-` made `マイナンバー123…` look like part of an ID.
- **Full-width characters are mapped one character to one**, so a match in the
  normalized line maps back to the original text by character offsets
  (`Normalized::original_range`). Values are reported as written.
- **`fm` gets the prompt on stdin.** As an argument, a text starting with `-`
  (an SQL comment) is parsed as an option and the chunk is lost.
- **The `fm` schema is generated at run time** with `fm schema object`. `fm`
  reads only the format it writes, not plain JSON Schema.
- **The model's context is 4,096 tokens.** Chunks are 2,000 characters, and a
  chunk the model calls too long is halved and retried, down to 200 characters.
  Where a line has to be cut, the pieces overlap by 100 characters so a name on
  the cut is seen whole once.
- **An answer from `fm` without `items`, `kind` or `value` is a failure**, not an
  empty result.
- **Paths and values are escaped before printing** (`output::printable`): they
  come from what is searched, and an escape sequence in a file name would be run
  by the terminal. JSON output is escaped by serde instead.
- **Parallel `fm` calls barely help** (4 at once: 15.7 s → 13.2 s); the model
  serializes them. Calls are sequential.
- **A chunk `fm` fails on is an error (exit 2)**, not silence. A scan that skipped
  part of a file must not look clean.
- **The model's answers are filtered** (`fm::accept`): invented kinds are dropped,
  names must contain Japanese or look like `First Last`, single ASCII tokens are
  not addresses, phones need 10–13 digits, My Numbers must pass the check digit.
  Without this the model reports user IDs and handles by the hundred.
- Hidden and gitignored files are searched by default, on purpose: that is where
  dumps and `.env` files are.

## Release

Pushing a version to main that has no GitHub Release yet releases it
(`.github/workflows/release.yml`): tests, builds for aarch64-apple-darwin and
x86_64-unknown-linux-gnu, then a GitHub Release. crates.io publishing runs only
when the repository variable `PUBLISH_CRATES` is `true` and the secret
`CARGO_REGISTRY_TOKEN` is set. Whether to publish the crate is decided apart
from the GitHub Release (`scripts/crate-indexed.sh` asks the sparse index), and
the crate is published from the release tag, so a failed publish is retried by
the next push even though the release is already out. The macOS binary is signed with a
Developer ID and notarised in the build job (repository secrets
`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`,
`APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`; the build stops if one is
missing). The cask and browser downloads quarantine the binary, so an unsigned
one would be refused.
The Homebrew formula lives in `cyberneura/homebrew-tap`
(`Formula/piigrep.rb`); the tap updates it from the latest release on its own
schedule.
