use std::env;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/SumiPDF.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // app.rc already supplies the executable's manifest.
        println!("cargo:rustc-link-arg-bin=SumiPDF=/MANIFEST:NO");
    }
    embed_resource::compile_for("assets/app.rc", ["SumiPDF"], embed_resource::NONE)
        .manifest_required()
        .expect("compile Windows icon and manifest");
}
