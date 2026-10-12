{
  description = "TempleDB Rust - A Rust port of TempleDB";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          name = "templedb-rust-dev";

          packages = with pkgs; [
            # `rustToolchain` already provides cargo, rustc and (via the
            # extension above) rust-analyzer. Listing pkgs.cargo /
            # pkgs.rustc / pkgs.rust-analyzer alongside it put a second,
            # *different* toolchain on PATH — `which -a cargo` returned
            # 1.99.0 from the overlay and 1.98.1 from nixpkgs. The
            # overlay won only because mkShell resolves collisions by
            # list order, so the pin was decided by where a line sat in
            # this file. Reordering the list, or nixpkgs moving, would
            # have silently changed the compiler.
            rustToolchain

            # rusqlite uses the `bundled` feature, so SQLite is not a
            # build input. `sqlite` is here for the CLI: checking a
            # query against the live database by hand is how most of
            # the parity work gets done.
            sqlite

            # The reference oracle, not a build input either. The
            # implementation this port is diffed against is written in
            # Python, and `crates/templedb-db/tests/difflib_parity.rs`
            # drives `difflib` directly to check the SequenceMatcher
            # port against the real thing. Plain `python3` with no
            # packages: difflib is in the standard library, and the
            # version matters less than having one at all — difflib's
            # algorithm has been stable for many releases.
            python3

            git
            just
          ];

          shellHook = ''
            export RUST_BACKTRACE=1
            echo "TempleDB Rust dev shell loaded"
            echo "Rust version: $(rustc --version)"
            echo "Cargo version: $(cargo --version)"
          '';

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      });
}
