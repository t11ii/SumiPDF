use std::{env, fs, path::PathBuf};

fn main() {
    let windows = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    println!("cargo:rustc-check-cfg=cfg(mupdf_backend)");
    println!("cargo:rerun-if-env-changed=SUMIPDF_MUPDF_DIR");
    println!("cargo:rerun-if-changed=src/mupdf_bridge.c");
    if let Some(dir) = env::var_os("SUMIPDF_MUPDF_DIR") {
        let dir = PathBuf::from(dir);
        let lib = if msvc {
            dir.join("platform/win32/x64/Release")
        } else {
            dir.join("build/release")
        };
        let libraries = if msvc {
            ["libmupdf", "libthirdparty", "libresources"].as_slice()
        } else {
            ["mupdf", "mupdf-third"].as_slice()
        };
        let header = dir.join("include/mupdf/fitz/version.h");
        println!("cargo:rerun-if-changed={}", header.display());
        println!("cargo:rustc-cfg=mupdf_backend");
        cc::Build::new()
            .include(dir.join("include"))
            .file("src/mupdf_bridge.c")
            .compile("sumi_mupdf");
        println!("cargo:rustc-link-search=native={}", lib.display());
        for name in libraries {
            let file = lib.join(if msvc {
                format!("{name}.lib")
            } else {
                format!("lib{name}.a")
            });
            assert!(
                file.is_file(),
                "MuPDF library missing at {}",
                file.display()
            );
            println!("cargo:rerun-if-changed={}", file.display());
            println!("cargo:rustc-link-lib=static={name}");
        }
        if msvc {
            println!("cargo:rustc-link-lib=advapi32");
        } else if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
            println!("cargo:rustc-link-lib=c++");
        } else {
            println!("cargo:rustc-link-lib=stdc++");
        }
    } else if windows {
        panic!("Set SUMIPDF_MUPDF_DIR to the prepared MuPDF directory. See README.md.");
    }
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/SumiPDF.ico");
    if !windows {
        return;
    }
    if msvc {
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
