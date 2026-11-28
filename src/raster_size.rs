const FIT_EDGE: u32 = 8192;
const ZOOM_EDGE: u32 = 16_384;
pub const FIT_PIXELS: u64 = 8 * 1024 * 1024;
const ZOOM_PIXELS: u64 = 32 * 1024 * 1024;

pub fn view_pixel_limit(zoom: f64) -> u64 {
    if zoom > 1.0 { ZOOM_PIXELS } else { FIT_PIXELS }
}

fn edge_limit(pixels: u64) -> u32 {
    if pixels > FIT_PIXELS {
        ZOOM_EDGE
    } else {
        FIT_EDGE
    }
}

pub fn fitted_page_size(area: (i32, i32), ratio: f64, zoom: f64) -> (i32, i32) {
    let width = f64::from(area.0).min(f64::from(area.1) * ratio);
    (
        (width * zoom).round().max(1.0) as i32,
        (width / ratio * zoom).round().max(1.0) as i32,
    )
}

pub fn bounded_bitmap_size(size: (u32, u32), pixels: u64) -> (u32, u32) {
    let pixels = pixels.max(1);
    let edge = u64::from(edge_limit(pixels)).min(pixels) as f64;
    let (width, height) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let scale = 1.0_f64
        .min(edge / width.max(height))
        .min((pixels as f64 / (width * height)).sqrt());
    // Round down to stay within both caps. Even a very slender page gets one pixel.
    (
        (width * scale).floor().max(1.0) as u32,
        (height * scale).floor().max(1.0) as u32,
    )
}

pub fn supersampled_size((width, height): (u32, u32)) -> Option<(u32, u32)> {
    // Keep the temporary raster bounded even when the page cache has room for more.
    const SAMPLE_PIXELS: u64 = 4 * 1024 * 1024;
    let samples = bounded_bitmap_size(
        (width.saturating_mul(2), height.saturating_mul(2)),
        SAMPLE_PIXELS,
    );
    // Skip the extra render work if the budget only allows a marginal increase.
    (samples.0 >= width.saturating_add(width.div_ceil(4))
        && samples.1 >= height.saturating_add(height.div_ceil(4)))
    .then_some(samples)
}

#[cfg(test)]
pub fn decoded_size(requested: (u32, u32), actual: (u32, u32)) -> Option<(u32, u32)> {
    bounded_decoded_size(requested, actual, FIT_PIXELS)
}

pub fn bounded_decoded_size(
    requested: (u32, u32),
    actual: (u32, u32),
    pixels: u64,
) -> Option<(u32, u32)> {
    let (w, h) = actual;
    if w == 0 || h == 0 {
        return None;
    }
    if w.max(h) <= edge_limit(pixels) && u64::from(w) * u64::from(h) <= pixels {
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
    fn smoothing_samples_small_pages_and_bounds_the_intermediate_image() {
        assert_eq!(supersampled_size((707, 1000)), Some((1414, 2000)));
        let (width, height) = supersampled_size((990, 1400)).unwrap();
        assert!(width > 990 && height > 1400);
        assert!(u64::from(width) * u64::from(height) <= 4 * 1024 * 1024);
        assert_eq!(supersampled_size((1600, 2400)), None);
    }

    #[test]
    fn fit_bitmap_matches_the_display_box() {
        let ratio = 595.28 / 841.89;
        let (width, height) = fitted_page_size((900, 1000), ratio, 1.0);
        assert_eq!((width, height), (707, 1000));
        assert_eq!(
            bounded_bitmap_size((width as u32, height as u32), FIT_PIXELS),
            (707, 1000)
        );
    }

    #[test]
    fn zoom_budget_keeps_a_long_page_at_its_requested_size() {
        let budget = view_pixel_limit(4.0);
        let requested = (1000, 12000);
        assert_eq!(bounded_bitmap_size(requested, budget), requested);
        assert_eq!(
            bounded_decoded_size(requested, requested, budget),
            Some(requested)
        );
    }

    #[test]
    fn zoom_renders_a_large_portrait_at_four_times_fit() {
        assert_eq!(view_pixel_limit(0.5), 8 * 1024 * 1024);
        assert_eq!(view_pixel_limit(1.0), 8 * 1024 * 1024);
        let budget = view_pixel_limit(4.0);
        assert_eq!(budget, 32 * 1024 * 1024);
        assert_eq!(bounded_bitmap_size((4000, 5600), budget), (4000, 5600));
        let (w, h) = bounded_bitmap_size((8000, 11200), budget);
        assert!(u64::from(w) * u64::from(h) <= budget);
        assert!(w > 4000 && h > 5600);
    }

    #[test]
    fn bounds_large_and_extremely_tall_pages_before_rendering() {
        for size in [(1, u32::MAX), (u32::MAX, 1), (u32::MAX, u32::MAX)] {
            let (w, h) = bounded_bitmap_size(size, FIT_PIXELS);
            assert!((1..=FIT_EDGE).contains(&w) && (1..=FIT_EDGE).contains(&h));
            assert!(u64::from(w) * u64::from(h) <= FIT_PIXELS);
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
