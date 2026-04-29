//! VESA-style linear framebuffer.
//!
//! Two backends through the `Surface` trait:
//!
//!   * `LinearFb` — wraps a kernel-virtual pointer to MMIO video
//!     memory.  Used at runtime when the bootloader places us into a
//!     graphical mode (e.g. bootloader 0.9 + `vga_320x200` feature).
//!   * `MemSurface` — wraps a heap `Vec<u8>` so the test suite can
//!     assert on pixel patterns without any video hardware.
//!
//! The pixel model is "indexed 256-colour, one byte per pixel" to
//! match VGA mode 13h.  Higher-bpp framebuffers (32 bpp on real BIOS
//! VBE) would add another `LinearFb32` wrapper; the `Surface` trait
//! is intentionally pixel-format-agnostic apart from the index→RGB
//! mapping the renderer chooses.
//!
//! ## What's *not* here
//!
//! No bootloader-level VBE call sequence: bootloader 0.9's
//! `vga_320x200` does that for us when enabled.  Switching modes at
//! runtime from a long-mode kernel is doable (mode-13h register
//! sequence is well documented on osdev.org/VGA_Hardware) but lives
//! in a future `vga::set_mode_13h` once we want it on demand.
//!
//! No font ROM: we only rasterise a caller-supplied 8x8 bitmap —
//! pulling a real font (font8x8 / Cozette / IBM CGA dump) is the
//! consumer's job.  This keeps the module licence-clean and lets it
//! be tested with stub patterns.

use alloc::vec::Vec;

/// Common interface all framebuffer backends implement.  The renderer
/// only needs `width()`, `height()`, `put_pixel()`, and `read_pixel()`
/// — everything else is built on top.
pub trait Surface {
    fn width(&self)  -> u32;
    fn height(&self) -> u32;
    fn put_pixel(&mut self, x: u32, y: u32, color: u8);
    fn read_pixel(&self, x: u32, y: u32) -> u8;
}

// -----------------------------------------------------------------------------
// LinearFb — backed by a raw MMIO pointer.
// -----------------------------------------------------------------------------

/// Linear framebuffer at a fixed virtual address.  Owned by whoever
/// constructs it; concurrent access must be coordinated externally.
pub struct LinearFb {
    pub base: *mut u8,
    pub width: u32,
    pub height: u32,
    pub stride: u32, // bytes between adjacent rows; usually == width
}

unsafe impl Send for LinearFb {}
unsafe impl Sync for LinearFb {}

impl LinearFb {
    /// # Safety
    /// `base` must point at `stride * height` writable bytes that
    /// are a real video framebuffer or backing memory; nothing else
    /// must alias the same range while this `LinearFb` is alive.
    pub unsafe fn new(base: *mut u8, width: u32, height: u32, stride: u32) -> Self {
        Self { base, width, height, stride }
    }
    fn offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height { return None; }
        Some((y * self.stride + x) as usize)
    }
}

impl Surface for LinearFb {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }
    fn put_pixel(&mut self, x: u32, y: u32, color: u8) {
        if let Some(off) = self.offset(x, y) {
            unsafe { core::ptr::write_volatile(self.base.add(off), color); }
        }
    }
    fn read_pixel(&self, x: u32, y: u32) -> u8 {
        if let Some(off) = self.offset(x, y) {
            unsafe { core::ptr::read_volatile(self.base.add(off)) }
        } else { 0 }
    }
}

// -----------------------------------------------------------------------------
// MemSurface — heap-backed test fixture.
// -----------------------------------------------------------------------------

pub struct MemSurface {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl MemSurface {
    pub fn new(width: u32, height: u32) -> Self {
        let len = (width as usize) * (height as usize);
        let data = alloc::vec![0u8; len];
        Self { width, height, data }
    }
}

impl Surface for MemSurface {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }
    fn put_pixel(&mut self, x: u32, y: u32, color: u8) {
        if x >= self.width || y >= self.height { return; }
        let idx = (y * self.width + x) as usize;
        self.data[idx] = color;
    }
    fn read_pixel(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height { return 0; }
        let idx = (y * self.width + x) as usize;
        self.data[idx]
    }
}

// -----------------------------------------------------------------------------
// Drawing helpers — built on top of `Surface`.
// -----------------------------------------------------------------------------

pub fn clear<S: Surface>(s: &mut S, color: u8) {
    for y in 0..s.height() {
        for x in 0..s.width() {
            s.put_pixel(x, y, color);
        }
    }
}

pub fn fill_rect<S: Surface>(s: &mut S, x: u32, y: u32, w: u32, h: u32, color: u8) {
    for dy in 0..h {
        for dx in 0..w {
            s.put_pixel(x + dx, y + dy, color);
        }
    }
}

/// Draw a 1-pixel-thick rectangle outline.
pub fn rect<S: Surface>(s: &mut S, x: u32, y: u32, w: u32, h: u32, color: u8) {
    if w == 0 || h == 0 { return; }
    for dx in 0..w {
        s.put_pixel(x + dx, y, color);
        s.put_pixel(x + dx, y + h - 1, color);
    }
    for dy in 0..h {
        s.put_pixel(x, y + dy, color);
        s.put_pixel(x + w - 1, y + dy, color);
    }
}

/// Bresenham line draw.  Endpoints inclusive.
pub fn line<S: Surface>(s: &mut S, x0: i32, y0: i32, x1: i32, y1: i32, color: u8) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        if x >= 0 && y >= 0 {
            s.put_pixel(x as u32, y as u32, color);
        }
        if x == x1 && y == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x += sx; }
        if e2 <= dx { err += dx; y += sy; }
    }
}

/// Rasterise an 8x8 caller-provided glyph bitmap at `(x, y)`.  Each
/// byte of `glyph` is one row, MSB = leftmost pixel.  Set pixels
/// take `fg`; clear pixels take `bg` (use a colour-key value if you
/// want transparent backgrounds; the caller controls the convention).
pub fn glyph8x8<S: Surface>(s: &mut S, x: u32, y: u32,
                            glyph: &[u8; 8], fg: u8, bg: u8)
{
    for (row, &bits) in glyph.iter().enumerate() {
        for col in 0..8u32 {
            let on = (bits >> (7 - col)) & 1 != 0;
            s.put_pixel(x + col, y + row as u32, if on { fg } else { bg });
        }
    }
}

/// Rasterise a string by looking up each byte in a 256-entry 8x8
/// font table (`font[byte]` is one glyph).  Skips bytes < 0x20.
pub fn text8x8<S: Surface>(s: &mut S, x: u32, y: u32, text: &[u8],
                           font: &[[u8; 8]; 256], fg: u8, bg: u8)
{
    let mut cursor_x = x;
    for &b in text {
        if b == b'\n' { continue; }
        glyph8x8(s, cursor_x, y, &font[b as usize], fg, bg);
        cursor_x += 8;
    }
}
