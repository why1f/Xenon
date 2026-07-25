use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

const SUPPORTED_VERSION: &str = "26.6.27";

fn main() {
    for name in [
        "XRAY_BINARY_PATH",
        "XRAY_BINARY_VERSION",
        "XRAY_BINARY_SHA256",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let embedded_path = out_dir.join("xray-core");
    let target = env::var("TARGET").expect("TARGET is set by Cargo");
    let profile = env::var("PROFILE").expect("PROFILE is set by Cargo");
    let asset_path = env::var_os("XRAY_BINARY_PATH").map(PathBuf::from);

    match asset_path {
        Some(path) => embed_asset(&path, &embedded_path),
        None if target.contains("linux") && profile == "release" => panic!(
            "Linux release builds require XRAY_BINARY_PATH, XRAY_BINARY_VERSION={SUPPORTED_VERSION}, and XRAY_BINARY_SHA256"
        ),
        None => {
            fs::write(&embedded_path, []).expect("write empty development Xray asset");
            println!("cargo:rustc-env=XRAY_EMBEDDED_AVAILABLE=0");
            println!("cargo:rustc-env=XRAY_EMBEDDED_SHA256=");
        }
    }

    println!(
        "cargo:rustc-env=XRAY_EMBEDDED_PATH={}",
        embedded_path.display()
    );
    println!("cargo:rustc-env=XRAY_EMBEDDED_VERSION={SUPPORTED_VERSION}");
}

fn embed_asset(source: &PathBuf, destination: &PathBuf) {
    let version = env::var("XRAY_BINARY_VERSION")
        .expect("XRAY_BINARY_VERSION is required when XRAY_BINARY_PATH is set");
    assert_eq!(
        version.trim_start_matches('v'),
        SUPPORTED_VERSION,
        "only Xray-core v{SUPPORTED_VERSION} is supported"
    );
    let expected_hash = env::var("XRAY_BINARY_SHA256")
        .expect("XRAY_BINARY_SHA256 is required when XRAY_BINARY_PATH is set")
        .to_ascii_lowercase();
    let bytes = fs::read(source).expect("read XRAY_BINARY_PATH");
    assert!(!bytes.is_empty(), "Xray binary must not be empty");
    let actual_hash = to_hex(&Sha256::digest(&bytes));
    assert_eq!(actual_hash, expected_hash, "Xray binary SHA-256 mismatch");
    fs::write(destination, bytes).expect("copy Xray binary into OUT_DIR");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rustc-env=XRAY_EMBEDDED_AVAILABLE=1");
    println!("cargo:rustc-env=XRAY_EMBEDDED_SHA256={actual_hash}");
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            write!(&mut out, "{byte:02x}").expect("writing to String cannot fail");
            out
        })
}
