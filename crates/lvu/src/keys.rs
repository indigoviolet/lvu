//! `lvu --keys`, the terminal-encoding diagnostic (`docs/dialog-system.md`
//! §8.10). It lives here, beside the input handling it explains, rather than
//! in `lvu-app`, which does not depend on crossterm.

use std::env;
use std::io::IsTerminal as _;

/// `lvu --keys`: what the terminal actually sent, and what crossterm made of
/// it (`docs/dialog-system.md` §8.10).
///
/// The reason this exists is that a chord that "does nothing" has two very
/// different causes and no way to tell them apart from inside the TUI. An
/// xterm with its default `metaSendsEscape: false` sends Alt-f as the single
/// 8-bit character U+00E6 (`æ`), not as `ESC f`, so no application can see an
/// Alt chord there however it is bound; with `metaSendsEscape: true` the same
/// key sends `1b 66` and crossterm reports `ALT+Char('f')`. This prints both
/// halves — the raw bytes and the decoded `KeyEvent` — so the two cases are
/// one glance apart.
///
/// It reads the terminal directly rather than reusing the TUI's event loop: no
/// alternate screen, no mouse capture, no workspace, no capture root. Raw mode
/// is restored on every exit path, including the error one.
pub fn report() -> Result<(), String> {
    use std::io::Write as _;

    if !std::io::stdin().is_terminal() {
        return Err("--keys needs a terminal on stdin".into());
    }
    println!(
        "lvu --keys · press keys to see what this terminal sends. Ctrl-C exits.\n\
         TERM={term}  COLORTERM={colorterm}\n\n\
         If Alt-f shows as Char('æ') with no ALT modifier, your terminal is not\n\
         sending a chord at all — press the underlined letter on its own\n\
         instead, or set xterm's metaSendsEscape.\n\n  \
         {code:<24} {bytes:<14} modifiers",
        term = env::var("TERM").unwrap_or_else(|_| "(unset)".into()),
        colorterm = env::var("COLORTERM").unwrap_or_else(|_| "(unset)".into()),
        code = "decoded key",
        bytes = "bytes sent",
    );
    crossterm::terminal::enable_raw_mode().map_err(|error| format!("raw mode: {error}"))?;
    let outcome = keys_loop();
    // Restore the terminal before anything is reported, on both paths.
    let restored = crossterm::terminal::disable_raw_mode();
    let _ = std::io::stdout().flush();
    outcome?;
    restored.map_err(|error| format!("restoring the terminal: {error}"))?;
    println!();
    Ok(())
}

fn keys_loop() -> Result<(), String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use std::io::Write as _;

    let mut out = std::io::stdout();
    loop {
        let event = event::read().map_err(|error| format!("reading the terminal: {error}"))?;
        let Event::Key(key) = event else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        // crossterm hands back the decoded event, not the bytes, so the byte
        // column is reconstructed from the decoding — which is exactly the
        // mapping a reader needs to check, and it is faithful for the two
        // encodings that matter here.
        let bytes = key_bytes(&key);
        let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let modifiers = if key.modifiers.is_empty() {
            "none".to_owned()
        } else {
            format!("{:?}", key.modifiers)
                .trim_start_matches("KeyModifiers(")
                .trim_end_matches(')')
                .to_owned()
        };
        let _ = writeln!(
            out,
            "  {:<24} {:<14} {modifiers}\r",
            format!("{:?}", key.code),
            hex.join(" "),
        );
        let _ = out.flush();
    }
}

/// The bytes a terminal sends for a decoded key, for the printable cases
/// `--keys` is asked about. An `ALT` chord is the ESC-prefixed form, because
/// that is the only form crossterm reports as `ALT` at all.
fn key_bytes(key: &crossterm::event::KeyEvent) -> Vec<u8> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut bytes = Vec::new();
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }
    match key.code {
        KeyCode::Char(ch) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let upper = ch.to_ascii_uppercase() as u32;
            if (0x40..0x60).contains(&upper) {
                bytes.push(u8::try_from(upper - 0x40).unwrap_or(0));
            }
        }
        KeyCode::Char(ch) => {
            let mut buffer = [0u8; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
        }
        KeyCode::Enter => bytes.push(b'\r'),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Esc => bytes.push(0x1b),
        _ => {}
    }
    bytes
}
