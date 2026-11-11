{
  lib,
  rustPlatform,
  pkg-config,
  git,
}:

let
  manifest = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "herdr";
  version = manifest.package.version;

  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.intersection (lib.fileset.fromSource (lib.sources.cleanSource ./..)) (
      lib.fileset.unions [
        ../assets
        ../crates
        ../distribution/install.ps1
        ../docs/next/api/herdr-api.schema.json
        ../src
        ../vendor/libghostty-vt-c
        ../vendor/libghostty-vt.vendor.json
        ../vendor/portable-pty
        ../build.rs
        ../Cargo.lock
        ../Cargo.toml
        ../skills/herdr/SKILL.md
      ]
    );
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = [
    git
    pkg-config
  ];

  # Rust tests are covered by the normal CI workflow. The Nix check is
  # intentionally build-only so it validates packaging inputs without
  # duplicating the full Rust test suite.
  doCheck = false;

  meta = {
    description = "Terminal workspace manager for AI coding agents";
    homepage = "https://herdr.dev";
    license = lib.licenses.asl20;
    mainProgram = "herdr";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
  };
}
