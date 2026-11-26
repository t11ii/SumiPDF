use super::{Document, ObjectId, image};
use png::{ColorType, Decoder, Limits, Transformations};
use std::io::Cursor;

pub const MAX_WATERMARK_BYTES: usize = 8 * 1024 * 1024;
const MAX_PIXELS: usize = 4 * 1024 * 1024;

pub(super) struct Pixels {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) samples: Vec<u8>,
    pub(super) alpha: Option<Vec<u8>>,
    rgb: bool,
}

impl Pixels {
    pub(super) fn embed(self, doc: &mut Document) -> Result<ObjectId, String> {
        let Self {
            width,
            height,
            samples,
            alpha,
            rgb,
        } = self;
        let mask = alpha
            .map(|data| image(doc, width, height, "DeviceGray", data, None))
            .transpose()?;
        image(
            doc,
            width,
            height,
            if rgb { "DeviceRGB" } else { "DeviceGray" },
            samples,
            mask,
        )
    }
}

pub fn validate_watermark(bytes: &[u8]) -> Result<(), String> {
    decode(bytes).map(|_| ())
}

pub(super) fn decode(bytes: &[u8]) -> Result<Pixels, String> {
    if bytes.len() > MAX_WATERMARK_BYTES {
        return Err("透かしPNGは8 MiB以下にしてください。".into());
    }
    let invalid = |e| format!("透かしPNGを開けませんでした。\n{e}");
    let mut decoder = Decoder::new_with_limits(
        Cursor::new(bytes),
        Limits {
            bytes: 24 * 1024 * 1024,
        },
    );
    decoder.set_transformations(Transformations::normalize_to_color8());
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let header = decoder.read_header_info().map_err(invalid)?;
    let (width, height) = (header.width, header.height);
    if width > 4096 || height > 4096 || u64::from(width) * u64::from(height) > MAX_PIXELS as u64 {
        return Err("透かしPNGは各辺4096ピクセル以内、合計4194304画素以内にしてください。".into());
    }
    let mut reader = decoder.read_info().map_err(invalid)?;
    if reader.info().animation_control.is_some() {
        return Err("透かしには静止画のPNGを選んでください。".into());
    }
    let len = reader
        .output_buffer_size()
        .filter(|&n| n <= MAX_PIXELS * 4)
        .ok_or("透かしPNGの画素数が大きすぎます。")?;
    let mut data = vec![0; len];
    let frame = reader.next_frame(&mut data).map_err(invalid)?;
    reader.finish().map_err(invalid)?;
    let (rgb, channels, alpha) = match frame.color_type {
        ColorType::Rgb => (true, 3, false),
        ColorType::Grayscale => (false, 1, false),
        ColorType::Rgba => (true, 4, true),
        ColorType::GrayscaleAlpha => (false, 2, true),
        ColorType::Indexed => return Err("透かしPNGの色を展開できませんでした。".into()),
    };
    data.truncate(frame.buffer_size());
    let alpha = if alpha {
        let count = width as usize * height as usize;
        let mut coverage = Vec::with_capacity(count);
        // Strip alpha in-place. The destination never overtakes the source.
        for i in 0..count {
            let start = i * channels;
            coverage.push(data[start + channels - 1]);
            data.copy_within(start..start + channels - 1, i * (channels - 1));
        }
        data.truncate(count * (channels - 1));
        (!coverage.iter().all(|&a| a == 255)).then_some(coverage)
    } else {
        None
    };
    Ok(Pixels {
        width,
        height,
        samples: data,
        alpha,
        rgb,
    })
}
