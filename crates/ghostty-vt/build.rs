use std::env;
use std::path::PathBuf;

fn c_translation_unit(target_os: &str, target_arch: &str) -> &'static str {
    match (target_os, target_arch) {
        ("linux", "x86_64") => "ghostty_vt_x86_64_linux.c",
        ("linux", "aarch64") => "ghostty_vt_aarch64_linux.c",
        ("macos", "x86_64") => "ghostty_vt_x86_64_macos.c",
        ("macos", "aarch64") => "ghostty_vt_aarch64_macos.c",
        ("linux" | "macos", other) => panic!(
            "unsupported {target_os} architecture for pre-transpiled libghostty-vt-c: {other} \
             (expected x86_64 or aarch64)"
        ),
        (other, _) => panic!(
            "unsupported target_os for pre-transpiled libghostty-vt-c: {other} \
             (expected linux or macos)"
        ),
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let c_dir = manifest_dir.join("../../vendor/libghostty-vt-c");

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH");
    let c_unit = c_dir.join(c_translation_unit(&target_os, &target_arch));
    let rt_shims = c_dir.join("zig_rt_shims.c");
    let zig_header = c_dir.join("zig.h");

    println!("cargo:rerun-if-changed=build.rs");
    for path in [
        &c_unit,
        &rt_shims,
        &zig_header,
        &c_dir.join("provenance.json"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut build = cc::Build::new();
    build
        .file(&c_unit)
        .file(&rt_shims)
        .include(&c_dir)
        .opt_level(3)
        .debug(false)
        .warnings(false)
        .flag("-std=c99")
        .flag("-fno-strict-aliasing")
        .flag("-fwrapv")
        .flag_if_supported("-Wno-incompatible-pointer-types")
        .compile("ghostty-vt");

    if target_os != "macos" {
        println!("cargo:rustc-link-lib=m");
    }
}
