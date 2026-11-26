const MAX_EDGE: u32 = 8192;
const MAX_PIXELS: u64 = 8 * 1024 * 1024;

pub fn bitmap_size(page_width: f64, page_height: f64, requested_width: u32) -> (u32, u32) {
    let width = f64::from(requested_width.clamp(1, MAX_EDGE));
    let height = width * page_height / page_width;
    let scale = 1.0_f64
        .min(f64::from(MAX_EDGE) / height)
        .min((MAX_PIXELS as f64 / (width * height)).sqrt());
    // Round down to stay within both caps. Even a very slender page gets one pixel.
    (
        (width * scale).floor().max(1.0) as u32,
        (height * scale).floor().max(1.0) as u32,
    )
}

pub fn decoded_size(requested: (u32, u32), actual: (u32, u32)) -> Option<(u32, u32)> {
    let (w, h) = actual;
    if w == 0 || h == 0 {
        return None;
    }
    if w.max(h) <= MAX_EDGE && u64::from(w) * u64::from(h) <= MAX_PIXELS {
        return Some(actual);
    }
    // The requested box was bounded before rendering. Fit the actual frame
    // inside it, retaining its aspect ratio even if Windows swapped the axes.
    let scale = (f64::from(requested.0) / f64::from(w)).min(f64::from(requested.1) / f64::from(h));
    Some((
        (f64::from(w) * scale).floor().max(1.0) as u32,
        (f64::from(h) * scale).floor().max(1.0) as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_large_and_extremely_tall_pages_before_rendering() {
        for ratio in [0.0001, 0.1, 1.0, 1.5, 100.0, 1.0e30] {
            let (w, h) = bitmap_size(1.0, ratio, u32::MAX);
            assert!((1..=MAX_EDGE).contains(&w) && (1..=MAX_EDGE).contains(&h));
            assert!(u64::from(w) * u64::from(h) <= MAX_PIXELS);
        }
    }

    #[test]
    fn uses_the_returned_bitmap_dimensions_for_page_pixels() {
        for actual in [(596, 842), (1190, 1684), (842, 595), (298, 421)] {
            assert_eq!(decoded_size((595, 842), actual), Some(actual));
        }
    }

    #[test]
    fn oversized_frames_are_scaled_proportionally_and_empty_frames_are_rejected() {
        assert_eq!(decoded_size((640, 480), (16000, 8000)), Some((640, 320)));
        assert_eq!(decoded_size((640, 480), (4000, 4000)), Some((480, 480)));
        assert_eq!(decoded_size((640, 480), (1, u32::MAX)), Some((1, 480)));
        assert_eq!(decoded_size((640, 480), (0, 100)), None);
        assert_eq!(decoded_size((640, 480), (100, 0)), None);
    }
}
