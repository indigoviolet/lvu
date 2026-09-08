//! §6.2 / §5.2: a scrim dims the backdrop, it does not redraw it.
//!
//! The indicator heart in the sidebar's corner is drawn as half-block cells:
//! the glyph says *which* halves are painted, and the two colours say what they
//! are. A cell's foreground is its upper (or filled) pixel and its background
//! the lower one, so the shape of the art is the relationship between fg and
//! bg — not the glyph.
//!
//! The reported bug: opening `/` repainted every foreground `muted` and left
//! every background alone, so each `▀` became grey-over-red and each `▄` a grey
//! bar. The heart read as two flat blocks with a step. That is not a dimmed
//! heart, it is half of one erased, and no amount of contrast tuning fixes it
//! because the two halves are no longer moving together.
//!
//! What these hold: the symbols are untouched, both pixels of every art cell
//! move toward the backdrop by the same amount, the lit pixels stay
//! distinguishable from it, and the art does not move when a layer opens.

use lvu::{
    Action, App,
    component::Open,
    delight::{ActivityState, DelightConfig},
    fixture::FixtureProvider,
    theme::{ColorDepth, Theme, ThemeId, contrast, resolved_rgb},
    ui,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};
use std::time::Duration;

/// The acceptance size, and the smallest one that shows the corner art at all.
const SIZE: (u16, u16) = (80, 24);

/// Where the corner heart lands inside the sidebar: five columns wide, three
/// rows tall, above the sidebar's bottom border.
const ART: (std::ops::Range<u16>, std::ops::Range<u16>) = (1..6, 19..22);

