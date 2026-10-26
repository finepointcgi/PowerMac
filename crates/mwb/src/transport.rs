//! Encrypted package framing on top of any byte stream
//! (Class/SocketStuff.cs `TcpSk`, `TcpSendData`, `TcpReceiveData`).

use std::io::{self, Read, Write};

use crate::crypto::{self, CbcDecryptor, CbcEncryptor, BLOCK, SALT_SIZE};
use crate::protocol::{self, Data, PACKAGE_SIZE};

/// Sending half: on creation writes the cleartext salt+IV header followed by one
/// encrypted random block (`SendOrReceiveARandomDataBlockPerInitialIV`).
pub struct SecureWriter<W: Write> {
    inner: W,
    enc: CbcEncryptor,
}

impl<W: Write> SecureWriter<W> {
    pub fn new(inner: W, key: &str) -> io::Result<Self> {
        let salt: [u8; SALT_SIZE] = crypto::random_bytes();
        let iv: [u8; BLOCK] = crypto::random_bytes();
        let first: [u8; BLOCK] = crypto::random_bytes();
        let dk = crypto::derive_key(key, &salt);
        Self::with_params(inner, &dk, &salt, &iv, &first)
    }

    /// Deterministic constructor (test vectors): explicit derived key, salt, IV and first block.
    pub fn with_params(mut inner: W, derived_key: &[u8; 32], salt: &[u8; 16], iv: &[u8; 16], first_block: &[u8; 16]) -> io::Result<Self> {
        let mut header = [0u8; 32];
        header[..16].copy_from_slice(salt);
        header[16..].copy_from_slice(iv);
        inner.write_all(&header)?;
        let mut enc = CbcEncryptor::new(derived_key, iv);
        let ct = enc.update(first_block);
        inner.write_all(&ct)?;
        inner.flush()?;
        Ok(SecureWriter { inner, enc })
    }

    /// Stamp magic/checksum (mutating a copy) and write the package's wire bytes.
    pub fn send(&mut self, d: &Data, magic: u32) -> io::Result<()> {
        let wire = d
            .wire_bytes()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "package type not set"))?;
        let mut bytes = wire.to_vec();
        protocol::stamp(&mut bytes, magic);
        let ct = self.enc.update(&bytes);
        self.inner.write_all(&ct)?;
        self.inner.flush()
    }

    pub fn get_ref(&self) -> &W {
        &self.inner
    }
}

/// Receiving half: reads the header, then discards the first decrypted block.
pub struct SecureReader<R: Read> {
    inner: R,
    dec: CbcDecryptor,
    plain: Vec<u8>,
}

/// Outcome of reading one package.
#[derive(Debug)]
pub struct Received {
    pub data: Data,
    /// Whether magic number + checksum validated (otherwise type is `Invalid`).
    pub valid: bool,
}

impl<R: Read> SecureReader<R> {
    pub fn new(mut inner: R, key: &str) -> io::Result<Self> {
        let mut header = [0u8; 32];
        inner.read_exact(&mut header)?;
        let dk = crypto::derive_key(key, &header[..16]);
        let iv: [u8; 16] = header[16..].try_into().unwrap();
        let mut r = SecureReader { inner, dec: CbcDecryptor::new(&dk, &iv), plain: Vec::new() };
        let mut first = [0u8; BLOCK];
        r.read_plain(&mut first)?;
        Ok(r)
    }

    fn read_plain(&mut self, out: &mut [u8]) -> io::Result<()> {
        while self.plain.len() < out.len() {
            let mut buf = [0u8; 4096];
            let n = self.inner.read(&mut buf)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
            }
            let p = self.dec.update(&buf[..n]);
            self.plain.extend_from_slice(&p);
        }
        out.copy_from_slice(&self.plain[..out.len()]);
        self.plain.drain(..out.len());
        Ok(())
    }

    /// `TcpReceiveData`: read 32 bytes, validate, and read 32 more if it is a big package.
    pub fn recv(&mut self, magic: u32) -> io::Result<Received> {
        let mut buf = [0u8; 64];
        self.read_plain(&mut buf[..PACKAGE_SIZE])?;
        let valid = protocol::validate_received(&mut buf, magic);
        let t = buf[0] as u32;
        match protocol::is_big_package(t) {
            None => return Err(io::Error::new(io::ErrorKind::InvalidData, "package type 0")),
            Some(true) => {
                let mut rest = [0u8; 32];
                self.read_plain(&mut rest)?;
                buf[32..].copy_from_slice(&rest);
            }
            Some(false) => {}
        }
        Ok(Received { data: Data::from_bytes(&buf), valid })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ptype;

    #[test]
    fn pipe_roundtrip() {
        let key = "abcdEFGH1234!@#$";
        let magic = crypto::get_24bit_hash(key);
        let mut out = Vec::new();
        {
            let mut w = SecureWriter::new(&mut out, key).unwrap();
            let mut d = Data::new(ptype::Hello);
            d.set_machine_name("HOST-A");
            d.set_id(7);
            w.send(&d, magic).unwrap();
            let mut m = Data::new(ptype::Mouse);
            m.set_md_x(1234);
            w.send(&m, magic).unwrap();
        }
        assert_eq!(out.len(), 32 + 16 + 64 + 32);
        let mut r = SecureReader::new(&out[..], key).unwrap();
        let a = r.recv(magic).unwrap();
        assert!(a.valid);
        assert_eq!(a.data.ptype(), ptype::Hello);
        assert_eq!(a.data.machine_name(), "HOST-A");
        let b = r.recv(magic).unwrap();
        assert_eq!(b.data.md_x(), 1234);
        assert!(r.recv(magic).is_err());
    }

    #[test]
    fn wrong_key_is_invalid() {
        let mut out = Vec::new();
        {
            let mut w = SecureWriter::new(&mut out, "keykeykeykeykey1").unwrap();
            let d = Data::new(ptype::Mouse);
            w.send(&d, crypto::get_24bit_hash("keykeykeykeykey1")).unwrap();
            w.send(&d, crypto::get_24bit_hash("keykeykeykeykey1")).unwrap();
        }
        let mut r = SecureReader::new(&out[..], "keykeykeykeykey2").unwrap();
        // Garbage decrypts: either invalid or type-0 error.
        match r.recv(crypto::get_24bit_hash("keykeykeykeykey2")) {
            Ok(p) => assert!(!p.valid),
            Err(_) => {}
        }
    }
}
