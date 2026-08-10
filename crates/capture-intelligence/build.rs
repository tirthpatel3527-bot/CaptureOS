fn main() {
    println!("cargo:rerun-if-changed=native/macos_face.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    cc::Build::new()
        .file("native/macos_face.m")
        .flag("-fobjc-arc")
        .compile("captureos_macos_face");
    for framework in ["Foundation", "Vision", "CoreGraphics", "ImageIO"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
