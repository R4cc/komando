// Embed both app icons. Resource #1 is the default for Explorer and installed shortcuts;
// Settings can select resource #2 for the running window, taskbar and notification area.
fn main() {
    // Komando's UI runs on the main thread, which Windows gives 1 MiB of stack unless the
    // executable asks for more. Laying out a deep window can need more than that, and running
    // out ends the process without a panic or a line in the log. Zed asks for 8 MiB too.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
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
