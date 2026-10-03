use alacritty_terminal::event::EventListener;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Color, CursorShape};
use std::fmt::Write as _;
use std::sync::Arc;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Col {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

const BOLD: u16 = 1;
const DIM: u16 = 2;
const ITALIC: u16 = 4;
const UNDERLINE: u16 = 8;
const INVERSE: u16 = 16;
const HIDDEN: u16 = 32;
const STRIKE: u16 = 64;
const SPACER: u16 = 128;
const WIDE: u16 = 256;
const DOUBLE_UNDERLINE: u16 = 512;
const CURLY_UNDERLINE: u16 = 1024;

const ACTIVE: Col = Col::Rgb(0xd9, 0x77, 0x57);
const INACTIVE: Col = Col::Rgb(0x8a, 0x8a, 0x8a);
const HOVERED: Col = Col::Rgb(0xee, 0xee, 0xee);
const BLACK: Col = Col::Rgb(0, 0, 0);

#[derive(Clone, PartialEq, Default)]
pub struct Cell {
    ch: char,
    extra: Option<Box<[char]>>,
    fg: Col,
    bg: Col,
    attrs: u16,
    link: Option<Arc<str>>,
}

pub enum Target {
    Tab(usize),
    Close(usize),
    New,
}

pub struct Region {
    pub start: u16,
    pub end: u16,
    pub target: Target,
}

pub struct Screen {
    cols: u16,
    rows: u16,
    front: Vec<Cell>,
    back: Vec<Cell>,
    out: Vec<u8>,
}

fn blank() -> Cell {
    Cell { ch: ' ', ..Default::default() }
}

fn convert_color(color: Color) -> Col {
    match color {
        Color::Spec(rgb) => Col::Rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => Col::Idx(i),
        Color::Named(named) => match named as usize {
            n @ 0..=15 => Col::Idx(n as u8),
            n @ 259..=266 => Col::Idx((n - 259) as u8),
            _ => Col::Default,
        },
    }
}

fn push_color(out: &mut String, color: Col, base: u8) {
    match color {
        Col::Default => {
            let _ = write!(out, ";{}", base + 9);
        },
        Col::Idx(i) => {
            let _ = write!(out, ";{};5;{i}", base + 8);
        },
        Col::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{r};{g};{b}", base + 8);
        },
    }
}

fn sgr(cell: &Cell) -> String {
    let mut s = String::from("\x1b[0");
    for (bit, code) in [
        (BOLD, "1"),
        (DIM, "2"),
        (ITALIC, "3"),
        (UNDERLINE, "4"),
        (DOUBLE_UNDERLINE, "4:2"),
        (CURLY_UNDERLINE, "4:3"),
        (INVERSE, "7"),
        (HIDDEN, "8"),
        (STRIKE, "9"),
    ] {
        if cell.attrs & bit != 0 {
            s.push(';');
            s.push_str(code);
        }
    }
    push_color(&mut s, cell.fg, 30);
    push_color(&mut s, cell.bg, 40);
    s.push('m');
    s
}

