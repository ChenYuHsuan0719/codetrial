//! Makes `rust-embed` see a changed asset tree and records the commit being
//! built.
//!
//! The derive expands to one `include_bytes!` per file, and rustc records
//! those in dep-info, so *editing* an embedded asset already rebuilds. Files
//! that appear or disappear do not: they were never in the previous expansion,
//! so nothing names them and Cargo considers the crate fresh.
//!
//! The failure that costs a release: build once, run `scripts/fetch-vendor.sh`
//! for the first time, build again. Nothing recompiles, and the binary ships
//! without the 37 MB of vendored runtime. It boots, serves the page, and then
//! fails in the browser with a Python runtime that will not start.
//!
//! Cargo walks a directory given to `rerun-if-changed` in full, so the one
//! line covers the tree.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=web");
    set_build_commit();
}

/// The released binary must identify the source it was built from even when
/// the rolling `latest` tag has since moved. CI supplies its checkout SHA;
/// local builds use HEAD, and source archives without either say `unknown`.
/// Both are seven characters wide, because `--short` alone follows
/// `core.abbrev`, and a setting below seven would fail `hex_sha`.
///
/// `CODETRIAL_RELEASE_COMMIT` is the same commit when CI supplied it, and empty
/// otherwise. Only a CI build can be the `latest` release, so the update notice
/// compares that one and nothing else: a local build's HEAD is usually a commit
/// the release will never name, and it would be offered an update forever.
fn set_build_commit() {
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    let root = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .filter(|root| root.join(".git").exists());
    if let Some(root) = root.as_deref() {
        watch_git_head(root);
    }

    let released = std::env::var("GITHUB_SHA")
        .ok()
        .and_then(|sha| hex_sha(&sha).map(|sha| sha[..7].to_ascii_lowercase()));
    let commit = released
        .clone()
        .or_else(|| {
            root.as_deref()
                .and_then(|root| git_output(root, &["rev-parse", "--short=7", "HEAD"]))
                .and_then(|sha| hex_sha(&sha).map(str::to_ascii_lowercase))
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CODETRIAL_BUILD_COMMIT={commit}");
    println!(
        "cargo:rustc-env=CODETRIAL_RELEASE_COMMIT={}",
        released.unwrap_or_default()
    );
}

fn hex_sha(sha: &str) -> Option<&str> {
    let sha = sha.trim();
    (sha.len() >= 7 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(sha)
}

fn git_output(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// HEAD holds the commit in a detached checkout and names a ref on a branch.
/// Watch that ref too, so the next local commit rebuilds the identifier even
/// when none of the source files changed.
fn watch_git_head(root: &std::path::Path) {
    let Some(head) = git_path(root, "HEAD") else {
        return;
    };
    println!("cargo:rerun-if-changed={}", head.display());
    if let Some(reference) = git_output(root, &["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git_path(root, reference.trim())
    {
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        } else {
            // A packed or unborn branch has no loose ref yet. A commit creates
            // one without changing packed-refs, so watch the nearest existing
            // parent as well as packed-refs.
            if let Some(parent) = path.ancestors().skip(1).find(|parent| parent.is_dir()) {
                println!("cargo:rerun-if-changed={}", parent.display());
            }
            if let Some(packed) = git_path(root, "packed-refs")
                && packed.is_file()
            {
                println!("cargo:rerun-if-changed={}", packed.display());
            }
        }
    }
}

/// Joined onto `root` rather than asked for as absolute. `--path-format` needs
/// git 2.31, and an older one, such as the git in the Bullseye image the Linux
/// release builds in, echoes the unknown flag back as output and still exits 0.
/// The answer is relative to the working directory, which is `root`, or
/// already absolute, which `join` keeps as it is.
fn git_path(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    git_output(root, &["rev-parse", "--git-path", name]).map(|path| root.join(path.trim()))
}
