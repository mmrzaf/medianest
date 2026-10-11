//! Geometry of the photo grid: sections, rows and tile rectangles.
//!
//! This module is pure computation, so it can be tested without a display
//! and stays fast for hundreds of thousands of items.

use std::ops::Range;

/// How items are grouped under headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grouping {
    None,
    Day,
    Month,
}

/// What the layout needs to know about one item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Width divided by height.
    pub aspect: f32,
    /// Local capture time in milliseconds, `None` when unknown.
    pub local_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub width: f32,
    /// Tile size (square mode) or target row height (aspect mode).
    pub target: f32,
    pub gap: f32,
    pub padding: f32,
    pub header_height: f32,
    pub square: bool,
    pub grouping: Grouping,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    #[must_use]
    pub fn center_x(&self) -> f32 {
        self.x + self.width / 2.0
    }

    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }
}

/// A run of items under one header.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Grouping key: days, months or years since the epoch; `i64::MIN` for no date.
    pub key: i64,
    pub items: Range<usize>,
    /// Header rectangle; zero height when there are no headers.
    pub header: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    Header(usize),
    Items(Range<usize>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub y: f32,
    pub height: f32,
    pub kind: RowKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub rows: Vec<Row>,
    pub rects: Vec<Rect>,
    pub sections: Vec<Section>,
    /// For each item, the index of its row in `rows`.
    pub row_of: Vec<u32>,
    pub height: f32,
}

const MS_PER_DAY: i64 = 86_400_000;

/// The grouping key of a capture time.
#[must_use]
pub fn group_key(local_ms: Option<i64>, grouping: Grouping) -> i64 {
    let Some(ms) = local_ms else {
        return i64::MIN;
    };
    let days = ms.div_euclid(MS_PER_DAY);
    match grouping {
        Grouping::None => 0,
        Grouping::Day => days,
        Grouping::Month => chrono::DateTime::from_timestamp_millis(ms)
            .map(|d| d.naive_utc().date())
            .map_or(i64::MIN, |date| {
                i64::from(chrono::Datelike::year(&date)) * 12
                    + i64::from(chrono::Datelike::month0(&date))
            }),
    }
}

impl Layout {
    #[must_use]
    pub fn compute(cells: &[Cell], params: &Params) -> Self {
        let mut layout = Self {
            rects: vec![Rect::default(); cells.len()],
            row_of: vec![0; cells.len()],
            ..Self::default()
        };
        let inner = (params.width - 2.0 * params.padding).max(params.target.min(48.0));
        let mut y = params.padding;
        let mut start = 0;
        while start < cells.len() {
            let key = group_key(cells[start].local_ms, params.grouping);
            let mut end = start + 1;
            if params.grouping != Grouping::None {
                while end < cells.len() && group_key(cells[end].local_ms, params.grouping) == key {
                    end += 1;
                }
            } else {
                end = cells.len();
            }
            let section = layout.sections.len();
            let header = if params.grouping == Grouping::None {
                Rect {
                    x: params.padding,
                    y,
                    width: inner,
                    height: 0.0,
                }
            } else {
                let rect = Rect {
                    x: params.padding,
                    y,
                    width: inner,
                    height: params.header_height,
                };
                layout.rows.push(Row {
                    y,
                    height: params.header_height,
                    kind: RowKind::Header(section),
                });
                y += params.header_height;
                rect
            };
            layout.sections.push(Section {
                key,
                items: start..end,
                header,
            });
            y = if params.square {
                layout.place_square(start..end, y, inner, params)
            } else {
                layout.place_justified(cells, start..end, y, inner, params)
            };
            y += params.gap * 2.0;
            start = end;
        }
        layout.height = y + params.padding;
        layout
    }

    fn push_row(&mut self, y: f32, height: f32, items: Range<usize>) {
        let row = u32::try_from(self.rows.len()).unwrap_or(u32::MAX);
        for index in items.clone() {
            self.row_of[index] = row;
        }
        self.rows.push(Row {
            y,
            height,
            kind: RowKind::Items(items),
        });
    }

