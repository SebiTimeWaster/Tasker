//! Linux: pins a few libm symbols to old glibc versions so release binaries built on a
//! new distro still start on older ones (see build/glibc_compat.c).
//! Windows: embeds the app icon (drawn by src/icon.rs) and version info into the .exe.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;

const PINNED: &[&str] = &["acosf", "asinf", "atan2f", "acoshf", "coshf", "sinhf", "hypot", "hypotf"];

fn main() {
    println!("cargo:rerun-if-changed=build/glibc_compat.c");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=RC");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target == "x86_64-unknown-linux-gnu" {
        cc::Build::new().file("build/glibc_compat.c").compile("glibc_compat");
        for f in PINNED {
            println!("cargo:rustc-link-arg=-Wl,--wrap={f}");
        }
    } else if target.contains("windows") {
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
        if let Err(e) = windows_resources(&target, &out) {
            // Not fatal: the window and tray icons are set at runtime anyway.
            println!("cargo:warning=no icon/version info in tasker.exe: {e}");
        }
    }
}

fn windows_resources(target: &str, out: &Path) -> Result<(), String> {
    let ico = out.join("tasker.ico");
    std::fs::write(&ico, ico_file(&[16, 20, 24, 32, 40, 48, 64, 256])).map_err(|e| e.to_string())?;
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut v: Vec<&str> = version.split(['.', '-']).take(3).collect();
    v.resize(3, "0");
    let numeric = format!("{},{},{},0", v[0], v[1], v[2]);
    let rc = out.join("tasker.rc");
    std::fs::write(
        &rc,
        format!(
            r#"1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Tasker"
      VALUE "ProductName", "Tasker"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "tasker.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            ico = ico.display().to_string().replace('\\', "/"),
        ),
    )
    .map_err(|e| e.to_string())?;

    let compiled = if target.ends_with("-msvc") {
        let res = out.join("tasker.res");
        run_first(&rc_tools(), |c| c.arg("/nologo").arg("/fo").arg(&res).arg(&rc))?;
        res
    } else {
        // MinGW: the cross tool (x86_64-w64-mingw32-windres) or plain `windres` (MSYS2).
        let obj = out.join("tasker_res.o");
        let arch = target.split('-').next().unwrap_or("x86_64");
        let tools: Vec<PathBuf> = match std::env::var_os("WINDRES") {
            Some(t) => vec![t.into()],
            None => vec![format!("{arch}-w64-mingw32-windres").into(), "windres".into()],
        };
        run_first(&tools, |c| c.arg("-i").arg(&rc).arg("-o").arg(&obj))?;
        obj
    };
    println!("cargo:rustc-link-arg-bins={}", compiled.display());
    Ok(())
}

/// `RC` from the environment, else rc.exe on the PATH (Developer Command Prompt), else the
/// newest one in the Windows SDK (installed along with the MSVC build tools).
fn rc_tools() -> Vec<PathBuf> {
    if let Some(rc) = std::env::var_os("RC") {
        return vec![rc.into()];
    }
    let arch = match std::env::var("HOST").unwrap_or_default().split('-').next() {
        Some("aarch64") => "arm64",
        Some("i686" | "i586") => "x86",
        _ => "x64",
    };
    let mut tools = vec![PathBuf::from("rc.exe")];
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Some(base) = std::env::var_os(var) else { continue };
        let Ok(dirs) = std::fs::read_dir(Path::new(&base).join("Windows Kits").join("10").join("bin")) else { continue };
        // Version directories like 10.0.22621.0, newest first.
        let mut versions: Vec<(Vec<u32>, PathBuf)> = dirs
            .flatten()
            .filter_map(|d| {
                let name = d.file_name().into_string().ok()?;
                let v = name.split('.').map(|n| n.parse().ok()).collect::<Option<Vec<u32>>>()?;
                Some((v, d.path().join(arch).join("rc.exe")))
            })
            .filter(|(_, p)| p.is_file())
            .collect();
        versions.sort();
        tools.extend(versions.into_iter().rev().map(|(_, p)| p));
    }
    tools
}

/// Run the first of `tools` that works; the error is the last one's.
fn run_first(tools: &[PathBuf], args: impl Fn(&mut Command) -> &mut Command) -> Result<(), String> {
    let mut err = "no tool to run".to_string();
    for t in tools {
        let name = t.display();
        match args(&mut Command::new(t)).output() {
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => err = format!("{name} failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => err = format!("{name} not found ({e})"),
        }
    }
    Err(err)
}

/// A .ico with one 32-bit BMP image per size.
fn ico_file(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| {
            let rgba = icon::render(s, icon::IconState::Idle);
            let mut b = Vec::new();
            // BITMAPINFOHEADER; the height counts the color and the (unused) AND mask.
            for v in [40u32, s, s * 2] {
                b.extend(v.to_le_bytes());
            }
            b.extend(1u16.to_le_bytes()); // planes
            b.extend(32u16.to_le_bytes()); // bits per pixel
            b.extend([0u8; 24]); // no compression, sizes / resolutions / palette unused
            // BGRA, bottom-up rows
            for y in (0..s).rev() {
                for x in 0..s {
                    let p = ((y * s + x) * 4) as usize;
                    b.extend([rgba[p + 2], rgba[p + 1], rgba[p], rgba[p + 3]]);
                }
            }
            // AND mask: 1 bit per pixel, rows padded to 4 bytes; all zero (alpha decides)
            b.extend(vec![0u8; (s.div_ceil(32) * 4 * s) as usize]);
            b
        })
        .collect();
    let mut ico = Vec::new();
    ico.extend([0u8, 0, 1, 0]); // reserved, type 1 = icon
    ico.extend((sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let dim = if s >= 256 { 0 } else { s as u8 }; // 0 means 256
        ico.extend([dim, dim, 0, 0]);
        ico.extend(1u16.to_le_bytes()); // planes
        ico.extend(32u16.to_le_bytes()); // bits per pixel
        ico.extend((img.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        ico.extend(img);
    }
    ico
}
