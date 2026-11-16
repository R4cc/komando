// Embed both app icons. Resource #1 is the default for Explorer and installed shortcuts;
// Settings can select resource #2 for the running window, taskbar and notification area.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/icon-light.ico");
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.set_icon_with_id("assets/icon-light.ico", "2");
            res.set("ProductName", "Komando");
            res.set("FileDescription", "Komando");
            res.compile().expect("embedding Windows resources");
        }
    }
}
