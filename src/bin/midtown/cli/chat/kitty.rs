//! Kitty graphics protocol support for inline image rendering
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

use std::io::{self, Write};

use base64ct::{Base64, Encoding};
use crossterm::{cursor::MoveTo, execute};
use ratatui::prelude::CrosstermBackend;

/// An inline image to be rendered after ratatui draws
#[derive(Debug, Clone)]
pub struct InlineImage {
    /// Screen x coordinate (column)
    pub x: u16,
    /// Screen y coordinate (row)
    pub y: u16,
    /// Number of columns the image should occupy
    pub cols: u16,
    /// Number of rows the image should occupy
    pub rows: u16,
    /// PNG image data
    pub png_data: Vec<u8>,
}

/// Maximum bytes of base64 data per chunk (4096 is the Kitty protocol recommendation)
const CHUNK_SIZE: usize = 4096;

/// Render inline images using the Kitty graphics protocol
///
/// Writes APC escape sequences directly to the terminal, bypassing ratatui's
/// buffer system (which doesn't support image protocols).
///
/// Each image is placed at its specified (x, y) position with the given
/// column and row dimensions.
pub fn render_kitty_images<W: io::Write>(
    backend: &mut CrosstermBackend<W>,
    images: &[InlineImage],
) -> io::Result<()> {
    for image in images {
        render_single_image(backend, image)?;
    }
    backend.flush()?;
    Ok(())
}

/// Render a single inline image using the Kitty graphics protocol
fn render_single_image<W: io::Write>(
    backend: &mut CrosstermBackend<W>,
    image: &InlineImage,
) -> io::Result<()> {
    if image.png_data.is_empty() {
        return Ok(());
    }

    // Move cursor to the image position
    execute!(backend, MoveTo(image.x, image.y))?;

    // Encode the PNG data as base64
    let encoded = Base64::encode_string(&image.png_data);

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
        write!(
            backend,
            "\x1b_Ga=T,f=100,c={},r={},m=0;{}\x1b\\",
            image.cols, image.rows, chunks[0]
        )?;
    } else {
        // Multiple chunks: first chunk with params
        write!(
            backend,
            "\x1b_Ga=T,f=100,c={},r={},m=1;{}\x1b\\",
            image.cols, image.rows, chunks[0]
        )?;

        // Middle chunks (continuation)
        for chunk in &chunks[1..chunks.len() - 1] {
            write!(backend, "\x1b_Gm=1;{}\x1b\\", chunk)?;
        }

        // Last chunk
        write!(backend, "\x1b_Gm=0;{}\x1b\\", chunks[chunks.len() - 1])?;
    }

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
    fn test_inline_image_struct() {
        let image = InlineImage {
            x: 10,
            y: 5,
            cols: 40,
            rows: 15,
            png_data: vec![1, 2, 3],
        };
        assert_eq!(image.x, 10);
        assert_eq!(image.y, 5);
        assert_eq!(image.cols, 40);
        assert_eq!(image.rows, 15);
    }

    #[test]
    fn test_render_kitty_images_empty() {
        let mut buf = Vec::new();
        let mut backend = CrosstermBackend::new(&mut buf);
        let result = render_kitty_images(&mut backend, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_render_single_image_writes_apc_sequence() {
        let mut buf = Vec::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            let image = InlineImage {
                x: 0,
                y: 0,
                cols: 10,
                rows: 5,
                png_data: vec![0x89, b'P', b'N', b'G'], // minimal data
            };
            render_single_image(&mut backend, &image).unwrap();
            backend.flush().unwrap();
        }

        let output = String::from_utf8_lossy(&buf);
        // Should contain APC start sequence
        assert!(output.contains("\x1b_G"), "Should contain Kitty APC start");
        // Should contain the protocol params
        assert!(output.contains("a=T"), "Should contain action=Transmit");
        assert!(output.contains("f=100"), "Should contain format=PNG");
        assert!(output.contains("c=10"), "Should contain cols");
        assert!(output.contains("r=5"), "Should contain rows");
        // Should contain APC end sequence
        assert!(output.contains("\x1b\\"), "Should contain APC terminator");
    }

    #[test]
    fn test_render_empty_png_data_is_noop() {
        let mut buf = Vec::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            let image = InlineImage {
                x: 0,
                y: 0,
                cols: 10,
                rows: 5,
                png_data: vec![],
            };
            render_single_image(&mut backend, &image).unwrap();
            backend.flush().unwrap();
        }
        // Should only contain the cursor move, no APC sequence
        let output = String::from_utf8_lossy(&buf);
        assert!(!output.contains("\x1b_G"), "Empty data should skip APC");
    }

    #[test]
    fn test_chunked_encoding_large_data() {
        // Create data that will encode to more than CHUNK_SIZE base64 chars
        let large_data = vec![0u8; CHUNK_SIZE]; // Will be >CHUNK_SIZE when base64 encoded
        let mut buf = Vec::new();
        {
            let mut backend = CrosstermBackend::new(&mut buf);
            let image = InlineImage {
                x: 0,
                y: 0,
                cols: 10,
                rows: 5,
                png_data: large_data,
            };
            render_single_image(&mut backend, &image).unwrap();
            backend.flush().unwrap();
        }

        let output = String::from_utf8_lossy(&buf);
        // Should have m=1 (more data follows) in first chunk
        assert!(
            output.contains("m=1"),
            "Large data should use chunked encoding"
        );
        // Should have m=0 (last chunk)
        assert!(output.contains("m=0"), "Last chunk should have m=0");
    }
}
