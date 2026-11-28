//! Machine-readable operations on the same Windows EXE as the viewer.
use crate::pdfseal::Target;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

const HELP: &str = r#"SumiPDF.exe FILE.pdf [OTHER.pdf ...]
SumiPDF.exe --inspect FILE.pdf
SumiPDF.exe --preview FILE.pdf --out PAGE.png (--page N [--rect L,T,R,B] | --field ID)
SumiPDF.exe --certificates
SumiPDF.exe --sign FILE.pdf --out SIGNED.pdf (--field ID | --page N --rect L,T,R,B)
            [--watermark LOGO.png] [--signature-image IMAGE]
            [--certificate THUMBPRINT | --pfx FILE.pfx] [--expect-sha256 HASH]

Pages start at 1. Rectangles use fractions of the visible page (top-left 0,0).
--inspect returns JSON with page count, writable signature fields and input SHA-256.
For signed PDFs, alreadySigned and allowsNewField describe placement options.
--preview saves a page PNG with an optional target outline. --sign creates a new PDF.
--signature-image places a PNG or JPEG in the left half of the visible signature.
With --pfx, enter the password in the Windows dialog. With neither identity option, SumiPDF opens its Windows certificate store picker. Use --certificates
to list eligible identities in CurrentUser/My and LocalMachine/My. A key provider may request a PIN.
Existing output files are preserved. Commands return JSON except --help."#;

#[derive(Debug, PartialEq)]
enum Identity {
    Store([u8; 20]),
    Pfx(PathBuf),
}

enum Command {
    Help,
    Certificates,
    Inspect(PathBuf),
    Preview {
        input: PathBuf,
        output: PathBuf,
        page: Option<u32>,
        target: Option<Target>,
    },
    Sign {
        input: PathBuf,
        output: PathBuf,
        target: Target,
        watermark: Option<PathBuf>,
        signature_image: Option<PathBuf>,
        certificate: Option<Identity>,
        expected: Option<[u8; 32]>,
    },
}

pub fn is_command(first: Option<&OsString>) -> bool {
    first
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with("--"))
}

struct Flags(BTreeMap<String, OsString>);

impl Flags {
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self, String> {
        let mut flags = BTreeMap::new();
        while let Some(key) = args.next() {
            let key = key.into_string().map_err(|_| "オプション名が不正です。")?;
            if !key.starts_with("--") {
                return Err(format!("不明な引数です: {key}"));
            }
            let val = args
                .next()
                .ok_or_else(|| format!("{key} の値が必要です。"))?;
            if flags.insert(key.clone(), val).is_some() {
                return Err(format!("{key} が重複しています。"));
            }
        }
        Ok(Self(flags))
    }
    fn text(&mut self, key: &str) -> Result<Option<String>, String> {
        self.0
            .remove(key)
            .map(|v| {
                v.into_string()
                    .map_err(|_| format!("{key} の値が不正です。"))
            })
            .transpose()
    }
    fn path(&mut self, key: &str) -> Option<PathBuf> {
        self.0.remove(key).map(PathBuf::from)
    }
}

