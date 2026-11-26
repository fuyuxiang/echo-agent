//! Binary crash blob format ("GCRX").
//!
//! The signal handler writes this format using only `libc::write` (no allocation).
//! The startup reader parses it in normal Rust context.

/// Magic bytes identifying a valid crash file.
pub const MAGIC: [u8; 4] = *b"GCRX";

/// Current format version.
pub const VERSION: u8 = 1;

/// Maximum backtrace frames captured in the signal handler.
pub const MAX_FRAMES: usize = 64;

/// Length of the null-padded version string field.
pub const VERSION_STRING_LEN: usize = 32;

/// Fixed header size (before the variable-length frames array).
///
/// Layout:
/// - magic:        4 bytes
/// - version:      1 byte
/// - signal:       1 byte
/// - si_code:      4 bytes (i32, little-endian)
/// - si_addr:      8 bytes (u64, little-endian)
/// - pid:          4 bytes (u32, little-endian)
/// - timestamp:    8 bytes (u64, little-endian)
/// - n_frames:     2 bytes (u16, little-endian)
/// - app_version: 32 bytes (null-padded UTF-8)
pub const HEADER_SIZE: usize = 4 + 1 + 1 + 4 + 8 + 4 + 8 + 2 + VERSION_STRING_LEN;

/// Total maximum file size: header + 64 frames * 8 bytes each.
pub const MAX_FILE_SIZE: usize = HEADER_SIZE + MAX_FRAMES * 8;

/// Parsed crash data from a `last-crash.bin` file.
#[derive(Debug, Clone)]
pub struct CrashBlob {
    pub signal: u8,
    pub si_code: i32,
    pub si_addr: u64,
    pub pid: u32,
    pub timestamp: u64,
    pub frames: Vec<usize>,
    pub app_version: String,
}

impl CrashBlob {
    /// Parse a crash blob from bytes. Returns `None` if the data is invalid.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < HEADER_SIZE {
            return None;
        }
        if data[0..4] != MAGIC {
            return None;
        }
        if data[4] != VERSION {
            return None;
        }

        let signal = data[5];
        let si_code = i32::from_le_bytes([data[6], data[7], data[8], data[9]]);
        let si_addr = u64::from_le_bytes([
            data[10], data[11], data[12], data[13], data[14], data[15], data[16], data[17],
        ]);
        let pid = u32::from_le_bytes([data[18], data[19], data[20], data[21]]);
        let timestamp = u64::from_le_bytes([
            data[22], data[23], data[24], data[25], data[26], data[27], data[28], data[29],
        ]);
        let n_frames = u16::from_le_bytes([data[30], data[31]]) as usize;

        let version_bytes = &data[32..32 + VERSION_STRING_LEN];
        let app_version = std::str::from_utf8(version_bytes)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();

        if n_frames > MAX_FRAMES {
            return None;
        }
        let frames_start = HEADER_SIZE;
        let frames_end = frames_start + n_frames * 8;
        if data.len() < frames_end {
            return None;
        }

        let mut frames = Vec::with_capacity(n_frames);
        for i in 0..n_frames {
            let offset = frames_start + i * 8;
            let addr = u64::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);
            frames.push(addr as usize);
        }

        Some(CrashBlob {
            signal,
            si_code,
            si_addr,
            pid,
            timestamp,
            frames,
            app_version,
        })
    }
}

/// Helpers for writing fields in the signal handler using raw byte copies.
/// These are used by `handler.rs` — all operations are on a pre-allocated
/// static buffer, no allocation involved.
pub mod writer {
    use super::{MAGIC, VERSION, VERSION_STRING_LEN};

    /// Write the crash blob header into `buf`, returning the number of bytes written.
    /// The caller must ensure `buf` is at least `HEADER_SIZE` bytes.
    ///
    /// # Safety
    ///
    /// This is called from a signal handler. The buffer must be valid and large enough.
    pub unsafe fn write_header(
        buf: &mut [u8],
        signal: u8,
        si_code: i32,
        si_addr: u64,
        pid: u32,
        timestamp: u64,
        n_frames: u16,
        app_version: &[u8],
    ) -> usize {
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = VERSION;
        buf[5] = signal;
        buf[6..10].copy_from_slice(&si_code.to_le_bytes());
        buf[10..18].copy_from_slice(&si_addr.to_le_bytes());
        buf[18..22].copy_from_slice(&pid.to_le_bytes());
        buf[22..30].copy_from_slice(&timestamp.to_le_bytes());
        buf[30..32].copy_from_slice(&n_frames.to_le_bytes());

        // Null-pad the version string field.
        let version_field = &mut buf[32..32 + VERSION_STRING_LEN];
        version_field.fill(0);
        let copy_len = app_version.len().min(VERSION_STRING_LEN);
        version_field[..copy_len].copy_from_slice(&app_version[..copy_len]);

        32 + VERSION_STRING_LEN
    }

    /// Write a single frame pointer into `buf` at the given offset.
    /// Returns the new offset.
    ///
    /// # Safety
    ///
    /// The caller must ensure `buf[offset..offset+8]` is valid.
    pub unsafe fn write_frame(buf: &mut [u8], offset: usize, addr: usize) -> usize {
        buf[offset..offset + 8].copy_from_slice(&(addr as u64).to_le_bytes());
        offset + 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_crash_blob() {
        let mut buf = [0u8; MAX_FILE_SIZE];
        let version = b"0.1.169-alpha.2";
        let frames: &[usize] = &[0xdead_beef, 0xcafe_babe, 0x1234_5678];

        unsafe {
            let mut offset = writer::write_header(
                &mut buf,
                10, // SIGBUS on macOS
                2,  // BUS_ADRERR
                0x7f8a_1234_0000,
                42,
                1_712_678_587,
                frames.len() as u16,
                version,
            );
            for &frame in frames {
                offset = writer::write_frame(&mut buf, offset, frame);
            }

            let blob = CrashBlob::parse(&buf[..offset]).expect("parse should succeed");
            assert_eq!(blob.signal, 10);
            assert_eq!(blob.si_code, 2);
            assert_eq!(blob.si_addr, 0x7f8a_1234_0000);
            assert_eq!(blob.pid, 42);
            assert_eq!(blob.timestamp, 1_712_678_587);
            assert_eq!(blob.frames, frames);
            assert_eq!(blob.app_version, "0.1.169-alpha.2");
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(b"NOPE");
        assert!(CrashBlob::parse(&buf).is_none());
    }

    #[test]
    fn rejects_truncated_data() {
        assert!(CrashBlob::parse(&[]).is_none());
        assert!(CrashBlob::parse(&MAGIC).is_none());
    }
}
