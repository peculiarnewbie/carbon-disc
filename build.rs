fn main() {
    // Embed the brake-disc icon into the Windows executable so Explorer and the
    // taskbar show it; other platforms set the icon at runtime via the viewport.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/logo.ico")
            .compile()
            .expect("embed the Windows icon resource");
    }
    println!("cargo:rerun-if-changed=assets/logo.ico");
}
