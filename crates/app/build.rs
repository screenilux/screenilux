//! Gives the executable its icon and version details, so Explorer, the Start
//! menu and "Open with" show Screenilux rather than a blank program.

#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("screenilux.ico");
    std::fs::write(&ico, icon::ico()).expect("write the icon");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico.to_str().expect("a UTF-8 OUT_DIR"))
            .set("ProductName", "Screenilux")
            .set("FileDescription", "Screenilux screen recorder")
            .set("CompanyName", "EagleMind");
        if let Err(e) = res.compile() {
            // A missing resource compiler costs the icon, not the build.
            println!("cargo:warning=no icon embedded: {e}");
        }
    }
}
