#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
mod highlight;
#[cfg(test)]
mod highlight_roundtrip;
#[cfg(any(windows, test))]
mod pdfseal;
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
    viewer::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("SumiPDF は Windows 10 / 11 用のアプリです。");
}
