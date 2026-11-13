fn main() {
    println!("cargo:rerun-if-changed=assets/corewatch.ico");
    // Resources are embedded on the user's Windows build. Cross-platform checks
    // do not require a Windows resource compiler.
    if cfg!(windows) && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/corewatch.ico");
        resource.set("ProductName", "CoreWatch");
        resource.set("FileDescription", "CoreWatch hardware control");
        resource.compile().expect("Could not embed CoreWatch icon. Check Windows C++ Build Tools installation.");
    }
}
