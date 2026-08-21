//! Classifying a response body by its `Content-Type`, and decoding the ones
//! that are images.
//!
//! Named `media` rather than `image` because a top-level `mod image` would be
//! ambiguous with the `image` crate at every `use image::…` in the tree.
//!
//! Pure functions over a header string and a byte slice, no IO — the same shape
//! as `curl.rs`, and for the same reason: the interesting rules here are the
//! ones that decide whether a body is shown as text or as pixels, and those are
//! worth testing without a server.

use anyhow::{bail, Result};
use image::{DynamicImage, ImageReader, Limits};
use std::io::Cursor;

/// The largest body we will hand to the decoder. A response is fully in memory
/// by the time we see it, but decoding expands it — a 4 MB PNG is ~130 MB of
/// RGBA at 6000×5000 — so the cap is on the *encoded* size as a cheap first
/// gate, with `Limits` below covering the expansion.
const MAX_ENCODED_BYTES: usize = 32 * 1024 * 1024;

/// Strict bounds handed to the decoder, in the spirit of yazi's own limits: a
/// malformed or hostile header must fail the decode rather than the process.
/// `max_alloc` is what actually catches a small file claiming enormous
/// dimensions, since the allocation happens before any pixel is read.
const MAX_PIXELS_PER_SIDE: u32 = 16_384;
const MAX_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

/// The bare type/subtype of a `Content-Type`, without parameters or space:
/// `image/png; charset=binary` → `image/png`.
fn essence(content_type: &str) -> &str {
    content_type.split(';').next().unwrap_or("").trim()
}

/// Whether a body with this `Content-Type` is an image we can decode and hand
/// to a terminal graphics protocol.
///
/// SVG is deliberately **not** one: it is markup, the decoder cannot read it,
/// and its source is more useful in the text pane than a failed decode would
/// be. Everything else under `image/` is worth attempting — an unknown subtype
/// still reaches `decode`, which guesses the format from the bytes and reports
/// an honest error if it is not one we support.
pub fn is_renderable_image(content_type: &str) -> bool {
    let essence = essence(content_type);
    // `get` rather than a slice: a `Content-Type` is attacker-controlled and
    // need not split on a char boundary at 6.
    essence.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("image/"))
        && essence.len() > 6
        && !essence.eq_ignore_ascii_case("image/svg+xml")
}

/// The line that stands in for an image in the response pane — `image/png ·
/// 1024×768 · 42.1 KB`.
///
/// It is the pane's body text in *every* image case, whether or not the image
/// renders beside it: with no graphics protocol it is the whole of what the
/// pane shows, and with one it is still the only part the filter and the
/// `V`/`y` copy have to work on.
pub fn summarize(content_type: &str, bytes: usize, dims: Option<(u32, u32)>) -> String {
    let mut out = essence(content_type).to_string();
    if let Some((w, h)) = dims {
        out.push_str(&format!(" · {w}×{h}"));
    }
    out.push_str(&format!(" · {}", human_size(bytes)));
    out
}

/// `pub(crate)` so `utils::format_run_summary` renders "4.2 KB" the same way
/// this line does. The two summaries sit in the same slot — an image response's
/// is this one with a run summary on the front — and a byte count that read
/// differently between them would be the first thing anyone noticed.
pub(crate) fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * KB;
    let n = bytes as f64;
    if n < KB {
        format!("{bytes} B")
    } else if n < MB {
        format!("{:.1} KB", n / KB)
    } else {
        format!("{:.1} MB", n / MB)
    }
}

/// Decodes an image body under explicit limits.
///
/// The format is guessed from the bytes rather than taken from the header,
/// because the header is what a server claims and the magic is what it sent.
pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    if bytes.len() > MAX_ENCODED_BYTES {
        bail!("image is {}, over the {} cap", human_size(bytes.len()), human_size(MAX_ENCODED_BYTES));
    }

    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PIXELS_PER_SIDE);
    limits.max_image_height = Some(MAX_PIXELS_PER_SIDE);
    limits.max_alloc = Some(MAX_ALLOC_BYTES);
    reader.limits(limits);

    Ok(reader.decode()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_image_types_are_renderable() {
        assert!(is_renderable_image("image/png"));
        assert!(is_renderable_image("image/jpeg"));
        assert!(is_renderable_image("image/gif"));
        assert!(is_renderable_image("image/webp"));
    }

    /// A `Content-Type` carries parameters and arbitrary case; the header is
    /// read the same way `runner.rs` reads it for `is_json`.
    #[test]
    fn parameters_and_case_are_ignored() {
        assert!(is_renderable_image("image/png; charset=binary"));
        assert!(is_renderable_image("IMAGE/PNG"));
        assert!(is_renderable_image("  image/png  "));
        assert!(is_renderable_image("Image/Png ;q=1"));
    }

    /// SVG is markup — it belongs in the text pane, not the decoder.
    #[test]
    fn svg_is_not_renderable() {
        assert!(!is_renderable_image("image/svg+xml"));
        assert!(!is_renderable_image("IMAGE/SVG+XML; charset=utf-8"));
    }

    #[test]
    fn non_images_are_not_renderable() {
        assert!(!is_renderable_image("application/json"));
        assert!(!is_renderable_image("text/plain"));
        assert!(!is_renderable_image(""));
        // The prefix alone is not a type.
        assert!(!is_renderable_image("image/"));
        // A type that merely contains "image/" somewhere is not one either.
        assert!(!is_renderable_image("application/vnd.image/png"));
    }

    /// A header need not split on a char boundary — this must not panic.
    #[test]
    fn multibyte_content_type_is_refused_not_panicked() {
        assert!(!is_renderable_image("iöage/png"));
        assert!(!is_renderable_image("日本語"));
    }

    #[test]
    fn summary_shows_type_dimensions_and_size() {
        assert_eq!(
            summarize("image/png", 43_112, Some((1024, 768))),
            "image/png · 1024×768 · 42.1 KB"
        );
    }

    /// Dimensions are absent when the decode failed or was never attempted.
    #[test]
    fn summary_without_dimensions() {
        assert_eq!(summarize("image/png; charset=binary", 512, None), "image/png · 512 B");
    }

    #[test]
    fn sizes_scale_by_unit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
    }

    /// A body that is not an image at all must report, not panic — this is the
    /// path a server mislabelling JSON as `image/png` takes.
    #[test]
    fn decoding_a_non_image_is_an_error() {
        assert!(decode(b"{\"not\":\"an image\"}").is_err());
    }

    #[test]
    fn decoding_a_real_png_yields_its_dimensions() {
        // Smallest possible PNG: 1×1, written out rather than fetched so the
        // test needs no fixture file.
        const PNG_1X1: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let img = decode(PNG_1X1).expect("1×1 png should decode");
        assert_eq!((img.width(), img.height()), (1, 1));
    }

    #[test]
    fn oversized_bodies_are_refused_before_decoding() {
        let huge = vec![0u8; MAX_ENCODED_BYTES + 1];
        let err = decode(&huge).unwrap_err().to_string();
        assert!(err.contains("cap"), "got: {err}");
    }
}
