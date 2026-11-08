//! `cargo build-all`: release builds of Tasker for Linux and Windows, copied to `binary/`.
//!
//! The OS this runs on is built natively (into target/release, shared with a plain
//! `cargo build --release`); the other one is cross-compiled:
//!
//! * on Linux, `tasker.exe` with MinGW (`sudo apt install gcc-mingw-w64-x86-64`)
//! * on Windows, the Linux binary with zig (`winget install zig.zig`, then
//!   `cargo install --locked cargo-zigbuild`), for glibc 2.34 like a Linux build
//!
//! Missing Rust targets are added with rustup. Extra arguments go to both cargo builds
//! (e.g. `cargo build-all --locked`). A plain `cargo` alias can't do this: cargo has no
//! post-build step and doesn't allow aliases named like its own commands.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

struct Build {
    /// For messages.
    os: &'static str,
    exe: &'static str,
    /// `None`: the OS this runs on.
    cross: Option<Cross>,
}

struct Cross {
    target: &'static str,
    /// Cargo's arguments for it, before `--release`.
    cargo: &'static [&'static str],
    /// A command that only works when the cross toolchain is installed…
    probe: &'static [&'static str],
    /// …and how to install it.
    install: &'static str,
}

#[cfg(target_os = "linux")]
const BUILDS: &[Build] = &[
    Build { os: "Linux", exe: "tasker", cross: None },
    Build {
        os: "Windows",
        exe: "tasker.exe",
        cross: Some(Cross {
            target: "x86_64-pc-windows-gnu",
            cargo: &["build", "--target", "x86_64-pc-windows-gnu"],
            probe: &["x86_64-w64-mingw32-gcc", "--version"],
            install: "sudo apt install gcc-mingw-w64-x86-64   (Fedora: sudo dnf install mingw64-gcc)",
        }),
    },
];

#[cfg(windows)]
const BUILDS: &[Build] = &[
    Build { os: "Windows", exe: "tasker.exe", cross: None },
    Build {
        os: "Linux",
        exe: "tasker",
        cross: Some(Cross {
            target: "x86_64-unknown-linux-gnu",
            // The suffix is the oldest glibc to support; the output still lands in
            // target/x86_64-unknown-linux-gnu.
            cargo: &["zigbuild", "--target", "x86_64-unknown-linux-gnu.2.34"],
            probe: &["cargo-zigbuild", "--help"],
            install: "winget install zig.zig   and then   cargo install --locked cargo-zigbuild",
        }),
    },
];

#[cfg(not(any(target_os = "linux", windows)))]
const BUILDS: &[Build] = &[];

fn main() -> ExitCode {
    if BUILDS.is_empty() {
        eprintln!("cargo build-all runs on Linux and Windows only");
        return ExitCode::FAILURE;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let target_dir = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let out = root.join("binary");
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("can't create {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    let extra: Vec<String> = std::env::args().skip(1).collect();

    let mut done = Vec::new();
    let mut failed = Vec::new();
    for b in BUILDS {
        let dest = out.join(b.exe);
        // A failed build must not leave last time's binary behind looking current.
        let _ = std::fs::remove_file(&dest);
        eprintln!("\n=== {} ===", b.os);
        let built = build(&root, b, &extra).and_then(|()| {
            let dir = match &b.cross {
                Some(c) => target_dir.join(c.target),
                None => target_dir.clone(),
            };
            let src = dir.join("release").join(b.exe);
            std::fs::copy(&src, &dest).map_err(|e| format!("can't copy {}: {e}", src.display()))
        });
        match built {
            Ok(bytes) => done.push(format!("binary/{}  ({:.1} MB, {})", b.exe, bytes as f64 / 1e6, b.os)),
            Err(e) => failed.push(format!("{} build failed: {e}", b.os)),
        }
    }

    eprintln!();
    for d in &done {
        eprintln!("{d}");
    }
    for f in &failed {
        eprintln!("error: {f}");
    }
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn build(root: &Path, b: &Build, extra: &[String]) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    match &b.cross {
        None => {
            cmd.arg("build");
        }
        Some(c) => {
            let probe = Command::new(c.probe[0]).args(&c.probe[1..]).output();
            if !probe.is_ok_and(|o| o.status.success()) {
                return Err(format!("the cross compiler is missing. Install it with:\n    {}", c.install));
            }
            add_rust_target(c.target)?;
            cmd.args(c.cargo);
        }
    }
    cmd.args(["--release", "--package", "tasker"]).args(extra).current_dir(root);
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("cargo exited with {s}")),
        Err(e) => Err(format!("can't run cargo: {e}")),
    }
}

/// `rustup target add` unless it is installed. Without rustup, cargo reports it.
fn add_rust_target(target: &str) -> Result<(), String> {
    let Ok(list) = Command::new("rustup").args(["target", "list", "--installed"]).output() else {
        return Ok(());
    };
    if String::from_utf8_lossy(&list.stdout).lines().any(|l| l.trim() == target) {
        return Ok(());
    }
    eprintln!("Adding the Rust target {target} …");
    match Command::new("rustup").args(["target", "add", target]).status() {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!("`rustup target add {target}` failed")),
    }
}
