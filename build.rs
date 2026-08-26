//! ビルド時に走らせている commit を binary へ埋める (#231)｡
//!
//! 起動ログに版が入っていなかったので､ログだけを見てもどのビルドの話なのか
//! 分からなかった｡`.app` は `scripts/build-app-bundle.sh` で組み直され､
//! 手元には worktree が何本も並ぶ｡`0.1.0` は当分動かないから､区別が付くのは
//! commit だけになる｡
//!
//! `git` が無い・`.git` が無い場合は `unknown` にしてビルドは通す｡
//! 版が読めないことは起動できない理由にならない｡ただし env var 自体は必ず
//! 出すので､`main.rs` 側は `option_env!` ではなく `env!` で受けられる —
//! 埋め忘れがコンパイルエラーになる｡

use std::path::Path;
use std::process::Command;

fn main() {
    let hash = git_hash().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=TWIGPUI_GIT_HASH={hash}");
}

/// 今の commit の短縮 hash｡追跡中のファイルに未コミットの差分があれば
/// `abc1234-dirty` になる｡git が使えなければ `None`｡
fn git_hash() -> Option<String> {
    let mut hash = non_empty(&["rev-parse", "--short", "HEAD"])?;
    watch_inputs();
    // 空の出力は clean を意味するので､ここでは `non_empty` を通さない｡
    if !run(&["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        // 追跡外のファイルでは印を付けない｡`target/` や手元のメモが
        // あるだけで dirty になると､印が何も言わなくなる｡
        hash.push_str("-dirty");
    }
    Some(hash)
}

/// commit を進めたら､また source を触ったら再ビルドされるよう見張る｡
///
/// `rerun-if-changed` を 1 つでも出すと､cargo の既定 (パッケージ内の
/// どのファイルが変わっても走らせ直す) が **置き換わる**｡だから HEAD だけを
/// 挙げると source を触っても build script が走らず､`-dirty` が付かないまま
/// 古い hash が残る｡clean だと嘘をつく向きなので､source の側も挙げ直す｡
/// git を数回起動する分の費用は､これが相乗りしているコンパイルに比べれば
/// 無いに等しい｡
///
/// worktree では `.git` はディレクトリではなくファイルなので､`.git/HEAD` を
/// 直に指定すると存在しないパスになる — cargo はそれを「毎回走らせろ」と
/// 読むので､黙って毎ビルド走ることになる｡実際のパスは git に訊く｡
/// HEAD 自体は worktree ごとの git dir にあり､ref は共有の git dir にある｡
fn watch_inputs() {
    // ディレクトリは再帰的に見られる｡binary へ入るのはこの 2 つだけだ
    // (`assets` は `src/assets.rs` が埋め込む)｡
    watch(Path::new("src"));
    watch(Path::new("assets"));

    let Some(git_dir) = non_empty(&["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    watch(&Path::new(&git_dir).join("HEAD"));

    let Some(common_dir) = non_empty(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
    else {
        return;
    };
    let common_dir = Path::new(&common_dir);
    // detached HEAD なら ref は無い｡そのときは HEAD だけで足りる｡
    if let Some(reference) = non_empty(&["symbolic-ref", "-q", "HEAD"]) {
        let path = common_dir.join(&reference);
        if path.exists() {
            watch(&path);
            return;
        }
    }
    // ref が packed されていると loose なファイルは無い｡
    let packed = common_dir.join("packed-refs");
    if packed.exists() {
        watch(&packed);
    }
}

/// `path` が変わったら build script を走らせ直させる｡
fn watch(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

/// `git` を引数付きで走らせ､trim した stdout を返す｡起動できないか
/// 非ゼロで終われば `None`｡
fn run(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// [`run`] のうち､空の出力を答えとして扱えない呼び出し向け｡
fn non_empty(args: &[&str]) -> Option<String> {
    run(args).filter(|text| !text.is_empty())
}
