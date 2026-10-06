use core::fmt;

#[derive(Debug)]
pub struct FixedBuf<const N: usize> {
    buf: [u8; N],
    pos: usize,
}

impl<const N: usize> Default for FixedBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> FixedBuf<N> {
    #[inline]
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            pos: 0,
        }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.buf[..self.pos]) }
    }

    #[inline]
    pub const fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub const fn remaining(&self) -> usize {
        N - self.pos
    }

    #[inline]
    pub const fn is_full(&self) -> bool {
        self.pos >= N
    }

    #[inline]
    pub const fn clear(&mut self) {
        self.pos = 0;
    }
}

impl<const N: usize> fmt::Write for FixedBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let available = N - self.pos;

        if bytes.len() > available {
            Err(fmt::Error)
        } else {
            self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
            self.pos += bytes.len();
            Ok(())
        }
    }
}

impl<const N: usize> fmt::Display for FixedBuf<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<const N: usize> AsRef<str> for FixedBuf<N> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
