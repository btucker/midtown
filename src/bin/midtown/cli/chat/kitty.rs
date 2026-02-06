//! Kitty graphics protocol support for fullscreen image rendering
//!
//! The Kitty graphics protocol uses APC (Application Program Command) escape
//! sequences to display images inline in the terminal. This module handles
//! encoding PNG data and writing the protocol sequences.
//!
//! Protocol format:
//!   \x1b_G{key=value,...};{base64_data}\x1b\\
//!
//! For large images, data is chunked:
//!   First chunk:  \x1b_G{params},m=1;{chunk}\x1b\\
//!   Middle chunks: \x1b_Gm=1;{chunk}\x1b\\
//!   Last chunk:    \x1b_Gm=0;{chunk}\x1b\\
//!
//! When running inside tmux, APC sequences must be wrapped in DCS passthrough:
//!   \x1bPtmux;\x1b\x1b_G{params};{data}\x1b\x1b\\\x1b\\
//! The ESC characters at the APC start and end delimiters are doubled inside
//! the passthrough. The base64 payload is ESC-free by construction.

use std::io;

use base64ct::{Base64, Encoding};
use crossterm::{cursor::MoveTo, execute, terminal};

/// Maximum bytes of base64 data per chunk (4096 is the Kitty protocol recommendation)
const CHUNK_SIZE: usize = 4096;

/// Check if we're running inside tmux by looking for the TMUX environment variable.
fn is_inside_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

/// Write a Kitty APC sequence, wrapping in tmux DCS passthrough if needed.
///
/// `payload` is the content between `\x1b_G` and `\x1b\\`,
/// e.g. `"a=T,f=100,c=10,r=5,m=0;{base64_data}"`.
///
/// When `tmux_wrap` is true, the sequence is wrapped as:
///   `\x1bPtmux;\x1b\x1b_G{payload}\x1b\x1b\\\x1b\\`
fn write_kitty_apc<W: io::Write>(writer: &mut W, payload: &str, tmux_wrap: bool) -> io::Result<()> {
    if tmux_wrap {
        write!(writer, "\x1bPtmux;\x1b\x1b_G{}\x1b\x1b\\\x1b\\", payload)
    } else {
        write!(writer, "\x1b_G{}\x1b\\", payload)
    }
}

/// Render a fullscreen image using the Kitty graphics protocol.
///
/// Clears the screen, renders the PNG image filling the terminal, then waits
/// for the caller to handle returning to normal display. The image is placed
/// at (0,0) and sized to fill the terminal dimensions.
pub fn render_fullscreen_image<W: io::Write>(writer: &mut W, png_data: &[u8]) -> io::Result<()> {
    render_fullscreen_image_inner(writer, png_data, is_inside_tmux())
}

