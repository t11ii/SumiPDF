use super::{Document, ObjectId, image};
use png::{ColorType, Decoder, Limits, Transformations};
use std::io::Cursor;
use zune_jpeg::{
    JpegDecoder,
    zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions},
};

pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PIXELS: usize = 4 * 1024 * 1024;
const MAX_SIDE: u32 = 4096;

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

fn check_bytes(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("Choose an image no larger than 8 MiB.".into());
    }
    Ok(())
}

fn check_dimensions(width: u32, height: u32) -> Result<(), String> {
    if !(1..=MAX_SIDE).contains(&width)
        || !(1..=MAX_SIDE).contains(&height)
        || u64::from(width) * u64::from(height) > MAX_PIXELS as u64
    {
        return Err(
            "Choose an image up to 4096 pixels per side and 4,194,304 pixels total.".into(),
        );
    }
    Ok(())
}

pub(super) fn decode_png(bytes: &[u8]) -> Result<Pixels, String> {
    check_bytes(bytes)?;
    let invalid = |e| format!("Could not open the PNG image.\n{e}");
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
    check_dimensions(width, height)?;
    let mut reader = decoder.read_info().map_err(invalid)?;
    if reader.info().animation_control.is_some() {
        return Err("Choose a still PNG image.".into());
    }
    let len = reader
        .output_buffer_size()
        .filter(|&n| n <= MAX_PIXELS * 4)
        .ok_or("The PNG has too many pixels.")?;
    let mut data = vec![0; len];
    let frame = reader.next_frame(&mut data).map_err(invalid)?;
    reader.finish().map_err(invalid)?;
    let (rgb, channels, alpha) = match frame.color_type {
        ColorType::Rgb => (true, 3, false),
        ColorType::Grayscale => (false, 1, false),
        ColorType::Rgba => (true, 4, true),
        ColorType::GrayscaleAlpha => (false, 2, true),
        ColorType::Indexed => return Err("Could not expand the PNG palette.".into()),
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

pub(super) fn decode_signature_image(bytes: &[u8]) -> Result<Pixels, String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return decode_png(bytes);
    }
    check_bytes(bytes)?;
    if !bytes.starts_with(b"\xff\xd8") {
        return Err("Choose a PNG or JPEG for the signature image.".into());
    }
    let invalid = |e| format!("Could not open the JPEG image.\n{e}");
    let options = DecoderOptions::new_safe()
        .set_max_width(MAX_SIDE as usize)
        .set_max_height(MAX_SIDE as usize)
        .jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(invalid)?;
    let (width, height) = decoder
        .dimensions()
        .ok_or("The JPEG image has no size information.")?;
    check_dimensions(width as u32, height as u32)?;
    let samples = decoder.decode().map_err(invalid)?;
    if samples.len() != width * height * 3 {
        return Err("The JPEG pixels do not match the image dimensions.".into());
    }
    Ok(Pixels {
        width: width as u32,
        height: height as u32,
        samples,
        alpha: None,
        rgb: true,
    })
}
