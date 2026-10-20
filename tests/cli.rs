//! Runs the built binary over files made for each test.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

// A valid My Number made for these tests: 12345678901 + check digit 8.
const MY_NUMBER: &str = "123456789018";

fn piigrep(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_piigrep"))
        .current_dir(dir)
        .args(args)
        .env_remove("NO_COLOR")
        .output()
        .expect("piigrep starts")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8")
}

fn write(dir: &Path, path: &str, contents: impl AsRef<[u8]>) {
    let path = dir.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn tree() -> TempDir {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "customers.csv",
        format!(
            "id,mail,tel\n1,taro@example.jp,090-1234-5678\n2,,\n3,hanako@example.jp,\nmy number: {MY_NUMBER}\n"
        ),
    );
    write(
        dir.path(),
        "notes/readme.md",
        "# Setup\n\nRun `make` with version 1.2.3.\n",
    );
    dir
}

#[test]
fn reports_each_find_as_path_line_kind_value_and_exits_zero() {
    // Arrange
    let dir = tree();

    // Act
    let output = piigrep(dir.path(), &[]);

    // Assert
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        stdout(&output),
        "customers.csv:2:email:taro@example.jp\n\
         customers.csv:2:phone:090-1234-5678\n\
         customers.csv:4:email:hanako@example.jp\n\
         customers.csv:5:my_number:123456789018\n"
    );
}

#[test]
fn exits_one_when_nothing_is_found() {
    // Arrange
    let dir = tree();

    // Act
    let output = piigrep(dir.path(), &["notes"]);

    // Assert
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(stdout(&output), "");
}

#[test]
fn exits_two_for_a_path_that_does_not_exist() {
    // Act
    let output = piigrep(tree().path(), &["missing"]);

    // Assert
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing"));
}

#[test]
fn reads_shift_jis() {
    // Arrange
    let dir = TempDir::new().unwrap();
    let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("氏名,電話\n山田,０３ー１２３４ー５６７８\n");
    write(dir.path(), "sjis.csv", bytes);

    // Act
    let output = piigrep(dir.path(), &[]);

    // Assert
    assert_eq!(
        stdout(&output),
        "sjis.csv:2:phone:０３ー１２３４ー５６７８\n"
    );
}

#[test]
fn searches_gitignored_files_unless_told_not_to() {
    // Arrange
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    write(dir.path(), ".gitignore", "dump.sql\n");
    write(
        dir.path(),
        "dump.sql",
        "INSERT INTO users VALUES ('a@example.jp');\n",
    );

    // Act
    let by_default = piigrep(dir.path(), &["-l"]);
    let with_gitignore = piigrep(dir.path(), &["-l", "--gitignore"]);

    // Assert
    assert_eq!(stdout(&by_default), "dump.sql\n");
    assert_eq!(with_gitignore.status.code(), Some(1), "{with_gitignore:?}");
}

#[test]
fn skips_node_modules_unless_told_not_to() {
    // Arrange
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "node_modules/pkg/README.md",
        "author: a@example.jp\n",
    );

    // Act
    let by_default = piigrep(dir.path(), &["-l"]);
    let everything = piigrep(dir.path(), &["-l", "--no-default-excludes"]);

    // Assert
    assert_eq!(by_default.status.code(), Some(1), "{by_default:?}");
    assert_eq!(stdout(&everything), "node_modules/pkg/README.md\n");
}

#[test]
fn searches_hidden_files() {
    // Arrange
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "ADMIN_EMAIL=a@example.jp\n");

    // Act
    let output = piigrep(dir.path(), &["-l"]);

    // Assert
    assert_eq!(stdout(&output), ".env\n");
}

#[test]
fn glob_narrows_and_excludes() {
    // Arrange
    let dir = TempDir::new().unwrap();
    write(dir.path(), "a.csv", "a@example.jp\n");
    write(dir.path(), "b.log", "b@example.jp\n");

    // Act
    let only_csv = piigrep(dir.path(), &["-l", "-g", "*.csv"]);
    let no_csv = piigrep(dir.path(), &["-l", "-g", "!*.csv"]);

    // Assert
    assert_eq!(stdout(&only_csv), "a.csv\n");
    assert_eq!(stdout(&no_csv), "b.log\n");
}

#[test]
fn kind_filters_what_is_reported() {
    // Act
    let output = piigrep(tree().path(), &["-k", "phone,my_number"]);

    // Assert
    assert_eq!(
        stdout(&output),
        "customers.csv:2:phone:090-1234-5678\ncustomers.csv:5:my_number:123456789018\n"
    );
}

#[test]
fn asking_for_names_without_fm_is_an_error() {
    // Act
    let output = piigrep(tree().path(), &["-k", "name"]);

    // Assert
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("only with --fm"));
}

#[test]
fn reads_standard_input_for_a_dash() {
    // Arrange
    let mut child = Command::new(env!("CARGO_BIN_EXE_piigrep"))
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello\ncall 03-1234-5678\n")
        .unwrap();

    // Act
    let output = child.wait_with_output().unwrap();

    // Assert
    assert_eq!(stdout(&output), "<stdin>:2:phone:03-1234-5678\n");
}

#[test]
fn skips_binary_and_oversized_files() {
    // Arrange
    let dir = TempDir::new().unwrap();
    write(dir.path(), "image.bin", b"\x89PNG\x00\x00a@example.jp");
    write(
        dir.path(),
        "big.txt",
        format!("{}\na@example.jp\n", "x".repeat(2000)),
    );
    write(dir.path(), "small.txt", "a@example.jp\n");

    // Act
    let output = piigrep(dir.path(), &["-l", "--max-filesize", "1K", "-v"]);

    // Assert
    assert_eq!(stdout(&output), "small.txt\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("image.bin: skipped (binary)"), "{stderr}");
    assert!(
        stderr.contains("big.txt: skipped (larger than --max-filesize)"),
        "{stderr}"
    );
}

#[test]
fn tsv_and_json_formats() {
    // Arrange
    let dir = tree();

    // Act
    let tsv = stdout(&piigrep(dir.path(), &["--format", "tsv", "-k", "phone"]));
    let json = stdout(&piigrep(dir.path(), &["--format", "json", "-k", "phone"]));

    // Assert
    assert_eq!(
        tsv,
        "path\tline\tkind\tvalue\tsource\ncustomers.csv\t2\tphone\t090-1234-5678\tregex\n"
    );
    let row: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(row["path"], "customers.csv");
    assert_eq!(row["line"], 2);
    assert_eq!(row["value"], "090-1234-5678");
}

#[test]
fn colour_is_off_when_piped_and_on_when_forced() {
    // Act
    let piped = stdout(&piigrep(tree().path(), &["-k", "phone"]));
    let forced = stdout(&piigrep(
        tree().path(),
        &["-k", "phone", "--color", "always"],
    ));

    // Assert
    assert!(!piped.contains('\x1b'));
    assert!(forced.contains("\x1b[35mcustomers.csv\x1b[0m"));
}

/// Needs macOS 27 with Apple Intelligence: `cargo test -- --ignored`.
#[test]
#[ignore]
fn fm_finds_a_name_and_an_address() {
    // Arrange
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "memo.md",
        "-- 打ち合わせメモ\n担当は山田太郎さん。\n住所は大阪府大阪市北区梅田2-2-2です。\n",
    );

    // Act
    let output = piigrep(dir.path(), &["--fm-only", "--format", "tsv"]);

    // Assert
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(out.contains("memo.md\t2\tname\t山田太郎\tfm"), "{out}");
    assert!(out.contains("\taddress\t"), "{out}");
}