fn draw(theme: Theme, open_search: bool, ascii: bool) -> Buffer {
    let (provider, sources, views) = FixtureProvider::demo();
    let mut app = App::new(sources, views, true);
    if open_search {
        app.handle(Action::Open(Open::Search), &provider);
    }
    let config = DelightConfig::new(true, false, ascii, Duration::from_secs(1));
    let mut terminal = Terminal::new(TestBackend::new(SIZE.0, SIZE.1)).unwrap();
    terminal
        .draw(|frame| {
            ui::render_with_theme(
                frame,
                &mut app,
                &provider,
                theme,
                Some((Duration::ZERO, config, ActivityState::Idle)),
            );
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn screen(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every cell of the corner art, as `(symbol, fg, bg)`.
fn art_cells(buffer: &Buffer) -> Vec<(String, Color, Color)> {
    let (columns, rows) = ART;
    rows.flat_map(|y| {
        columns
            .clone()
            .map(move |x| (x, y))
            .collect::<Vec<_>>()
            .into_iter()
    })
    .map(|(x, y)| {
        let cell = &buffer[(x, y)];
        (cell.symbol().to_owned(), cell.fg, cell.bg)
    })
    .collect()
}

fn is_block(symbol: &str) -> bool {
    matches!(symbol, "▀" | "▄" | "█" | "▌" | "▐" | "░" | "▒" | "▓")
}

/// How far `color` is from `background`, or `None` when either is a colour
/// whose displayed value lvu does not know.
fn separation(color: Color, background: Color) -> Option<f64> {
    contrast(color, background)
}

/// The bug, stated as the property it broke: the scrim must not change which
/// halves of the art are lit, and must move both halves of a cell together.
#[test]
fn the_scrim_dims_the_corner_art_without_flattening_its_shape() {
    for id in [ThemeId::LoveDark, ThemeId::LoveLight, ThemeId::Dracula] {
        let theme = id.theme();
        let plain = draw(theme, false, false);
        let scrimmed = draw(theme, true, false);

        let before = art_cells(&plain);
        let after = art_cells(&scrimmed);
        assert!(
            before.iter().any(|(symbol, _, _)| is_block(symbol)),
            "{id:?}: the corner art is not on screen, so this proves nothing\n{}",
            screen(&plain)
        );
        assert_eq!(
            before
                .iter()
                .map(|(symbol, _, _)| symbol)
                .collect::<Vec<_>>(),
            after
                .iter()
                .map(|(symbol, _, _)| symbol)
                .collect::<Vec<_>>(),
            "{id:?}: the scrim moved or replaced a glyph\n{}",
            screen(&scrimmed)
        );

        for (index, ((symbol, fg, bg), (_, dim_fg, dim_bg))) in
            before.iter().zip(after.iter()).enumerate()
        {
            if !is_block(symbol) {
                continue;
            }
            // Every pixel of the cell keeps its own colour rather than being
            // repainted with one shared muted foreground: this is exactly what
            // grey-bar-over-red-bar was.
            assert_ne!(
                dim_fg,
                dim_bg,
                "{id:?} cell {index}: both pixels ended the same colour, so the \
                 half-block has no shape left\n{}",
                screen(&scrimmed)
            );
            // The lit pixel stays lit — quieter than before, but still clearly
            // not the backdrop.
            let backdrop = theme.base_bg;
            let (lit, dim_lit) = if separation(*fg, backdrop) >= separation(*bg, backdrop) {
                (*fg, *dim_fg)
            } else {
                (*bg, *dim_bg)
            };
            let was = separation(lit, backdrop).expect("the art is RGB");
            let now = separation(dim_lit, backdrop).expect("a dimmed pixel stays RGB");
            // A pixel that already sat on the backdrop can cross it by a
            // rounding step on its way there; anything visible must recede.
            assert!(
                now <= was + 0.05,
                "{id:?} cell {index}: the scrim brightened the art ({was:.2} -> {now:.2})"
            );
            if was >= 1.5 {
                assert!(
                    now >= 1.25,
                    "{id:?} cell {index}: the lit pixel faded into the backdrop \
                     ({was:.2} -> {now:.2})"
                );
            }
        }
        // And it *is* dimmer: at least one cell actually moved.
        assert_ne!(before, after, "{id:?}: the scrim did nothing at all");
    }
}

/// The other half of the report: the art must not jump when a layer opens.
#[test]
fn the_corner_art_keeps_its_anchor_when_a_layer_opens() {
    for id in [ThemeId::LoveDark, ThemeId::Terminal] {
        let theme = id.theme();
        let plain = draw(theme, false, false);
        let scrimmed = draw(theme, true, false);
        let occupied = |buffer: &Buffer| {
            let mut cells = Vec::new();
            for y in 0..SIZE.1 {
                for x in 0..SIZE.0 {
                    if is_block(buffer[(x, y)].symbol()) {
                        cells.push((x, y));
                    }
                }
            }
            cells
        };
        assert_eq!(
            occupied(&plain),
            occupied(&scrimmed),
            "{id:?}: the art moved when Search opened\n{}",
            screen(&scrimmed)
        );
    }
}

/// Sixteen colours and the ASCII fallback: the same rule, and no panic.
///
/// The art is decoded from checked-in SGR assets, so its pixels are `Rgb` at
/// every depth — `with_depth` changes the chrome around it, not the picture.
/// The ASCII fallback draws no block cells at all, which is the other way to
/// satisfy the rule: there is no shape for a scrim to flatten.
#[test]
fn the_rule_holds_at_sixteen_colours_and_the_ascii_fallback_has_no_shape_to_lose() {
    let theme = ThemeId::LoveDark.theme().with_depth(ColorDepth::Ansi16);
    let plain = draw(theme, false, false);
    let scrimmed = draw(theme, true, false);
    for ((symbol, fg, bg), (_, dim_fg, dim_bg)) in
        art_cells(&plain).iter().zip(art_cells(&scrimmed).iter())
    {
        if !is_block(symbol) {
            continue;
        }
        assert_ne!(
            dim_fg,
            dim_bg,
            "sixteen colours: the half-block lost its shape\n{}",
            screen(&scrimmed)
        );
        // Whatever the depth, a pixel lvu can measure is dimmed toward the
        // backdrop and one it cannot is left alone; neither is repainted muted.
        if resolved_rgb(*fg).is_some() {
            assert_ne!(*dim_fg, theme.muted, "an art pixel was repainted as text");
        }
        let _ = bg;
    }

    // ASCII: the corner art is off, the footer badge is text, and a scrim over
    // text is the ordinary muted pass.
    let ascii = draw(ThemeId::LoveDark.theme(), true, true);
    let (columns, rows) = ART;
    for y in rows {
        for x in columns.clone() {
            assert!(
                !is_block(ascii[(x, y)].symbol()),
                "the ASCII fallback drew a block cell at ({x},{y})\n{}",
                screen(&ascii)
            );
        }
    }
}
