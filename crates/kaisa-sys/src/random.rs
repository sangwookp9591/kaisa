//! Unpredictable bytes from the operating system, for capability tokens.

use std::io;

/// Fills `buffer` with bytes from the operating system's cryptographic random source.
///
/// # Errors
///
/// Returns the I/O error if the source cannot be read.
#[cfg(unix)]
pub fn fill_random(buffer: &mut [u8]) -> io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buffer)
}

/// Fills `buffer` with bytes from the operating system's cryptographic random source.
///
/// # Errors
///
/// Returns the OS error if the system random number generator fails.
#[cfg(windows)]
pub fn fill_random(buffer: &mut [u8]) -> io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    for chunk in buffer.chunks_mut(u32::MAX as usize) {
        let length: u32 = u32::try_from(chunk.len()).unwrap_or(u32::MAX);
        #[allow(unsafe_code)]
        // SAFETY: `chunk` is a valid, writable buffer of exactly `length` bytes; a null
        // algorithm handle is what BCRYPT_USE_SYSTEM_PREFERRED_RNG requires.
        let status: i32 = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                chunk.as_mut_ptr(),
                length,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status < 0 {
            return Err(io::Error::other(format!(
                "BCryptGenRandom failed: {status:#x}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::fill_random;

    /// Long enough that two equal draws would mean the source is broken.
    const TOKEN_BYTES: usize = 32;

    #[test]
    fn two_draws_from_the_random_source_differ() {
        let mut first = [0_u8; TOKEN_BYTES];
        let mut second = [0_u8; TOKEN_BYTES];

        fill_random(&mut first).expect("the OS random source is readable");
        fill_random(&mut second).expect("the OS random source is readable");

        assert_ne!(first, second, "capability tokens must not repeat");
        assert_ne!(
            first, [0_u8; TOKEN_BYTES],
            "the buffer must actually be filled"
        );
    }
}
