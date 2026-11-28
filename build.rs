use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
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
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo build output directory"));
    let (major, minor, patch, version) = (
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH"),
        env!("CARGO_PKG_VERSION"),
    );
    fs::write(
        out_dir.join("sumiVersion.h"),
        format!(
            "#define SUMIPDF_VERSION {major},{minor},{patch},0\n#define SUMIPDF_VERSION_TEXT \"{version}\\0\"\n"
        ),
    )
    .expect("write Windows resource version");
    embed_resource::compile_for(
        "assets/app.rc",
        ["SumiPDF"],
        embed_resource::ParamsIncludeDirs([out_dir.as_os_str()]),
    )
    .manifest_required()
    .expect("compile Windows resources");
}
