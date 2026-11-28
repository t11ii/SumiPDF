#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
mod appearance;
#[cfg(any(windows, test))]
mod cli;
#[cfg(any(windows, test))]
mod continuous;
#[cfg(any(windows, test))]
mod highlight;
#[cfg(test)]
mod highlight_roundtrip;
#[cfg(any(windows, test))]
mod pdfseal;
#[cfg(any(windows, test))]
mod preferences;
#[cfg(any(windows, test))]
mod raster_size;
#[cfg(any(windows, test))]
mod textselect;
#[cfg(windows)]
mod viewer;
#[cfg(windows)]
mod wincrypto;
#[cfg(windows)]
mod winpdf;

#[cfg(windows)]
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if cli::is_command(args.first()) {
        cli::attach_console();
        if let Err(error) = cli::run(args) {
            eprintln!("{}", serde_json::json!({"error": error}));
            std::process::exit(1);
        }
    } else {
        viewer::run(args);
    }
}

#[cfg(any(windows, test))]
fn write_new(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let folder = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(folder)
        .map_err(|e| format!("一時ファイルを作成できません。{e}"))?;
    file.write_all(bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| format!("ファイルを書き込めません。{e}"))?;
    file.persist_noclobber(path)
        .map_err(|e| format!("新しい名前で保存できません。{e}"))?;
    Ok(())
}

#[cfg(windows)]
fn read_watermark(path: &std::path::Path) -> Result<Vec<u8>, String> {
    read_image_file(path, appearance::validate_watermark)
}

#[cfg(windows)]
fn read_signature_image(path: &std::path::Path) -> Result<Vec<u8>, String> {
    read_image_file(path, appearance::validate_signature_image)
}

#[cfg(windows)]
fn read_image_file(
    path: &std::path::Path,
    validate: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(appearance::MAX_IMAGE_BYTES as u64 + 1)
                .read_to_end(&mut data)
        })
        .map_err(|e| format!("Could not read the image from {}.\n{e}", path.display()))?;
    validate(&data)?;
    Ok(data)
}

#[cfg(not(windows))]
fn main() {
    eprintln!("SumiPDF は Windows 10 / 11 用のアプリです。");
}