    fn place_square(
        &mut self,
        items: Range<usize>,
        mut y: f32,
        inner: f32,
        params: &Params,
    ) -> f32 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let columns =
            (((inner + params.gap) / (params.target + params.gap)).floor() as usize).max(1);
        #[allow(clippy::cast_precision_loss)]
        let size = (inner - params.gap * (columns as f32 - 1.0)) / columns as f32;
        let mut index = items.start;
        while index < items.end {
            let row_end = (index + columns).min(items.end);
            for (column, item) in (index..row_end).enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let x = params.padding + column as f32 * (size + params.gap);
                self.rects[item] = Rect {
                    x,
                    y,
                    width: size,
                    height: size,
                };
            }
            self.push_row(y, size, index..row_end);
            y += size + params.gap;
            index = row_end;
        }
        y - params.gap
    }

    fn place_justified(
        &mut self,
        cells: &[Cell],
        items: Range<usize>,
        mut y: f32,
        inner: f32,
        params: &Params,
    ) -> f32 {
        let mut row_start = items.start;
        while row_start < items.end {
            let mut sum = 0.0f32;
            let mut row_end = row_start;
            let mut height = params.target;
            let mut complete = false;
            while row_end < items.end {
                sum += cells[row_end].aspect.max(0.05);
                row_end += 1;
                #[allow(clippy::cast_precision_loss)]
                let gaps = params.gap * (row_end - row_start - 1) as f32;
                let candidate = (inner - gaps) / sum;
                if candidate <= params.target {
                    height = candidate;
                    complete = true;
                    break;
                }
                height = candidate;
            }
            if !complete {
                // Last, partial row: natural height, left aligned.
                height = height.min(params.target);
            }
            let mut x = params.padding;
            for (item, cell) in cells.iter().enumerate().take(row_end).skip(row_start) {
                let mut width = cell.aspect.max(0.05) * height;
                if complete && item + 1 == row_end {
                    width = (params.padding + inner - x).max(1.0);
                }
                self.rects[item] = Rect {
                    x,
                    y,
                    width,
                    height,
                };
                x += width + params.gap;
            }
            self.push_row(y, height, row_start..row_end);
            y += height + params.gap;
            row_start = row_end;
        }
        y - params.gap
    }

    /// Rows that intersect `top..bottom`.
    #[must_use]
    pub fn rows_between(&self, top: f32, bottom: f32) -> Range<usize> {
        let first = self.rows.partition_point(|row| row.y + row.height < top);
        let last = self.rows.partition_point(|row| row.y <= bottom);
        first..last.max(first)
    }

    /// Item under a point.
    #[must_use]
    pub fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        let rows = self.rows_between(y, y);
        self.rows[rows].iter().find_map(|row| match &row.kind {
            RowKind::Items(items) => items
                .clone()
                .find(|&index| self.rects[index].contains(x, y)),
            RowKind::Header(_) => None,
        })
    }

    /// Section whose header is under a point.
    #[must_use]
    pub fn header_at(&self, x: f32, y: f32) -> Option<usize> {
        let rows = self.rows_between(y, y);
        self.rows[rows].iter().find_map(|row| match row.kind {
            RowKind::Header(section) if self.sections[section].header.contains(x, y) => {
                Some(section)
            }
            _ => None,
        })
    }

    /// Section containing an item.
    #[cfg(test)]
    #[must_use]
    pub fn section_of(&self, item: usize) -> Option<usize> {
        let index = self
            .sections
            .partition_point(|section| section.items.end <= item);
        (index < self.sections.len()).then_some(index)
    }

    /// The item in the row above or below `item` closest horizontally.
    #[must_use]
    pub fn vertical_neighbour(&self, item: usize, down: bool) -> Option<usize> {
        let center = self.rects.get(item)?.center_x();
        let mut row = *self.row_of.get(item)? as usize;
        loop {
            row = if down {
                row.checked_add(1)?
            } else {
                row.checked_sub(1)?
            };
            let candidate = self.rows.get(row)?;
            if let RowKind::Items(items) = &candidate.kind {
                return items.clone().min_by(|a, b| {
                    let da = (self.rects[*a].center_x() - center).abs();
                    let db = (self.rects[*b].center_x() - center).abs();
                    da.total_cmp(&db)
                });
            }
        }
    }

    /// Items whose rectangles intersect a selection band.
    #[must_use]
    pub fn items_in(&self, band: Rect) -> Vec<usize> {
        let rows = self.rows_between(band.y, band.bottom());
        let mut found = Vec::new();
        for row in &self.rows[rows] {
            if let RowKind::Items(items) = &row.kind {
                for index in items.clone() {
                    let rect = self.rects[index];
                    let overlaps = rect.x < band.x + band.width
                        && rect.x + rect.width > band.x
                        && rect.y < band.bottom()
                        && rect.bottom() > band.y;
                    if overlaps {
                        found.push(index);
                    }
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(square: bool, grouping: Grouping) -> Params {
        Params {
            width: 1000.0,
            target: 200.0,
            gap: 4.0,
            padding: 10.0,
            header_height: 40.0,
            square,
            grouping,
        }
    }

    fn day(n: i64) -> Option<i64> {
        Some(n * MS_PER_DAY + 3_600_000)
    }

    #[test]
    fn square_rows_fill_the_width() {
        let cells: Vec<Cell> = (0..10)
            .map(|_| Cell {
                aspect: 1.5,
                local_ms: day(1),
            })
            .collect();
        let layout = Layout::compute(&cells, &params(true, Grouping::Day));
        assert_eq!(layout.sections.len(), 1);
        // 980px inner width fits 4 columns of about 242px.
        let first_row: Vec<_> = layout.rects[..4].iter().map(|r| r.y).collect();
        assert!(
            first_row
                .iter()
                .all(|y| (*y - first_row[0]).abs() < f32::EPSILON)
        );
        let last = layout.rects[3];
        assert!((last.x + last.width - 990.0).abs() < 0.01);
        assert!(layout.rects[4].y > layout.rects[0].y);
    }

    #[test]
    fn justified_rows_are_flush_and_no_taller_than_target() {
        let cells: Vec<Cell> = [1.5, 0.75, 1.0, 1.78, 1.33, 0.5, 2.0, 1.5]
            .iter()
            .map(|&aspect| Cell {
                aspect,
                local_ms: day(2),
            })
            .collect();
        let layout = Layout::compute(&cells, &params(false, Grouping::Day));
        for row in &layout.rows {
            if let RowKind::Items(items) = &row.kind {
                assert!(row.height <= 200.0 + 0.01);
                let last = layout.rects[items.end - 1];
                let full = items.end != cells.len();
                if full {
                    assert!(
                        (last.x + last.width - 990.0).abs() < 0.5,
                        "row {items:?} ends at {}",
                        last.x + last.width
                    );
                }
            }
        }
    }

    #[test]
    fn sections_follow_days_and_unknown_dates() {
        let cells = vec![
            Cell {
                aspect: 1.0,
                local_ms: day(5),
            },
            Cell {
                aspect: 1.0,
                local_ms: day(5),
            },
            Cell {
                aspect: 1.0,
                local_ms: day(4),
            },
            Cell {
                aspect: 1.0,
                local_ms: None,
            },
        ];
        let layout = Layout::compute(&cells, &params(true, Grouping::Day));
        let keys: Vec<i64> = layout.sections.iter().map(|s| s.key).collect();
        assert_eq!(keys, vec![5, 4, i64::MIN]);
        assert_eq!(layout.section_of(1), Some(0));
        assert_eq!(layout.section_of(3), Some(2));
        let header = layout.sections[1].header;
        assert_eq!(layout.header_at(header.x + 5.0, header.y + 5.0), Some(1));
    }

    #[test]
    fn hit_testing_and_navigation() {
        let cells: Vec<Cell> = (0..12)
            .map(|_| Cell {
                aspect: 1.0,
                local_ms: day(1),
            })
            .collect();
        let layout = Layout::compute(&cells, &params(true, Grouping::None));
        let rect = layout.rects[5];
        assert_eq!(layout.item_at(rect.x + 1.0, rect.y + 1.0), Some(5));
        assert_eq!(layout.item_at(5.0, 5.0), None, "padding is empty");
        assert_eq!(layout.vertical_neighbour(1, true), Some(5));
        assert_eq!(layout.vertical_neighbour(5, false), Some(1));
        assert_eq!(layout.vertical_neighbour(1, false), None);
        let band = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: rect.y + 1.0,
        };
        assert_eq!(layout.items_in(band), (0..8).collect::<Vec<_>>());
        let rows = layout.rows_between(rect.y, rect.y);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn large_libraries_lay_out_quickly() {
        let cells: Vec<Cell> = (0..200_000)
            .map(|i| Cell {
                aspect: if i % 3 == 0 { 0.75 } else { 1.5 },
                local_ms: day(i64::from(i / 40)),
            })
            .collect();
        let started = std::time::Instant::now();
        let layout = Layout::compute(&cells, &params(false, Grouping::Day));
        assert!(layout.height > 0.0);
        assert!(
            started.elapsed().as_millis() < 1_500,
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn grouping_keys() {
        let july = chrono::NaiveDate::from_ymd_opt(2023, 7, 14)
            .and_then(|d| d.and_hms_opt(10, 0, 0))
            .map(|d| d.and_utc().timestamp_millis());
        assert_eq!(group_key(july, Grouping::Month), 2023 * 12 + 6);
        assert_eq!(group_key(None, Grouping::Month), i64::MIN);
    }
}
