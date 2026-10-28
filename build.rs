fn main() {
    println!("cargo:rerun-if-changed=assets/cat-icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/cat-icon.ico")
            .compile()
            .expect("Could not embed the Windows icon");
    }
}