fn hash_bytes<const N: usize>(value: &str) -> Result<[u8; N], String> {
    let hex = value.as_bytes();
    if hex.len() != 2 * N || !hex.iter().all(u8::is_ascii_hexdigit) {
        return Err(format!("{} 桁の16進数が必要です。", 2 * N));
    }
    let mut bytes = [0; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(bytes)
}

fn parse(args: Vec<OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let verb = args.next().unwrap_or_default();
    let verb = verb.to_str().unwrap_or("");
    if matches!(verb, "--help" | "--certificates") {
        if args.next().is_some() {
            return Err(format!("{verb} は引数を取りません。"));
        }
        return Ok(if verb == "--help" {
            Command::Help
        } else {
            Command::Certificates
        });
    }
    if !matches!(verb, "--inspect" | "--preview" | "--sign") {
        return Err("不明なコマンドです。--help で使い方を確認できます。".into());
    }
    let input = PathBuf::from(args.next().ok_or("入力 PDF が必要です。")?);
    let mut flags = Flags::parse(args)?;
    let command = if verb == "--inspect" {
        Command::Inspect(input)
    } else {
        let output = flags
            .path("--out")
            .ok_or("--out で新しい出力ファイル名を指定してください。")?;
        let field = flags.text("--field")?;
        let page = flags
            .text("--page")?
            .map(|p| {
                p.parse::<u32>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or("ページは1以上の整数で指定してください。")
            })
            .transpose()?;
        let rect = flags
            .text("--rect")?
            .map(|r| {
                let values: Vec<f32> = r
                    .split(',')
                    .map(|n| n.trim().parse::<f32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| "座標を数値で指定してください。")?;
                let a: [f32; 4] = values
                    .try_into()
                    .map_err(|_| "--rect には左,上,右,下の4個の数値が必要です。")?;
                if !a.iter().all(|v| (0.0..=1.0).contains(v)) || a[0] >= a[2] || a[1] >= a[3] {
                    return Err("矩形がページ外または空です。");
                }
                Ok(a)
            })
            .transpose()?;
        let target = match (field,page,rect) {
            (Some(id),None,None) if !id.is_empty() => Some(Target::Field(id)),
            (None,Some(page),Some(rect)) => Some(Target::Area{page,rect}),
            (None,Some(_),None) if verb=="--preview" => None,
            _ => return Err("--field か、--page と --rect の組を指定してください。ページ画像には --page だけでも指定できます。".into()),
        };
        if verb == "--preview" {
            Command::Preview {
                input,
                output,
                page,
                target,
            }
        } else {
            let certificate = match (flags.text("--certificate")?, flags.path("--pfx")) {
                (Some(_), Some(_)) => {
                    return Err("--certificate と --pfx はどちらか一方を指定してください。".into());
                }
                (Some(hash), None) => Some(Identity::Store(hash_bytes(&hash)?)),
                (None, Some(path)) => Some(Identity::Pfx(path)),
                (None, None) => None,
            };
            Command::Sign {
                input,
                output,
                target: target.ok_or("署名欄が必要です。")?,
                watermark: flags.path("--watermark"),
                signature_image: flags.path("--signature-image"),
                certificate,
                expected: flags
                    .text("--expect-sha256")?
                    .as_deref()
                    .map(hash_bytes)
                    .transpose()?,
            }
        }
    };
    if let Some((key, _)) = flags.0.first_key_value() {
        return Err(format!("{verb} で使用できないオプションです: {key}"));
    }
    Ok(command)
}

#[cfg(windows)]
pub fn attach_console() {
    use windows_sys::Win32::{Foundation::INVALID_HANDLE_VALUE, System::Console::*};
    // Automation usually redirects both handles. Attaching unconditionally could replace them.
    let missing = |which| {
        let h = unsafe { GetStdHandle(which) };
        h.is_null() || h == INVALID_HANDLE_VALUE
    };
    if missing(STD_OUTPUT_HANDLE) && missing(STD_ERROR_HANDLE) {
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[cfg(windows)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(windows)]
pub fn run(args: Vec<OsString>) -> Result<(), String> {
    use crate::{pdfseal, wincrypto::SigningCertificate, write_new};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let result = match parse(args)? {
        Command::Help => {
            println!("{HELP}");
            return Ok(());
        }
        Command::Certificates => json!({"certificates":SigningCertificate::list()?}),
        Command::Inspect(input) => {
            let bytes = std::fs::read(input).map_err(|e| e.to_string())?;
            let mut info = pdfseal::inspect(&bytes)?;
            info["sha256"] = sha256_hex(&bytes).into();
            info
        }
        Command::Preview {
            input,
            output,
            page,
            target,
        } => preview(&input, &output, page, target)?,
        Command::Sign {
            input,
            output,
            target,
            watermark,
            signature_image,
            certificate,
            expected,
        } => {
            if output.exists() {
                return Err("出力先が既に存在します。新しい名前を指定してください。".into());
            }
            let bytes = std::fs::read(input).map_err(|e| e.to_string())?;
            let hash: [u8; 32] = Sha256::digest(&bytes).into();
            if expected.is_some_and(|value| value != hash) {
                return Err("入力 PDF が確認時から変更されています。".into());
            }
            let (page, rect) = pdfseal::locate(&bytes, &target)?;
            let watermark = watermark.map(|p| crate::read_watermark(&p)).transpose()?;
            let signature_image = signature_image
                .map(|p| crate::read_signature_image(&p))
                .transpose()?;
            let cert = match certificate {
                Some(Identity::Store(hash)) => {
                    SigningCertificate::by_thumbprint(hash, std::ptr::null_mut())?
                }
                Some(Identity::Pfx(path)) => {
                    match SigningCertificate::pick_pfx(std::ptr::null_mut(), &path)? {
                        Some(choice) => choice.resolve(std::ptr::null_mut())?,
                        None => None,
                    }
                }
                None => SigningCertificate::choose(std::ptr::null_mut())?,
            }
            .ok_or("証明書の選択をキャンセルしました。")?;
            let name = cert.display_name()?;
            let artwork = crate::appearance::Artwork {
                watermark: watermark.as_deref(),
                signature_image: signature_image.as_deref(),
            };
            let signed = pdfseal::sign_visible(bytes, &target, &name, artwork, |a, b| {
                cert.sign_detached(a, b)
            })?;
            write_new(&output, &signed)?;
            json!({"file":output.to_string_lossy(),"signer":name,"page":page,"rect":rect,"sha256":sha256_hex(&signed)})
        }
    };
    println!("{result}");
    Ok(())
}

#[cfg(windows)]
fn preview(
    input: &std::path::Path,
    output: &std::path::Path,
    page: Option<u32>,
    target: Option<Target>,
) -> Result<serde_json::Value, String> {
    use crate::{pdfseal, winpdf};
    let _apartment = winpdf::initialize()?;
    let input = std::path::absolute(input).map_err(|e| e.to_string())?;
    let doc = winpdf::Document::open_file(&input)?;
    let bytes = doc.signing_source()?.read_bytes()?;
    let (page, rect) = match target {
        Some(target) => {
            let (n, r) = pdfseal::locate(&bytes, &target)?;
            (n, Some(r))
        }
        None => (page.ok_or("ページ番号を指定してください。")?, None),
    };
    if page == 0 || page > doc.page_count() {
        return Err("ページ番号が範囲外です。".into());
    }
    let bitmap = doc.render_fit(page - 1, (1800, 1800), 1.0)?;
    let png = page_png(bitmap.width, bitmap.height, &bitmap.pixels, rect)?;
    crate::write_new(output, &png)?;
    Ok(
        serde_json::json!({"file":output.to_string_lossy(),"page":page,"width":bitmap.width,"height":bitmap.height,"rect":rect,"sha256":sha256_hex(&bytes)}),
    )
}

pub(crate) fn page_png(
    w: u32,
    h: u32,
    bgra: &[u8],
    rect: Option<[f32; 4]>,
) -> Result<Vec<u8>, String> {
    let mut rgb = Vec::with_capacity(bgra.len() / 4 * 3);
    let bounds = rect.map(|[l, t, r, b]| {
        [
            (l * w as f32).floor() as u32,
            (t * h as f32).floor() as u32,
            (r * w as f32).ceil() as u32,
            (b * h as f32).ceil() as u32,
        ]
    });
    for (i, px) in bgra.as_chunks::<4>().0.iter().enumerate() {
        let x = i as u32 % w;
        let y = i as u32 / w;
        let edge = bounds.is_some_and(|[l, t, r, b]| {
            x >= l
                && x < r
                && y >= t
                && y < b
                && (x - l < 3 || r - x <= 3 || y - t < 3 || b - y <= 3)
        });
        rgb.extend_from_slice(&if edge {
            [24, 112, 195]
        } else {
            [px[2], px[1], px[0]]
        });
    }
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, w, h);
    encoder.set_color(png::ColorType::Rgb);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn command(values: &[&str]) -> Result<Command, String> {
        parse(values.iter().map(OsString::from).collect())
    }

    #[test]
    fn command_forms_preserve_paths_targets_and_identity() {
        assert!(is_command(Some(&"--inspect".into())));
        assert!(!is_command(Some(&"契約書.pdf".into())));
        assert!(HELP.contains("--watermark"));
        assert!(HELP.contains("--signature-image"));
        assert!(matches!(command(&["--help"]), Ok(Command::Help)));
        assert!(matches!(
            command(&["--certificates"]),
            Ok(Command::Certificates)
        ));
        assert!(
            matches!(command(&["--inspect","a b.pdf"]),Ok(Command::Inspect(p)) if p==Path::new("a b.pdf"))
        );
        assert!(
            matches!(command(&["--preview","a","--out","私の候補.png","--page","3"]),
            Ok(Command::Preview{input,output,page:Some(3),target:None}) if input==Path::new("a") && output==Path::new("私の候補.png"))
        );
        let digest = "01".repeat(32);
        let id = "ab".repeat(20);
        let args = [
            "--sign",
            "a.pdf",
            "--watermark",
            "my.png",
            "--signature-image",
            "サイン photo.jpeg",
            "--field",
            "existing",
            "--certificate",
            &id,
            "--expect-sha256",
            &digest,
            "--out",
            "b.pdf",
        ];
        let Command::Sign {
            input,
            output,
            target: Target::Field(field),
            watermark,
            signature_image,
            certificate,
            expected,
        } = command(&args).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (input, output, field),
            (
                PathBuf::from("a.pdf"),
                PathBuf::from("b.pdf"),
                "existing".into()
            )
        );
        assert_eq!(watermark, Some(PathBuf::from("my.png")));
        assert_eq!(signature_image, Some(PathBuf::from("サイン photo.jpeg")));
        assert_eq!(certificate, Some(Identity::Store([0xab; 20])));
        assert!(
            matches!(command(&["--sign","a","--field","one","--out","b","--pfx","自分の鍵.p12"]), Ok(Command::Sign { certificate: Some(Identity::Pfx(p)), .. }) if p==Path::new("自分の鍵.p12"))
        );
        assert_eq!(expected, Some([1; 32]));
        assert!(
            matches!(command(&["--sign","a","--page","2","--rect","0,.1,.8,1","--out","b"]),
            Ok(Command::Sign{target:Target::Area{page:2,rect},..}) if rect==[0.,0.1,0.8,1.])
        );
    }

    #[test]
    fn rejects_conflicting_targets_and_invalid_options() {
        for args in [
            &["--help", "a"][..],
            &[
                "--sign",
                "a",
                "--out",
                "b",
                "--field",
                "one",
                "--pfx",
                "a.pfx",
                "--certificate",
                "0000000000000000000000000000000000000000",
            ],
            &["--inspect", "a", "--watermark", "b"],
            &["--preview", "a", "--page", "0", "--out", "b"],
            &["--sign", "a", "--page", "2", "--out", "b"],
            &["--sign", "a", "--field", "one", "--page", "2", "--out", "b"],
            &[
                "--sign",
                "a",
                "--page",
                "1",
                "--rect",
                "NaN,.2,.8,1",
                "--out",
                "b",
            ],
            &[
                "--sign",
                "a",
                "--page",
                "1",
                "--rect",
                ".5,.1,.5,.9",
                "--out",
                "b",
            ],
            &["--sign", "a", "--field", "one", "--out", "b", "--out", "c"],
            &[
                "--sign",
                "a",
                "--field",
                "one",
                "--out",
                "b",
                "--certificate",
                "ff",
            ],
        ] {
            assert!(command(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn preview_outline_and_no_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("page.png");
        let bgra = [11, 22, 33, 255].repeat(20 * 20);
        let png = page_png(20, 20, &bgra, Some([0.25, 0.25, 0.75, 0.75])).unwrap();
        crate::write_new(&out, &png).unwrap();
        assert!(crate::write_new(&out, b"overwrite").is_err());
        let mut reader =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(out).unwrap()))
                .read_info()
                .unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut data).unwrap();
        let pixel = |x: usize, y: usize| &data[(y * 20 + x) * 3..(y * 20 + x) * 3 + 3];
        assert_eq!(pixel(5, 5), [24, 112, 195]);
        assert_eq!(pixel(10, 10), [33, 22, 11]);
        assert_eq!(pixel(15, 14), [33, 22, 11]);
    }
}
