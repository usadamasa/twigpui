{
  description = "twigpui development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      # gpui の x11 / wayland を落としているので macOS だけ。CI も macOS ランナー
      systems = [ "aarch64-darwin" ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
    in
    {
      devShells = forAllSystems (
        pkgs:
        let
          # upstream の配布物。llvm-tools-preview が rustc と同じ LLVM の llvm-cov を sysroot に置き、
          # cargo-llvm-cov はそこを自分で見つける
          rust = pkgs.rust-bin.stable.latest.default.override {
            extensions = [ "llvm-tools-preview" ];
          };
          # nixpkgs の check は nixpkgs の rustc で計測付き binary を走らせ、darwin では SIGSEGV で落ちる。
          # 計測に使うのは上の toolchain なので check を外す
          cargo-llvm-cov = pkgs.cargo-llvm-cov.overrideAttrs { doCheck = false; };
        in
        {
          default = pkgs.mkShellNoCC {
            packages = [
              rust
              cargo-llvm-cov
              pkgs.jq
            ];
            # gpui の build script の bindgen が dlopen する libclang。未指定だと clang-sys が
            # 独自の探索順で端末の libclang (Homebrew や CLT) を掴む
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            # リンクは Xcode CLT で行う。rust-overlay の toolchain が連れてくる nixpkgs の cc は apple-sdk 14.4 で
            # リンクし、AppKit は古い SDK のアプリを旧来の見た目と挙動で動かす。libiconv も /nix/store を指し、
            # store の GC で .app が起動しなくなる
            CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER = "/usr/bin/cc";
          };
        }
      );
    };
}
