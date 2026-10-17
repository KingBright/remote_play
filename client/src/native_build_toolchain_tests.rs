//! Exercise the actual production builder in subprocesses: no process-global
//! environment mutation or product/profile/network initialization.
use std::{env, path::Path, process::Command};
#[path = "../build_toolchain.rs"]
mod production;

#[test]
fn selection_child() {
    let Ok(compiler) = env::var("WEAVELO_TOOL_SELECTION_COMPILER") else {
        return;
    };
    let archiver = env::var("WEAVELO_TOOL_SELECTION_ARCHIVER").unwrap();
    let build = production::native_video(Path::new("unused-headers"));
    assert_eq!(build.get_compiler().path(), Path::new(&compiler));
    assert_eq!(build.get_archiver().get_program(), Path::new(&archiver));
}

#[test]
fn cargo_target_tool_precedence_and_native_defaults() {
    // Distinct existing executables make selection observable without compiling
    // C or creating an archive. Failed family probes only select cc-rs's GNU
    // fallback; no selected tool is used to build the product in this test.
    let cases: &[(&str, &str, &[(&str, &str)], &str, &str)] = &[
        (
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            &[
                ("CC_x86_64-unknown-linux-gnu", "/usr/bin/true"),
                ("AR_x86_64-unknown-linux-gnu", "/usr/bin/true"),
                ("CC_x86_64_unknown_linux_gnu", "/usr/bin/false"),
                ("AR_x86_64_unknown_linux_gnu", "/usr/bin/false"),
                ("TARGET_CC", "/usr/bin/false"),
                ("TARGET_AR", "/usr/bin/false"),
                ("CC", "/usr/bin/false"),
                ("AR", "/usr/bin/false"),
            ],
            "/usr/bin/true",
            "/usr/bin/true",
        ),
        (
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            &[
                ("CC_x86_64_unknown_linux_gnu", "/usr/bin/true"),
                ("AR_x86_64_unknown_linux_gnu", "/usr/bin/true"),
                ("TARGET_CC", "/usr/bin/false"),
                ("TARGET_AR", "/usr/bin/false"),
            ],
            "/usr/bin/true",
            "/usr/bin/true",
        ),
        (
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            &[
                ("TARGET_CC", "/usr/bin/true"),
                ("TARGET_AR", "/usr/bin/true"),
                ("CC", "/usr/bin/false"),
                ("AR", "/usr/bin/false"),
            ],
            "/usr/bin/true",
            "/usr/bin/true",
        ),
        (
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            &[("CC", "/usr/bin/true"), ("AR", "/usr/bin/true")],
            "/usr/bin/true",
            "/usr/bin/true",
        ),
        (
            "aarch64-apple-darwin",
            "aarch64-apple-darwin",
            &[],
            "cc",
            "ar",
        ),
    ];
    let out = tempfile::tempdir().unwrap();
    for (target, host, settings, compiler, archiver) in cases {
        let mut child = Command::new(env::current_exe().unwrap());
        child.args([
            "--exact",
            "native_build_toolchain_tests::selection_child",
            "--nocapture",
        ]);
        for (key, _) in env::vars_os() {
            let key_text = key.to_string_lossy();
            if key_text.starts_with("CC")
                || key_text.starts_with("AR")
                || key_text.starts_with("CFLAGS")
                || key_text.starts_with("TARGET_")
                || key_text.starts_with("HOST_")
                || key_text.starts_with("CARGO_TARGET_")
            {
                child.env_remove(key);
            }
        }
        child
            .env("TARGET", target)
            .env("HOST", host)
            .env("OUT_DIR", out.path())
            .env("OPT_LEVEL", "0")
            .env("DEBUG", "false")
            .env("CRATE_CC_NO_DEFAULTS", "1")
            .env("WEAVELO_TOOL_SELECTION_COMPILER", compiler)
            .env("WEAVELO_TOOL_SELECTION_ARCHIVER", archiver);
        for (key, value) in *settings {
            child.env(key, value);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{target}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
