use sha2::{Digest, Sha256};
use std::{env, fmt::Write as _, fs, io::Read as _, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=BKPR_GHOSTTY_VT_DIR");
    let dir = PathBuf::from(env::var_os("BKPR_GHOSTTY_VT_DIR").expect("Run scripts/prepare-ghostty-vt.sh and set BKPR_GHOSTTY_VT_DIR; arbitrary system Ghostty is not accepted"));
    let revision =
        fs::read_to_string(dir.join("bkpr-source-revision")).expect("missing prepared source pin");
    assert_eq!(
        revision.trim(),
        "93967b7f9478e94dcbb42125c8a90f34feb40d75",
        "wrong Ghostty revision"
    );
    let source_sha =
        fs::read_to_string(dir.join("bkpr-source-sha256")).expect("missing source archive hash");
    assert_eq!(
        source_sha.trim(),
        "af551aa496cf54ff734e11563d45fe2c49739ee754d1740d1ac984a67a42dc70"
    );
    let profile_path = dir.join("bkpr-build-profile");
    println!("cargo:rerun-if-changed={}", profile_path.display());
    let profile = fs::read_to_string(profile_path)
        .expect("native prerequisite profile missing; rerun scripts/prepare-ghostty-vt.sh");
    assert_eq!(
        profile.trim(),
        "ReleaseSafe-simd-libc-baseline-offer-v1",
        "invalid native prerequisite: require libc and validated paste-offer correlation"
    );
    let patch_path = dir.join("bkpr-source-patch-sha256");
    println!("cargo:rerun-if-changed={}", patch_path.display());
    let patch_sha =
        fs::read_to_string(patch_path).expect("missing native paste-offer patch provenance");
    assert_eq!(
        patch_sha.trim(),
        "196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c",
        "wrong native paste-offer patch"
    );
    let manifest = dir.join("bkpr-files.sha256");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let files = fs::read_to_string(&manifest).expect("missing prepared files hash manifest");
    assert!(files.contains("  lib/libghostty-vt.a"));
    assert!(files.contains("  include/ghostty/vt.h"));
    let mut buffer = [0u8; 16 * 1024];
    for line in files.lines() {
        let (expected, name) = line.split_once("  ").expect("invalid file hash manifest");
        assert!(!name.starts_with('/') && !name.split('/').any(|s| s == ".."));
        let path = dir.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        let mut file = fs::File::open(&path).expect("missing prepared native file");
        let mut hasher = Sha256::new();
        loop {
            let count = file
                .read(&mut buffer)
                .expect("reading prepared native file");
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let mut actual = String::with_capacity(64);
        for byte in hasher.finalize() {
            write!(actual, "{byte:02x}").unwrap();
        }
        assert_eq!(
            expected,
            actual,
            "native prerequisite changed: {}",
            path.display()
        );
    }
    println!("cargo:rerun-if-changed=src/shim.c");
    println!("cargo:rerun-if-changed=src/shim.h");
    cc::Build::new()
        .file("src/shim.c")
        .include(dir.join("include"))
        .compile("bkpr_terminal_shim");
    let bindings = bindgen::Builder::default()
        .header("src/shim.h")
        .clang_arg(format!("-I{}", dir.join("include").display()))
        .allowlist_function("ghostty_.*")
        .allowlist_function("bkpr_terminal_.*")
        .allowlist_type("Ghostty.*")
        .allowlist_var("GHOSTTY_.*")
        .prepend_enum_name(false)
        .derive_default(true)
        .generate_comments(false)
        .layout_tests(false)
        .generate()
        .expect("official pinned C header binding generation failed");
    bindings
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("ghostty.rs"))
        .unwrap();
    println!(
        "cargo:rustc-link-search=native={}",
        dir.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=ghostty-vt");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=dl");
    }
}