fn truncate(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if width + w + 1 > max {
            break;
        }
        width += w;
        out.push(c);
    }
    out.push('…');
    out
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Screen {
        let cells = cols as usize * rows as usize;
        Screen { cols, rows, front: vec![Cell::default(); cells], back: vec![blank(); cells], out: Vec::new() }
    }

    pub fn invalidate(&mut self) {
        self.front.fill(Cell::default());
    }

    fn put_str(&mut self, mut x: u16, y: u16, text: &str, fg: Col, bg: Col, attrs: u16) -> u16 {
        for ch in text.chars() {
            let w = ch.width().unwrap_or(0) as u16;
            if w == 0 || x + w > self.cols {
                continue;
            }
            let i = y as usize * self.cols as usize + x as usize;
            self.back[i] = Cell { ch, fg, bg, attrs: attrs | if w == 2 { WIDE } else { 0 }, ..blank() };
            if w == 2 {
                self.back[i + 1] = Cell { fg, bg, attrs: SPACER, ..blank() };
            }
            x += w;
        }
        x
    }

    pub fn draw_tabbar(&mut self, titles: &[&str], active: usize, hover: Option<u16>) -> Vec<Region> {
        self.back[..self.cols as usize].fill(blank());
        let count = titles.len().max(1);
        let available = self.cols.saturating_sub(3) as usize;
        let title_max = (available / count).saturating_sub(4).clamp(1, 32);
        let mut regions = Vec::new();
        let mut x = 0u16;
        for (i, title) in titles.iter().enumerate() {
            let text = truncate(title, title_max);
            let width = text.width() as u16 + 4;
            if x + width > available as u16 {
                break;
            }
            let end = x + width;
            let hovered = hover.is_some_and(|h| h >= x && h < end);
            let close_hovered = hover.is_some_and(|h| h + 3 >= end && h < end);
            let bg = if i == active {
                ACTIVE
            } else if hovered {
                HOVERED
            } else {
                INACTIVE
            };
            let after_title = self.put_str(x, 0, &format!(" {text}"), BLACK, bg, BOLD);
            let glyph = if close_hovered { " \u{f00d}" } else { " ×" };
            self.put_str(after_title, 0, glyph, BLACK, bg, BOLD);
            self.put_str(end - 1, 0, "▕", BLACK, bg, 0);
            regions.push(Region { start: x, end: end - 3, target: Target::Tab(i) });
            regions.push(Region { start: end - 3, end, target: Target::Close(i) });
            x = end;
        }
        if x + 3 <= self.cols {
            let hovered = hover.is_some_and(|h| h >= x && h < x + 3);
            self.put_str(x, 0, " + ", BLACK, if hovered { HOVERED } else { INACTIVE }, BOLD);
            regions.push(Region { start: x, end: x + 3, target: Target::New });
        }
        regions
    }

    /// Copies the visible part of a tab's terminal below the tab bar, returning where the cursor is
    pub fn draw_term<T: EventListener>(&mut self, term: &Term<T>) -> Option<(u16, u16)> {
        let cols = self.cols as usize;
        self.back[cols..].fill(blank());
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let rows = self.rows as i32 - 1;
        for indexed in content.display_iter {
            let y = indexed.point.line.0 + offset;
            let x = indexed.point.column.0;
            if y < 0 || y >= rows || x >= cols {
                continue;
            }
            let cell = indexed.cell;
            let f = cell.flags;
            let mut attrs = 0;
            for (flag, bit) in [
                (Flags::BOLD, BOLD),
                (Flags::DIM, DIM),
                (Flags::ITALIC, ITALIC),
                (Flags::UNDERLINE, UNDERLINE),
                (Flags::DOUBLE_UNDERLINE, DOUBLE_UNDERLINE),
                (Flags::UNDERCURL, CURLY_UNDERLINE),
                (Flags::INVERSE, INVERSE),
                (Flags::HIDDEN, HIDDEN),
                (Flags::STRIKEOUT, STRIKE),
                (Flags::WIDE_CHAR, WIDE),
            ] {
                if f.contains(flag) {
                    attrs |= bit;
                }
            }
            if f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                attrs |= SPACER;
            }
            self.back[(y as usize + 1) * cols + x] = Cell {
                ch: cell.c,
                extra: cell.zerowidth().map(Into::into),
                fg: convert_color(cell.fg),
                bg: convert_color(cell.bg),
                attrs,
                link: cell.hyperlink().map(|h| Arc::from(h.uri())),
            };
        }
        if content.cursor.shape == CursorShape::Hidden {
            return None;
        }
        let y = content.cursor.point.line.0 + offset;
        (y >= 0 && y < rows).then_some((content.cursor.point.column.0 as u16, y as u16 + 1))
    }

    pub fn draw_message(&mut self, text: &str) {
        let cols = self.cols as usize;
        self.back[cols..].fill(blank());
        self.put_str(1, 1, text, Col::Default, Col::Default, 0);
    }

    /// Emits only the cells that changed since the last frame
    pub fn flush(&mut self, cursor: Option<(u16, u16)>) -> &[u8] {
        self.out.clear();
        self.out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
        let cols = self.cols as usize;
        let mut position: Option<(usize, usize)> = None;
        let mut style: Option<String> = None;
        let mut link: Option<Arc<str>> = None;
        for i in 0..self.back.len() {
            let cell = &self.back[i];
            if *cell == self.front[i] || cell.attrs & SPACER != 0 {
                continue;
            }
            let (x, y) = (i % cols, i / cols);
            if position != Some((x, y)) {
                self.out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
            }
            let s = sgr(cell);
            if style.as_ref() != Some(&s) {
                self.out.extend_from_slice(s.as_bytes());
                style = Some(s);
            }
            if cell.link != link {
                let uri = cell.link.as_deref().unwrap_or("");
                self.out.extend_from_slice(format!("\x1b]8;;{uri}\x1b\\").as_bytes());
                link = cell.link.clone();
            }
            let mut buf = [0; 4];
            self.out.extend_from_slice(cell.ch.encode_utf8(&mut buf).as_bytes());
            for c in cell.extra.iter().flat_map(|e| e.iter()) {
                self.out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            let width = if cell.attrs & WIDE != 0 { 2 } else { 1 };
            position = Some((x + width, y));
        }
        if link.is_some() {
            self.out.extend_from_slice(b"\x1b]8;;\x1b\\");
        }
        self.out.extend_from_slice(b"\x1b[0m");
        if let Some((x, y)) = cursor {
            self.out.extend_from_slice(format!("\x1b[{};{}H\x1b[?25h", y + 1, x + 1).as_bytes());
        }
        self.out.extend_from_slice(b"\x1b[?2026l");
        self.front.clone_from(&self.back);
        &self.out
    }
}
