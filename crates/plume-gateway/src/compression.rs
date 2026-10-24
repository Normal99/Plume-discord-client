//! `compress=zlib-stream`: the whole connection is one zlib stream, and each
//! gateway message ends with a sync flush (`00 00 ff ff`). One message can
//! arrive split over several WebSocket frames.
//!
//! This roughly quarters READY's size on the wire. The inflater keeps its
//! 32 KB window for the life of the connection, which is what makes later
//! messages small.

use flate2::{Decompress, FlushDecompress, Status};

const SUFFIX: [u8; 4] = [0x00, 0x00, 0xff, 0xff];
/// Buffers grown past this (by READY, usually) are shrunk back afterwards,
/// so one big message doesn't pin megabytes for the whole session.
const KEEP_CAPACITY: usize = 64 * 1024;

pub struct Inflater {
    zlib: Decompress,
    input: Vec<u8>,
    output: Vec<u8>,
}

impl Default for Inflater {
    fn default() -> Self {
        Inflater {
            zlib: Decompress::new(true),
            input: Vec::new(),
            output: Vec::new(),
        }
    }
}

impl Inflater {
    /// Feeds one binary frame. Returns the decompressed message once it's
    /// complete, or `None` if more frames are needed.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<&[u8]>, flate2::DecompressError> {
        self.input.extend_from_slice(frame);
        if !self.input.ends_with(&SUFFIX) {
            return Ok(None);
        }

        self.output.clear();
        if self.output.capacity() > KEEP_CAPACITY {
            self.output.shrink_to(KEEP_CAPACITY);
        }
        let mut consumed = 0;
        loop {
            if self.output.len() == self.output.capacity() {
                // Compressed gateway JSON is usually 4-8x smaller.
                self.output.reserve((self.input.len() * 4).max(4096));
            }
            let (before_in, before_out) = (self.zlib.total_in(), self.zlib.total_out());
            let status = self
                .zlib
                .decompress_vec(&self.input[consumed..], &mut self.output, FlushDecompress::Sync)?;
            consumed += (self.zlib.total_in() - before_in) as usize;
            let progressed = self.zlib.total_in() != before_in || self.zlib.total_out() != before_out;
            let has_room = self.output.len() < self.output.capacity();
            // Done when all input is used and zlib had room to spare (so it
            // isn't holding back output). No progress with room means the
            // same: nothing more will come out.
            if status == Status::StreamEnd || (has_room && (consumed == self.input.len() || !progressed)) {
                break;
            }
        }

        self.input.clear();
        if self.input.capacity() > KEEP_CAPACITY {
            self.input.shrink_to(KEEP_CAPACITY);
        }
        Ok(Some(&self.output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    /// Compresses one message the way the gateway does: same stream every
    /// time, with a sync flush at the end.
    fn compress(zlib: &mut Compress, message: &[u8]) -> Vec<u8> {
        let start = zlib.total_in();
        let mut out = Vec::with_capacity(64);
        loop {
            let done = (zlib.total_in() - start) as usize;
            zlib.compress_vec(&message[done..], &mut out, FlushCompress::Sync)
                .unwrap();
            let all_in = (zlib.total_in() - start) as usize == message.len();
            if all_in && out.len() < out.capacity() && out.ends_with(&SUFFIX) {
                return out;
            }
            out.reserve(1024);
        }
    }

    #[test]
    fn messages_share_one_stream() {
        let mut zlib = Compress::new(Compression::default(), true);
        let mut inflater = Inflater::default();
        for text in [r#"{"op":10}"#, r#"{"op":11}"#, r#"{"op":0,"t":"READY"}"#] {
            let frame = compress(&mut zlib, text.as_bytes());
            assert_eq!(inflater.push(&frame).unwrap(), Some(text.as_bytes()));
        }
    }

    #[test]
    fn a_message_split_over_frames() {
        let mut zlib = Compress::new(Compression::default(), true);
        let mut inflater = Inflater::default();
        let frame = compress(&mut zlib, b"hello hello hello");
        let (first, second) = frame.split_at(frame.len() / 2);
        assert_eq!(inflater.push(first).unwrap(), None);
        assert_eq!(inflater.push(second).unwrap(), Some(&b"hello hello hello"[..]));
    }

    #[test]
    fn big_messages_decompress_fully_and_buffers_shrink_after() {
        let mut zlib = Compress::new(Compression::default(), true);
        let mut inflater = Inflater::default();
        // ~2 MB of not-very-compressible JSON-ish text, like a big READY.
        let big: String = (0..200_000u32)
            .map(|i| format!("{},", i.wrapping_mul(2_654_435_761)))
            .collect();
        let frame = compress(&mut zlib, big.as_bytes());
        assert_eq!(inflater.push(&frame).unwrap().map(<[u8]>::len), Some(big.len()));

        let small = compress(&mut zlib, b"{}");
        assert_eq!(inflater.push(&small).unwrap(), Some(&b"{}"[..]));
        assert!(inflater.output.capacity() <= KEEP_CAPACITY);
        assert!(inflater.input.capacity() <= KEEP_CAPACITY);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut inflater = Inflater::default();
        assert!(inflater.push(&[1, 2, 3, 0, 0, 0xff, 0xff]).is_err());
    }
}
