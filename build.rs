use std::{env, path::Path, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/app.rc");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let root = env::var_os("CARGO_MANIFEST_DIR").unwrap();
        let manifest = Path::new(&root).join("assets/app.manifest");
        println!("cargo:rustc-link-arg-bin=SumiPDF=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=SumiPDF=/MANIFESTINPUT:{}",
            manifest.display()
        );
        return;
    }

    // GNU ld accepts a COFF resource object directly. MinGW packages provide
    // the prefixed utility when cross-compiling, or bare windres on Windows.
    let out = env::var_os("OUT_DIR").unwrap();
    let resource = Path::new(&out).join("app.o");
    let compiler = if env::var("HOST").unwrap().contains("windows") {
        "windres"
    } else {
        "x86_64-w64-mingw32-windres"
    };
    let status = Command::new(compiler)
        .args(["-i", "assets/app.rc", "-O", "coff", "-o"])
        .arg(&resource)
        .status()
        .expect("run MinGW windres");
    assert!(status.success(), "compile Windows application manifest");
    println!("cargo:rustc-link-arg-bin=SumiPDF={}", resource.display());
}
