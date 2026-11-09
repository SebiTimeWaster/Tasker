//! Windows: puts the icon and the version info into tasker.exe (what Explorer and the Task
//! Manager show). Nothing to do for other systems.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/tasker.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The version comes from Cargo.toml; the names would otherwise be the package's ("tasker").
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/tasker.ico")
        .set("ProductName", "Tasker")
        .set("FileDescription", "Tasker")
        .set("OriginalFilename", "tasker.exe");
    // Not fatal: the window and tray icons are set at runtime anyway, only the .exe file
    // itself then has a generic icon.
    if let Err(e) = res.compile() {
        println!(
            "cargo:warning=no icon/version info in tasker.exe; is windres (MinGW) or rc.exe (Windows SDK) installed? ({e})"
        );
    }
}
