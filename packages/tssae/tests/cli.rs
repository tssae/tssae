use std::process::Command;

#[test]
fn prints_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_tssae-engine"))
        .arg("--version")
        .output()
        .expect("engine must start");

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        concat!(env!("CARGO_PKG_VERSION"), "\n").as_bytes()
    );
    assert_eq!(output.stderr, b"");
}

#[test]
fn rejects_missing_unknown_and_extra_arguments() {
    for args in [
        &[][..],
        &["--unknown"][..],
        &["--version", "extra"][..],
        &["serve"][..],
        &["serve", "--unknown"][..],
        &["serve", "--stdio", "extra"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tssae-engine"))
            .args(args)
            .output()
            .expect("engine must start");

        assert_eq!(output.status.code(), Some(2));
        assert_eq!(output.stdout, b"");
        assert_eq!(
            output.stderr,
            b"Usage: tssae-engine (--version | serve --stdio)\n"
        );
    }
}
