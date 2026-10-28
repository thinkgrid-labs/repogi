//! The executable identifies itself as `repogi`.

use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");

#[test]
fn reports_the_repogi_name_and_version() {
    let version = Command::new(BINARY).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        format!("repogi {}\n", env!("CARGO_PKG_VERSION"))
    );
    let help = Command::new(BINARY).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.starts_with("Repogi (Repo Graph Intelligence)"));
    assert!(help.contains("Usage: repogi <COMMAND>"));
}
