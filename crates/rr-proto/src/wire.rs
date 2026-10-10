//! Little-endian primitives for frame bodies.

use crate::Error;

#[derive(Default)]
pub struct Writer(Vec<u8>);

impl Writer {
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
    pub fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i16(&mut self, v: i16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// A string: u16 length, UTF-8 bytes (cut at 65535 bytes, on a character boundary).
    pub fn str(&mut self, s: &str) {
        let mut end = s.len().min(u16::MAX as usize);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.u16(end as u16);
        self.0.extend_from_slice(&s.as_bytes()[..end]);
    }
    /// Bytes: u32 length, then the bytes (length-prefixed, so fields can still be added after them).
    pub fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
}

pub struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or(Error::Malformed("body too short"))?;
        let s = &self.buf[self.at..end];
        self.at = end;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn i16(&mut self) -> Result<i16, Error> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn str(&mut self) -> Result<String, Error> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| Error::Malformed("utf-8"))
    }
    pub fn bytes(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    /// Bytes left in the body: a field added in a later version is there only when this is non-zero.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.at
    }
    /// Fields added in a later version: the value if the sender's frame has it, else `default`
    /// (an older peer).
    pub fn or_u8(&mut self, default: u8) -> Result<u8, Error> {
        if self.remaining() == 0 {
            Ok(default)
        } else {
            self.u8()
        }
    }
    pub fn or_u16(&mut self, default: u16) -> Result<u16, Error> {
        if self.remaining() == 0 {
            Ok(default)
        } else {
            self.u16()
        }
    }
    pub fn or_u32(&mut self, default: u32) -> Result<u32, Error> {
        if self.remaining() == 0 {
            Ok(default)
        } else {
            self.u32()
        }
    }
    pub fn or_u64(&mut self, default: u64) -> Result<u64, Error> {
        if self.remaining() == 0 {
            Ok(default)
        } else {
            self.u64()
        }
    }
    pub fn or_str(&mut self, default: &str) -> Result<String, Error> {
        if self.remaining() == 0 {
            Ok(default.to_string())
        } else {
            self.str()
        }
    }
}
