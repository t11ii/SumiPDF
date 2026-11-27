//! Virtual page layout. The leading page and a fraction of its stride describe
//! a reading position without summing the heights of preceding pages.
use std::collections::BTreeMap;

#[derive(Clone, Copy, Default)]
pub struct Position {
    pub page: u32,
    fraction: f64,
    horizontal: f64,
    ratio: Option<f64>,
}

impl Position {
    pub fn at(page: u32) -> Self {
        Self {
            page,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub page: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub struct Layout {
    pub position: Position,
    count: u32,
    area: (i32, i32),
    margin: i32,
    gap: i32,
    zoom: f64,
    ratios: BTreeMap<u32, f64>,
    typical: Option<f64>,
}

impl Layout {
    pub fn new(count: u32, position: Position) -> Self {
        Self {
            count,
            position,
            area: (1, 1),
            gap: 1,
            margin: 0,
            zoom: 1.0,
            ratios: position
                .ratio
                .into_iter()
                .map(|ratio| (position.page, ratio))
                .collect(),
            typical: position.ratio,
        }
    }

    pub fn configure(&mut self, area: (i32, i32), margin: i32, gap: i32, zoom: f64) {
        self.area = area;
        self.margin = margin;
        self.gap = gap.max(1);
        self.zoom = zoom;
        self.clamp();
    }

    pub fn measured(&mut self, page: u32, ratio: f64) {
        self.ratios.insert(page, ratio);
        self.typical.get_or_insert(ratio);
        self.position.ratio = self.ratios.get(&self.position.page).copied();
        self.clamp();
    }

    fn size(&self, page: u32) -> (i32, i32) {
        let ratio = self
            .ratios
            .get(&page)
            .copied()
            .or(self.typical)
            .unwrap_or(210.0 / 297.0);
        let w = f64::from(self.area.0).min(f64::from(self.area.1) * ratio);
        (
            (w * self.zoom).round().max(1.0) as i32,
            (w / ratio * self.zoom).round().max(1.0) as i32,
        )
    }

    fn stride(&self, page: u32) -> f64 {
        f64::from(self.size(page).1 + self.gap)
    }

    fn clamp(&mut self) {
        self.position.page = self.position.page.min(self.count.saturating_sub(1));
        let max_x = (f64::from(self.area.0) * (self.zoom - 1.0) / 2.0).max(0.0);
        self.position.horizontal = self.position.horizontal.clamp(-max_x, max_x);
        if self.position.page == self.count.saturating_sub(1) {
            let mut page = self.position.page;
            let stride = self.stride(page);
            let mut offset = f64::from(self.size(page).1 - self.area.1);
            if offset < 0.0 {
                // Bottom-align a short final page, keeping the preceding pages
                // in view. A document shorter than one screen starts at the top.
                let mut preceding = 0.0;
                while preceding < -offset && page > 0 {
                    page -= 1;
                    preceding += self.stride(page);
                }
                offset = offset.max(-preceding);
            }
            let maximum = offset / stride;
            self.position.fraction = self.position.fraction.clamp(maximum.min(0.0), maximum);
        }
    }

    pub fn jump(&mut self, page: u32) {
        self.position.page = page.min(self.count.saturating_sub(1));
        self.position.fraction = 0.0;
        self.position.ratio = self.ratios.get(&self.position.page).copied();
        self.forget_distant_sizes();
        self.clamp();
    }

    pub fn scroll(&mut self, dx: f64, dy: f64) -> bool {
        let old = self.position;
        self.position.horizontal += dx;
        if dy == 0.0 {
            self.clamp();
            return old.horizontal != self.position.horizontal;
        }
        let mut page = old.page;
        let mut offset = old.fraction * self.stride(page) + dy;
        while offset < 0.0 && page > 0 {
            page -= 1;
            offset += self.stride(page);
        }
        while page + 1 < self.count && offset >= self.stride(page) {
            offset -= self.stride(page);
            page += 1;
        }
        self.position.page = page;
        self.position.ratio = self.ratios.get(&page).copied();
        self.position.fraction = (offset / self.stride(page)).clamp(0.0, 1.0);
        if dy > 0.0
            && page + 1 < self.count
            && self.visible().last().is_some_and(|last| {
                last.page + 1 == self.count && last.y + last.h <= self.margin + self.area.1
            })
        {
            self.position.page = self.count - 1;
            self.position.ratio = self.ratios.get(&self.position.page).copied();
            self.position.fraction = 1.0;
        }
        self.clamp();
        if old.page != page {
            self.forget_distant_sizes();
        }
        old.page != self.position.page
            || old.fraction != self.position.fraction
            || old.horizontal != self.position.horizontal
    }

    fn forget_distant_sizes(&mut self) {
        // Even one-pixel-high pages have a gutter. Only retain geometry for
        // about two screens on either side of the reading position.
        let reach = (2 * (self.area.1 + 2 * self.margin) / self.gap + 4) as u32;
        let page = self.position.page;
        self.ratios
            .retain(|index, _| page.abs_diff(*index) <= reach);
    }

    fn place(&self, page: u32, y: i32) -> Placement {
        let (w, h) = self.size(page);
        let overflow = ((w - self.area.0 + 1) / 2).max(0);
        let pan = (self.position.horizontal.round() as i32).clamp(-overflow, overflow);
        Placement {
            page,
            x: self.margin + (self.area.0 - w) / 2 + pan,
            y,
            w,
            h,
        }
    }

    pub fn visible(&self) -> Vec<Placement> {
        let mut pages = Vec::new();
        if self.count == 0 {
            return pages;
        }
        let mut index = self.position.page;
        let mut y = self.margin - (self.position.fraction * self.stride(index)).round() as i32;
        while index > 0 && y > self.gap {
            index -= 1;
            y -= self.size(index).1 + self.gap;
        }
        while index < self.count && y < self.area.1 + 2 * self.margin {
            let placement = self.place(index, y);
            y += placement.h + self.gap;
            if placement.y + placement.h > 0 {
                pages.push(placement);
            }
            index += 1;
        }
        pages
    }

    pub fn wanted(&self) -> Vec<u32> {
        let visible = self.visible();
        // Render the leading page first, then the rest of the viewport in
        // reading order. Prefetch just one page beyond either edge.
        let mut pages = vec![self.position.page];
        pages.extend(
            visible
                .iter()
                .map(|p| p.page)
                .filter(|p| *p != self.position.page),
        );
        if let Some(last) = visible.last()
            && last.page + 1 < self.count
        {
            pages.push(last.page + 1);
        }
        if let Some(first) = visible.first()
            && first.page > 0
        {
            pages.push(first.page - 1);
        }
        pages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(count: u32) -> Layout {
        let mut l = Layout::new(count, Position::at(0));
        l.configure((600, 800), 18, 14, 1.0);
        l
    }

    #[test]
    fn fractionally_scrolls_between_mixed_pages_and_restores_by_page() {
        let mut l = layout(1_000_000);
        l.measured(0, 0.5);
        l.measured(1, 2.0);
        assert!(l.scroll(0.0, 400.5));
        let shown = l.visible();
        assert_eq!(shown.len(), 3);
        assert_eq!(shown[0].x, 118);
        assert_eq!((shown[0].w, shown[0].h, shown[0].y), (400, 800, -383));
        assert_eq!((shown[1].w, shown[1].h, shown[1].y), (600, 300, 431));
        assert!(l.scroll(0.0, 420.0));
        assert_eq!(l.position.page, 1);
        let saved = l.position;
        let mut reopened = Layout::new(1_000_000, saved);
        reopened.configure((600, 800), 18, 14, 1.0);
        reopened.measured(1, 2.0);
        assert_eq!(
            reopened.visible().iter().find(|p| p.page == 1).unwrap().y,
            l.visible().iter().find(|p| p.page == 1).unwrap().y
        );
        l.jump(999_999);
        assert_eq!(l.position.page, 999_999);
        assert!(!l.scroll(0.0, 600.0));
        assert!(l.wanted().len() <= 3);
        assert!(l.ratios.is_empty());
    }

    #[test]
    fn high_resolution_deltas_accumulate_clamp_and_reverse_at_ends() {
        let mut l = layout(2);
        assert!(!l.scroll(0.0, -100.0));
        for _ in 0..8 {
            l.scroll(0.0, 0.125);
        }
        assert_eq!(l.visible()[0].y, 17);
        l.configure((600, 800), 18, 14, 2.0);
        l.scroll(0.0, 9999.0);
        assert_eq!(l.position.page, 1);
        assert_eq!(l.visible().last().unwrap().y, 18 - 800);
        assert!(!l.scroll(0.0, 9999.0));
        l.scroll(0.0, -9999.0);
        assert_eq!(l.position.page, 0);
        assert_eq!(l.visible()[0].y, 18);
    }

    #[test]
    fn short_last_page_stops_at_the_bottom_and_keeps_the_previous_page_visible() {
        let mut l = layout(2);
        l.measured(0, 0.5);
        l.measured(1, 2.0);
        l.scroll(0.0, 314.0);
        let shown = l.visible();
        assert_eq!(shown.len(), 2);
        assert_eq!(l.position.page, 1);
        assert_eq!(shown[1].y + shown[1].h, 818);
        assert!(!l.scroll(30.0, 0.0));
        assert_eq!(l.position.page, 1);
        assert!(!l.scroll(0.0, 10.0));
        assert!(l.scroll(0.0, -1.0));
        assert_eq!(l.position.page, 0);
        l.jump(1);
        assert_eq!(l.visible()[1].y, 518);
    }
}
