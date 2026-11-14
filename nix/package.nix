{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  git,
  cctools ? null,
  xcbuild ? null,
}:

let
  manifest = lib.importTOML ../Cargo.toml;
  darwinToolchain = lib.optionals stdenv.hostPlatform.isDarwin [
    cctools
    xcbuild
  ];
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
  ] ++ darwinToolchain;

  # Rust tests are covered by the normal CI workflow. The Nix check is
  # intentionally build-only so it validates packaging inputs without
  # duplicating the full Rust test suite.
  doCheck = false;

  meta = {
    description = "Terminal workspace manager for AI coding agents";
    homepage = "https://herdr.dev";
    license = lib.licenses.asl20;
    mainProgram = "herdr";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
