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

        # Build with the same toolchain the dev shell pins, not the one
        # nixpkgs ships. `buildRustPackage` defaults to the latter, which
        # would mean `nix flake check` and `cargo test` in the shell could
        # disagree about the compiler — the exact failure the comment on
        # `rustToolchain` below was written about.
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # `target/` is 850 MB of build output and nix would copy all of
        # it into the store before doing anything else.
        src = pkgs.lib.cleanSourceWith {
          name = "templedb-rust-src";
          src = ./.;
          filter = path: type:
            !(type == "directory" && baseNameOf path == "target");
        };

        templedb-rs = rustPlatform.buildRustPackage {
          pname = "templedb-rs";
          version = "0.1.0";
          inherit src;
          cargoLock.lockFile = ./Cargo.lock;

          # The difflib oracle shells out to python3. It is the reason
          # this is a `nativeCheckInput` and not a build input.
          nativeCheckInputs = [ pkgs.python3 ];

          # Three invocations, because the oracle tests are
          # `#[ignore]`d — `cargo test` must pass on a machine with no
          # interpreter, so they do not run by default and a check that
          # only ran the default set would skip the five tests that pin
          # the SequenceMatcher port and the clock against the real
          # thing.
          #
          # They run from the crate directory because
          # `real_source_files_diff_identically` reads `src/difflib.rs`
          # and friends by relative path, and asserts it found some.
          checkPhase = ''
            runHook preCheck
            cargo test --workspace
            ( cd crates/templedb-db \
                && cargo test --test difflib_parity -- --ignored \
                && cargo test --test clock_parity -- --ignored )
            runHook postCheck
          '';

          meta = {
            description = "A Rust port of TempleDB (read-only subset)";
            mainProgram = "templedb-rs";
          };
        };
      in
      {
        packages.default = templedb-rs;
        packages.templedb-rs = templedb-rs;

        # `nix flake check` runs everything that can run hermetically:
        # the unit tests and the CPython difflib oracle, via the
        # `checkPhase` above.
        #
        # It deliberately does NOT run `scripts/parity-sweep.sh`. The
        # sweep diffs this binary against the Python `templedb` over the
        # live SQLite database, so it needs an interpreter, an installed
        # `templedb`, and ~340 MB of real data that no sandbox has. Use
        # `just sweep` (or `just check` for both halves); see the Checks
        # section of README.md for why that split is where it is.
        checks.default = templedb-rs;
        checks.templedb-rs = templedb-rs;

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

            # The Postgres *server*, not just the client, and pinned by
            # this flake rather than taken from the host.
            #
            # It lives here rather than in the system_config project
            # because the port is a separate project that owns its own
            # database (see the write-path design report). Three reasons
            # that matters: the version is pinned the same way the
            # toolchain is, so a parity run is reproducible; the write
            # scratch database is *meant* to be disposable and a system
            # service would make it persistent and precious; and
            # changing a test database should not require
            # `nixos system-switch`, which is a path that currently
            # fails on a read-only flake.lock.
            #
            # The connection strings are deliberately NOT here. They are
            # project-scoped TempleDB variables on `templedb-rust`, so
            # they are discoverable with `templedb var list templedb-rust`
            # and exportable through direnv. `conn.rs` already reads
            # `TEMPLEDB_URL` from the environment, so nothing in the
            # crate needs to know where it came from.
            postgresql

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
