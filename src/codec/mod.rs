//! Codecs written from scratch: CRC-32/Adler-32, inflate/deflate, base64,
//! SHA-1, PNG, JPEG, GIF.
//!
//! These exist because Kilat links no third-party crates: HTTP needs gzip to be
//! bandwidth-frugal, PNG is the screenshot/frame format, and the image decoders
//! are the whole reason `<img>` works on a phone build with no system libraries.

pub mod base64;
pub mod crc;
pub mod deflate;
pub mod gif;
pub mod inflate;
pub mod jpeg;
pub mod png;
pub mod sha1;

/// A decoded still image or animation, always 8-bit RGBA, straight (un-premultiplied).
#[derive(Clone, Debug)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// width*height*4 for each frame; `frames[0]` mirrors `rgba` for stills.
    pub rgba: Vec<u8>,
    pub frames: Vec<Frame>,
    /// Loop count for GIFs (0 = infinite, None = play once).
    pub loop_count: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub rgba: Vec<u8>,
    pub duration_ms: u32,
}

impl Decoded {
    pub fn still(width: u32, height: u32, rgba: Vec<u8>) -> Decoded {
        let frames = vec![Frame {
            rgba: rgba.clone(),
            duration_ms: 0,
        }];
        Decoded {
            width,
            height,
            rgba,
            frames,
            loop_count: None,
        }
    }

    pub fn animated(width: u32, height: u32, frames: Vec<Frame>, loop_count: Option<u16>) -> Decoded {
        let rgba = frames
            .first()
            .map(|f| f.rgba.clone())
            .unwrap_or_else(|| vec![0; (width * height * 4) as usize]);
        Decoded {
            width,
            height,
            rgba,
            frames,
            loop_count,
        }
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len().max(1)
    }

    pub fn frame(&self, index: usize) -> &[u8] {
        match self.frames.get(index % self.frame_count()) {
            Some(f) => &f.rgba,
            None => &self.rgba,
        }
    }

    /// Total pixels; used by the "too big to decode" budget check.
    pub fn pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }
}
