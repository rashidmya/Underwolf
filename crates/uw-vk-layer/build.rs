fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    for name in ["quad.vert", "quad.frag", "tex.frag"] {
        let source = format!("shaders/{name}");
        println!("cargo:rerun-if-changed={source}");
        let result = std::process::Command::new("glslc")
            .arg("-O")
            .arg(&source)
            .arg("-o")
            .arg(out.join(format!("{name}.spv")))
            .status()
            .expect("glslc is required for native rendering");
        assert!(result.success(), "shader compilation failed: {name}");
    }
}
