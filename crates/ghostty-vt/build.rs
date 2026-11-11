use std::env;
use std::path::PathBuf;

fn linux_c_translation_unit(target_arch: &str) -> &'static str {
    match target_arch {
        "x86_64" => "ghostty_vt_x86_64_linux.c",
        "aarch64" => "ghostty_vt_aarch64_linux.c",
        other => panic!(
            "unsupported Linux architecture for pre-transpiled libghostty-vt-c: {other} \
             (expected x86_64 or aarch64)"
        ),
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let c_dir = manifest_dir.join("../../vendor/libghostty-vt-c");

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS");
    assert_eq!(
        target_os, "linux",
        "this fork builds pre-transpiled libghostty-vt-c for Linux targets only (got target_os={target_os})"
    );

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH");
    let c_unit = c_dir.join(linux_c_translation_unit(&target_arch));
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
        .compile("ghostty-vt");

    println!("cargo:rustc-link-lib=m");
}