/// Inner implementation that accepts a tmux flag for testability.
fn render_fullscreen_image_inner<W: io::Write>(
    writer: &mut W,
    png_data: &[u8],
    tmux_wrap: bool,
) -> io::Result<()> {
    if png_data.is_empty() {
        return Ok(());
    }

    // Get terminal dimensions for sizing
    let (cols, rows) = terminal::size().unwrap_or((80, 24));

    // Move cursor to top-left
    execute!(writer, MoveTo(0, 0))?;

    // Encode the PNG data as base64
    let encoded = Base64::encode_string(png_data);

    // Split into chunks for transmission
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(CHUNK_SIZE)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();

    if chunks.is_empty() {
        return Ok(());
    }

    if chunks.len() == 1 {
        // Single chunk: transmit and display in one go
        let payload = format!("a=T,f=100,c={},r={},m=0;{}", cols, rows, chunks[0]);
        write_kitty_apc(writer, &payload, tmux_wrap)?;
    } else {
        // Multiple chunks: first chunk with params
        let payload = format!("a=T,f=100,c={},r={},m=1;{}", cols, rows, chunks[0]);
        write_kitty_apc(writer, &payload, tmux_wrap)?;

        // Middle chunks (continuation)
        for chunk in &chunks[1..chunks.len() - 1] {
            let payload = format!("m=1;{}", chunk);
            write_kitty_apc(writer, &payload, tmux_wrap)?;
        }

        // Last chunk
        let payload = format!("m=0;{}", chunks[chunks.len() - 1]);
        write_kitty_apc(writer, &payload, tmux_wrap)?;
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_size_constant() {
        assert_eq!(CHUNK_SIZE, 4096);
    }

    #[test]
    fn test_render_fullscreen_image_empty_data_is_noop() {
        let mut buf = Vec::new();
        render_fullscreen_image_inner(&mut buf, &[], false).unwrap();
        assert!(buf.is_empty(), "Empty data should produce no output");
    }

    #[test]
    fn test_render_fullscreen_image_writes_apc_sequence() {
        let mut buf = Vec::new();
        let png_data = vec![0x89, b'P', b'N', b'G']; // minimal data
        render_fullscreen_image_inner(&mut buf, &png_data, false).unwrap();

        let output = String::from_utf8_lossy(&buf);
        // Should contain APC start sequence
        assert!(output.contains("\x1b_G"), "Should contain Kitty APC start");
        // Should contain the protocol params
        assert!(output.contains("a=T"), "Should contain action=Transmit");
        assert!(output.contains("f=100"), "Should contain format=PNG");
        // Should contain APC end sequence
        assert!(output.contains("\x1b\\"), "Should contain APC terminator");
    }

    #[test]
    fn test_chunked_encoding_large_data() {
        // Create data that will encode to more than CHUNK_SIZE base64 chars
        let large_data = vec![0u8; CHUNK_SIZE]; // Will be >CHUNK_SIZE when base64 encoded
        let mut buf = Vec::new();
        render_fullscreen_image_inner(&mut buf, &large_data, false).unwrap();

        let output = String::from_utf8_lossy(&buf);
        // Should have m=1 (more data follows) in first chunk
        assert!(
            output.contains("m=1"),
            "Large data should use chunked encoding"
        );
        // Should have m=0 (last chunk)
        assert!(output.contains("m=0"), "Last chunk should have m=0");
    }

    #[test]
    fn test_write_kitty_apc_without_tmux() {
        let mut buf = Vec::new();
        write_kitty_apc(&mut buf, "a=T,f=100,c=10,r=5,m=0;AQID", false).unwrap();
        let output = String::from_utf8_lossy(&buf);
        assert_eq!(output, "\x1b_Ga=T,f=100,c=10,r=5,m=0;AQID\x1b\\");
    }

    #[test]
    fn test_write_kitty_apc_with_tmux_wrapping() {
        let mut buf = Vec::new();
        write_kitty_apc(&mut buf, "a=T,f=100,c=10,r=5,m=0;AQID", true).unwrap();
        let output = String::from_utf8_lossy(&buf);
        // Should be wrapped in DCS passthrough with doubled ESCs
        assert_eq!(
            output,
            "\x1bPtmux;\x1b\x1b_Ga=T,f=100,c=10,r=5,m=0;AQID\x1b\x1b\\\x1b\\"
        );
    }

    #[test]
    fn test_tmux_wrapped_fullscreen_image() {
        let mut buf = Vec::new();
        let png_data = vec![0x89, b'P', b'N', b'G'];
        render_fullscreen_image_inner(&mut buf, &png_data, true).unwrap();

        let output = String::from_utf8_lossy(&buf);
        // Should contain DCS passthrough start
        assert!(
            output.contains("\x1bPtmux;"),
            "Should contain DCS passthrough start"
        );
        // Should contain doubled ESC before APC
        assert!(
            output.contains("\x1b\x1b_G"),
            "Should contain doubled ESC before APC start"
        );
    }

    #[test]
    fn test_tmux_wrapped_chunked_image() {
        // Create data that will encode to more than CHUNK_SIZE base64 chars
        let large_data = vec![0u8; CHUNK_SIZE];
        let mut buf = Vec::new();
        render_fullscreen_image_inner(&mut buf, &large_data, true).unwrap();

        let output = String::from_utf8_lossy(&buf);
        // Each chunk should be individually wrapped in DCS passthrough
        let dcs_count = output.matches("\x1bPtmux;").count();
        assert!(
            dcs_count >= 2,
            "Each chunk should be wrapped in DCS passthrough, got {} wrappings",
            dcs_count
        );
    }

    #[test]
    fn test_is_inside_tmux_detection() {
        // Save original value
        let original = std::env::var("TMUX").ok();

        // SAFETY: env var mutation is not thread-safe; run with --test-threads=1
        unsafe {
            // With TMUX set, should detect tmux
            std::env::set_var("TMUX", "/tmp/tmux-1000/default,12345,0");
            assert!(is_inside_tmux(), "Should detect tmux when TMUX is set");

            // With TMUX removed, should not detect tmux
            std::env::remove_var("TMUX");
            assert!(
                !is_inside_tmux(),
                "Should not detect tmux when TMUX is unset"
            );

            // Restore original value
            if let Some(val) = original {
                std::env::set_var("TMUX", val);
            }
        }
    }
}
