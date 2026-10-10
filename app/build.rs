use serde_json::Value;
use std::{env, fs, path::Path};

fn check(ok: bool, message: &str) {
    assert!(ok, "RemotePlay build identity rejected: {message}");
}

fn bounded_file(path: &Path) -> Vec<u8> {
    check(path.is_absolute(), "absolute identity input required");
    check(
        path.canonicalize().ok().as_deref() == Some(path),
        "linked/noncanonical identity input",
    );
    let metadata = fs::symlink_metadata(path).expect("identity input unavailable");
    check(
        metadata.is_file() && metadata.len() <= 65536,
        "invalid identity input",
    );
    fs::read(path).expect("identity input unavailable")
}

fn hex(value: &Value, length: usize) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == length
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn generated_module(raw: &[u8]) -> String {
    let bytes = raw.iter().map(u8::to_string).collect::<Vec<_>>().join(",");
    format!(
        "#[used]\n\
         #[cfg_attr(target_os = \"windows\", unsafe(link_section = \".rpbuild\"))]\n\
         #[cfg_attr(target_os = \"macos\", unsafe(link_section = \"__TEXT,__rpbuild\"))]\n\
         pub static BUILD_IDENTITY_BYTES: [u8; {}] = [{}];\n\
         pub fn build_identity_bytes() -> &'static [u8] {{ &BUILD_IDENTITY_BYTES }}\n",
        raw.len(),
        bytes
    )
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for key in [
        "REMOTEPLAY_BUILD_IDENTITY_FILE",
        "REMOTEPLAY_BUILD_IDENTITY_RS",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let output = Path::new(&env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("compiled_build_identity.rs");
    let record = env::var_os("REMOTEPLAY_BUILD_IDENTITY_FILE");
    let module = env::var_os("REMOTEPLAY_BUILD_IDENTITY_RS");
    let (Some(record), Some(module)) = (record.clone(), module.clone()) else {
        check(
            record.is_none() && module.is_none(),
            "incomplete identity inputs",
        );
        check(
            env::var("PROFILE").as_deref() != Ok("release"),
            "release requires observed build identity inputs",
        );
        fs::write(
            output,
            "pub fn build_identity_bytes() -> &'static [u8] { &[] }\n",
        )
        .expect("write development identity");
        return;
    };
    let record = Path::new(&record);
    let module = Path::new(&module);
    let raw = bounded_file(record);
    let supplied_module = bounded_file(module);
    check(!raw.is_empty(), "empty identity record");
    let identity: Value = serde_json::from_slice(&raw).expect("invalid identity JSON");
    let source = &identity["source"];
    let config = &identity["configuration"];
    check(identity["schema"].as_u64() == Some(1), "identity schema");
    for (field, length) in [("commit", 40), ("git_tree", 40), ("snapshot_sha256", 64)] {
        check(hex(&source[field], length), "source digest");
    }
    check(
        source["file_count"]
            .as_u64()
            .is_some_and(|count| count > 0 && count <= 20000),
        "source inventory",
    );
    check(
        (source["state"] == "clean" && source["patch_sha256"].is_null())
            || (source["state"] == "patch" && hex(&source["patch_sha256"], 64)),
        "source state",
    );
    for field in ["configuration_sha256", "identity_sha256"] {
        check(hex(&identity[field], 64), "identity digest");
    }
    for field in ["toolchain_sha256", "environment_sha256"] {
        check(hex(&config[field], 64), "compiler configuration digest");
    }
    let platform = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS");
    let expected_features: &[&str] = match platform.as_str() {
        "macos" => &["gpui-restoration"],
        "linux" => &["gpui-restoration", "native-linux-video"],
        "windows" => &["gpui-restoration", "native-windows-video"],
        _ => panic!("Unsupported product identity platform"),
    };
    check(config["platform"] == platform, "platform");
    check(
        config["architecture"] == env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo architecture"),
        "architecture",
    );
    check(
        config["target"] == env::var("TARGET").expect("Cargo target"),
        "target triple",
    );
    check(
        config["version"] == env::var("CARGO_PKG_VERSION").expect("Cargo package version"),
        "package version",
    );
    let profile = env::var("PROFILE").expect("Cargo profile");
    check(
        config["profile"]
            == if profile == "debug" {
                "dev"
            } else {
                profile.as_str()
            },
        "profile",
    );
    check(
        config["package"] == "remote_play_app" && config["binary"] == "remote_play",
        "product target",
    );
    check(config["gui_entry"] == "restored-original-gpui", "GUI entry");
    check(
        config["features"] == serde_json::json!(expected_features),
        "requested features",
    );
    check(
        config["default_features"] == false
            && config["locked"] == true
            && config["offline"] == true,
        "build mode",
    );
    check(
        config["gui_arguments"]
            == if platform == "macos" {
                serde_json::json!(["--gui"])
            } else {
                serde_json::json!([])
            },
        "GUI arguments",
    );
    check(
        env::var_os("CARGO_FEATURE_DEFAULT").is_none(),
        "default features enabled",
    );
    let enabled = |name| env::var_os(name).is_some();
    check(
        enabled("CARGO_FEATURE_GPUI_RESTORATION"),
        "original GUI not compiled",
    );
    check(
        enabled("CARGO_FEATURE_NATIVE_LINUX_VIDEO") == (platform == "linux"),
        "native Linux feature",
    );
    check(
        enabled("CARGO_FEATURE_NATIVE_WINDOWS_VIDEO") == (platform == "windows"),
        "native Windows feature",
    );
    check(
        enabled("CARGO_FEATURE_GPUI_NATIVE_VIDEO") == (platform != "macos"),
        "native presentation feature",
    );
    check(
        supplied_module == generated_module(&raw).as_bytes(),
        "generated module/JSON mismatch",
    );
    for path in [record, module] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    fs::write(output, supplied_module).expect("write validated compiled identity");
}
