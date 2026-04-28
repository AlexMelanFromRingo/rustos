//! SHA-256 implementation following FIPS 180-4.
//!
//! Computes a 256-bit digest from arbitrary-length input.  Used for
//! password hashing and as a building block for keyed authentication.
//! No constant-time comparison guarantee at the algorithm level — callers
//! that compare digests against attacker-supplied input should use
//! [`constant_time_eq`] from this module.

use alloc::string::String;

/// Constants from FIPS 180-4 §4.2.2 — the first 32 bits of the fractional
/// parts of the cube roots of the first 64 primes.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
    0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial hash value H(0) — first 32 bits of the fractional parts of the
/// square roots of the first 8 primes.  FIPS 180-4 §5.3.3.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[inline] fn rotr(x: u32, n: u32) -> u32 { x.rotate_right(n) }
#[inline] fn ch (x: u32, y: u32, z: u32) -> u32 { (x & y) ^ (!x & z) }
#[inline] fn maj(x: u32, y: u32, z: u32) -> u32 { (x & y) ^ (x & z) ^ (y & z) }
#[inline] fn big_sigma0(x: u32) -> u32 { rotr(x, 2)  ^ rotr(x, 13) ^ rotr(x, 22) }
#[inline] fn big_sigma1(x: u32) -> u32 { rotr(x, 6)  ^ rotr(x, 11) ^ rotr(x, 25) }
#[inline] fn small_sigma0(x: u32) -> u32 { rotr(x, 7)  ^ rotr(x, 18) ^ (x >> 3) }
#[inline] fn small_sigma1(x: u32) -> u32 { rotr(x, 17) ^ rotr(x, 19) ^ (x >> 10) }

/// Streaming SHA-256 hasher.
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_bits: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 { h: H0, buf: [0u8; 64], buf_len: 0, total_bits: 0 }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut data = data;
        self.total_bits = self.total_bits.wrapping_add((data.len() as u64) * 8);

        // If we have a partial buffer, fill it first.
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = data.len().min(need);
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }

        // Whole blocks.
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }

        // Tail.
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            w[i] = small_sigma1(w[i - 2])
                .wrapping_add(w[i - 7])
                .wrapping_add(small_sigma0(w[i - 15]))
                .wrapping_add(w[i - 16]);
        }
        let mut a = self.h[0];
        let mut b = self.h[1];
        let mut c = self.h[2];
        let mut d = self.h[3];
        let mut e = self.h[4];
        let mut f = self.h[5];
        let mut g = self.h[6];
        let mut h = self.h[7];
        for i in 0..64 {
            let t1 = h.wrapping_add(big_sigma1(e))
                      .wrapping_add(ch(e, f, g))
                      .wrapping_add(K[i])
                      .wrapping_add(w[i]);
            let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(h);
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let total_bits = self.total_bits;

        // Pad: 0x80, then zero bytes, then 8-byte big-endian length.
        let mut tail = [0u8; 128];
        let used = self.buf_len;
        tail[..used].copy_from_slice(&self.buf[..used]);
        tail[used] = 0x80;
        let len_pos;
        if used + 1 + 8 <= 64 {
            len_pos = 64 - 8;
        } else {
            len_pos = 128 - 8;
        }
        tail[len_pos..len_pos + 8].copy_from_slice(&total_bits.to_be_bytes());

        let mut block = [0u8; 64];
        block.copy_from_slice(&tail[..64]);
        self.compress(&block);
        if len_pos == 128 - 8 {
            block.copy_from_slice(&tail[64..128]);
            self.compress(&block);
        }

        let mut out = [0u8; 32];
        for i in 0..8 {
            out[i * 4..i * 4 + 4].copy_from_slice(&self.h[i].to_be_bytes());
        }
        out
    }
}

/// One-shot helper.
pub fn hash(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// Lower-case hex encoding of a 32-byte digest.
pub fn hex(digest: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push(hex_nibble((b >> 4) & 0xF));
        s.push(hex_nibble(b & 0xF));
    }
    s
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + n - 10) as char,
        _ => '?',
    }
}

/// Constant-time equality test for two equal-length byte slices.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        // SHA-256("") = e3b0c442 98fc1c14 9afbf4c8 996fb924 27ae41e4 649b934c a495991b 7852b855
        let d = hash(b"");
        assert_eq!(hex(&d),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn abc() {
        // SHA-256("abc") = ba7816bf 8f01cfea 414140de 5dae2223 b00361a3 96177a9c b410ff61 f20015ad
        let d = hash(b"abc");
        assert_eq!(hex(&d),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn two_block() {
        // FIPS 180-4 §6.2.3 — 56-char input crosses the 448-bit padding
        // boundary so finalize() emits two compression blocks.
        let d = hash(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
        assert_eq!(hex(&d),
            "248d6a61d20638b8e5c026930c3e6039a33ce459964ff2167f6ecedd419db06c1");
    }
}
