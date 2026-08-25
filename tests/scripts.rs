#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable fixture");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fixture executable");
}

fn command_path(command: &str) -> PathBuf {
    let output = Command::new("/bin/sh")
        .args(["-c", "command -v \"$1\"", "sh", command])
        .output()
        .expect("resolve command path");
    assert!(output.status.success(), "command not found: {command}");
    PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("command path is UTF-8")
            .trim(),
    )
}

fn pinned_versions() -> (String, String) {
    let manifest: toml::Value = toml::from_str(include_str!("../crates/suiko-sudachi/Cargo.toml"))
        .expect("parse suiko-sudachi manifest");
    let sudachi = manifest["package"]["version"]
        .as_str()
        .expect("package version");
    let dictionary = include_str!("../build.rs")
        .lines()
        .find(|line| line.starts_with("const DICT_NAME"))
        .and_then(|line| line.split('"').nth(1))
        .and_then(|name| name.split_whitespace().nth(1))
        .expect("dictionary version");
    (sudachi.to_owned(), dictionary.to_owned())
}

#[test]
fn sudachi_update_check_runs_without_python() {
    let dir = tempdir().expect("temporary directory");
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).expect("create fixture bin directory");
    for command in ["cut", "dirname", "grep", "head", "sed"] {
        symlink(command_path(command), bin.join(command)).expect("link fixture command");
    }
    let (sudachi, dictionary) = pinned_versions();
    write_executable(
        &bin.join("gh"),
        &format!(
            r#"#!/bin/sh
case "$2" in
  repos/WorksApplications/sudachi.rs/tags) printf '%s\n' '{sudachi}' ;;
  repos/WorksApplications/SudachiDict/releases/latest) printf '%s\n' '{dictionary}' ;;
  *) exit 2 ;;
esac
"#
        ),
    );
    write_executable(&bin.join("curl"), "#!/bin/sh\nprintf '%s' '404'\n");

    let output = Command::new("/bin/sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/check-sudachi-updates.sh"
        ))
        .env("PATH", &bin)
        .output()
        .expect("run update check");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("更新なし"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
