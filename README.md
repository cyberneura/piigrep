# piigrep

Find personal information in files, on-device.

```console
$ piigrep ~/exports
customers.csv:2:email:taro@example.jp
customers.csv:2:phone:090-1234-5678
customers.csv:5:my_number:123456789018
```

piigrep walks a directory like `grep -r` and reports each piece of personal
information it finds as `path:line:kind:value`. It runs in two tiers:

- **Patterns** (default, fast): email addresses, Japanese phone numbers, postal
  codes, My Numbers (マイナンバー) and credit card numbers. My Numbers must pass
  their check digit and card numbers the Luhn check, and a number that is part
  of a longer ID is ignored. Full-width digits (`０９０ー１２３４ー５６７８`)
  are understood, and values are reported as written.
- **The on-device model** (`--fm`, macOS 27+): names, addresses and birthdays,
  which no pattern can find. piigrep asks Apple's Foundation Model through the
  `fm` command. The model runs on the Mac; nothing is sent anywhere.

Files are read as UTF-8, as Shift_JIS when they are not UTF-8, or as UTF-16
when they start with a BOM. Binary files are skipped.

## Install

```sh
brew install cyberneura/tap/piigrep          # formula
brew install --cask cyberneura/tap/piigrep   # or the cask (macOS)
cargo install piigrep
```

Binaries for Apple Silicon macOS (signed and notarized) and x86_64 GNU/Linux
(glibc; not Alpine or other musl systems) are also on the
[releases page](https://github.com/cyberneura/piigrep/releases).

## Usage

```sh
piigrep                         # the current directory
piigrep ~/Downloads notes.txt   # several paths
cat dump.sql | piigrep -        # standard input
piigrep -l ~/exports            # only the names of files with something found
piigrep -k email,phone .        # only these kinds
piigrep -g '*.csv' -g '!old/**' # only CSV files, skipping old/
piigrep --format json .         # one JSON object per line (also: tsv)
```

Kinds: `email`, `phone`, `postal_code`, `my_number`, `credit_card`, and with
`--fm` also `name`, `address`, `birthday`.

The patterns are for Japanese formats. Landline numbers written without
separators (`0312345678`) are not matched, as they cannot be told from
zero-padded IDs; nor are postal codes with neither a hyphen nor `〒`
(`1000005`), nor toll-free numbers (`0120-123-456`), which belong to
businesses. Other countries' ID numbers are not looked for.

### What is searched

Unlike ripgrep, piigrep searches **hidden files and files listed in
`.gitignore`** by default, since `.env` files, exports and database dumps are
where personal information tends to sit. `--gitignore` turns that back on.

`.git`, `node_modules`, `.venv`, `venv`, `__pycache__`, `.next`, `.cache`,
`dist` and `build` are skipped; `--no-default-excludes` searches them too.
Files over 50 MB are skipped (`--max-filesize`).

### The on-device model (`--fm`)

```sh
piigrep --fm ~/notes                # patterns and the model
piigrep --fm --fm-unmatched ~/notes # the model only where the patterns found nothing
piigrep --fm-only memo.txt          # the model alone
```

Requires macOS 27 or later with Apple Intelligence turned on (`fm available`
should print "System model available").

- **It is slow.** Text is sent to the model 2,000 characters at a time, and
  each request takes about 4 seconds. A folder of 1 MB of text takes around half
  an hour. Running requests in parallel barely helps, so piigrep does not. Use
  `--fm-unmatched`, `-g` or `--max-filesize` to cut down what the model sees.
- **It is a small model.** It misses some names and addresses, and it calls IDs
  and handles names. piigrep drops most of the latter (a name has to contain
  Japanese or look like `First Last`), so English-language handles and
  single-word names are not reported.
- When the model returns a value that is not in the text verbatim, the hit is
  reported against the range of lines it was given: `memo.txt:40-72:address:...`.
- A part the model refuses (its safety guardrails) or fails on is counted, and
  piigrep exits 2. `-v` lists which lines those were.

### Exit status

| Status | Meaning |
|---|---|
| 0 | Something was found |
| 1 | Nothing was found |
| 2 | An error occurred, including a part the model could not check |

## License

MIT. `piigrep --license` prints the licenses of the libraries it is built from.
