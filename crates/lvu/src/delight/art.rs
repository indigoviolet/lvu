//! Decode only checked-in SGR half-block art; never emit asset escape codes.
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use std::sync::OnceLock;

pub const FRAME_MILLIS: u128 = 110;
const BLACK: Color = Color::Rgb(0, 0, 0);

const SMALL: [&str; 10] = [
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-001.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-002.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-003.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-004.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-005.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-006.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-007.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-008.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-009.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/80x22-sharp/frame-010.ans"
    )),
];
const LARGE: [&str; 10] = [
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-001.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-002.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-003.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-004.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-005.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-006.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-007.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-008.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-009.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/startup/love-you-log-time/120x40-sharp/frame-010.ans"
    )),
];

const INDICATOR: [&str; 4] = [
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/indicator/heartbeat/5x3/frame-001.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/indicator/heartbeat/5x3/frame-002.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/indicator/heartbeat/5x3/frame-003.ans"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/indicator/heartbeat/5x3/frame-004.ans"
    )),
];

pub fn indicator(index: usize) -> &'static Buffer {
    static FRAMES: OnceLock<Vec<Buffer>> = OnceLock::new();
    &FRAMES.get_or_init(|| {
        INDICATOR
            .iter()
            .map(|source| decode(source, 5, 3))
            .collect()
    })[index % 4]
}

pub fn frame(large: bool, index: usize) -> &'static Buffer {
    static SMALL_FRAMES: OnceLock<Vec<Buffer>> = OnceLock::new();
    static LARGE_FRAMES: OnceLock<Vec<Buffer>> = OnceLock::new();
    let (cache, sources, width, height) = if large {
        (&LARGE_FRAMES, &LARGE, 120, 40)
    } else {
        (&SMALL_FRAMES, &SMALL, 80, 22)
    };
    &cache.get_or_init(|| {
        sources
            .iter()
            .map(|source| decode(source, width, height))
            .collect()
    })[index % 10]
}

fn decode(source: &str, width: u16, height: u16) -> Buffer {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    assert_eq!(source.lines().count(), usize::from(height));
    for (y, line) in source.lines().enumerate() {
        let mut remaining = line;
        let mut style = Style::default().fg(BLACK).bg(BLACK);
        let mut x = 0;
        while !remaining.is_empty() {
            if let Some(sgr) = remaining.strip_prefix("\x1b[") {
                let (codes, tail) = sgr
                    .split_once('m')
                    .expect("embedded art must contain SGR only");
                let codes: Vec<u8> = codes
                    .split(';')
                    .map(|value| value.parse().expect("SGR byte"))
                    .collect();
                let mut i = 0;
                while i < codes.len() {
                    match codes[i..] {
                        [0, ..] => style = Style::default().fg(BLACK).bg(BLACK),
                        [38, 2, r, g, b, ..] => {
                            style = style.fg(Color::Rgb(r, g, b));
                            i += 4;
                        }
                        [48, 2, r, g, b, ..] => {
                            style = style.bg(Color::Rgb(r, g, b));
                            i += 4;
                        }
                        _ => panic!("unsupported embedded art SGR"),
                    }
                    i += 1;
                }
                remaining = tail;
            } else {
                let symbol = remaining.chars().next().unwrap();
                assert!(matches!(symbol, ' ' | '▀' | '▄' | '█'));
                assert!(x < width);
                let end = symbol.len_utf8();
                buffer[(x, y as u16)]
                    .set_symbol(&remaining[..end])
                    .set_style(style);
                remaining = &remaining[end..];
                x += 1;
            }
        }
        assert_eq!(x, width);
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_embedded_frames_are_bounded_and_have_true_black_padding() {
        for large in [false, true] {
            for index in 0..10 {
                let frame = frame(large, index);
                assert_eq!(frame.area.width, if large { 120 } else { 80 });
                for y in 0..frame.area.height {
                    for x in [0, frame.area.width - 1] {
                        let cell = &frame[(x, y)];
                        assert_eq!(cell.bg, BLACK);
                        if cell.symbol() != " " {
                            assert_eq!(cell.fg, BLACK);
                        }
                    }
                }
            }
            assert_ne!(frame(large, 0), frame(large, 4));
        }
    }
}
