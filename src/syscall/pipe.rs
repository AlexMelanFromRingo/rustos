/// Kernel pipe implementation
///
/// A pipe is a unidirectional data channel with a fixed-size kernel buffer.
/// One end is for reading, the other for writing.

use alloc::vec::Vec;
use spin::Mutex;

/// Pipe buffer size (4 KiB, same as Linux default for atomic writes)
const PIPE_BUF_SIZE: usize = 4096;

/// Maximum number of concurrent pipes
const MAX_PIPES: usize = 64;

/// A single kernel pipe
pub struct Pipe {
    buffer: Vec<u8>,
    read_pos: usize,
    write_pos: usize,
    count: usize,           // Number of bytes in buffer
    read_open: bool,        // Read end is still open
    write_open: bool,       // Write end is still open
}

impl Pipe {
    fn new() -> Self {
        let mut buffer = Vec::with_capacity(PIPE_BUF_SIZE);
        buffer.resize(PIPE_BUF_SIZE, 0);
        Pipe {
            buffer,
            read_pos: 0,
            write_pos: 0,
            count: 0,
            read_open: true,
            write_open: true,
        }
    }

    /// Read up to `count` bytes from the pipe. Returns number of bytes read.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        let to_read = buf.len().min(self.count);
        for i in 0..to_read {
            buf[i] = self.buffer[self.read_pos];
            self.read_pos = (self.read_pos + 1) % PIPE_BUF_SIZE;
        }
        self.count -= to_read;
        to_read
    }

    /// Write up to `count` bytes to the pipe. Returns number of bytes written.
    pub fn write(&mut self, data: &[u8]) -> usize {
        let available = PIPE_BUF_SIZE - self.count;
        let to_write = data.len().min(available);
        for i in 0..to_write {
            self.buffer[self.write_pos] = data[i];
            self.write_pos = (self.write_pos + 1) % PIPE_BUF_SIZE;
        }
        self.count += to_write;
        to_write
    }

    /// Number of bytes available for reading
    pub fn available(&self) -> usize {
        self.count
    }

    /// Close the read end
    pub fn close_read(&mut self) {
        self.read_open = false;
    }

    /// Close the write end
    pub fn close_write(&mut self) {
        self.write_open = false;
    }

    /// Check if pipe is fully closed (both ends)
    pub fn is_closed(&self) -> bool {
        !self.read_open && !self.write_open
    }

    /// Check if write end is closed (EOF for readers)
    pub fn write_closed(&self) -> bool {
        !self.write_open
    }

    /// Check if read end is closed (EPIPE for writers)
    pub fn read_closed(&self) -> bool {
        !self.read_open
    }
}

/// Pipe identifier
pub type PipeId = usize;

/// Global pipe table
struct PipeTable {
    pipes: Vec<Option<Pipe>>,
}

impl PipeTable {
    const fn new() -> Self {
        PipeTable {
            pipes: Vec::new(),
        }
    }

    /// Create a new pipe, returns its ID
    fn create(&mut self) -> Option<PipeId> {
        // Find a free slot
        for (i, slot) in self.pipes.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(Pipe::new());
                return Some(i);
            }
        }
        // No free slot, try to allocate a new one
        if self.pipes.len() < MAX_PIPES {
            self.pipes.push(Some(Pipe::new()));
            return Some(self.pipes.len() - 1);
        }
        None
    }

    /// Get a mutable reference to a pipe
    fn get_mut(&mut self, id: PipeId) -> Option<&mut Pipe> {
        self.pipes.get_mut(id).and_then(|slot| slot.as_mut())
    }

    /// Remove a pipe if both ends are closed
    fn cleanup(&mut self, id: PipeId) {
        if let Some(Some(pipe)) = self.pipes.get(id) {
            if pipe.is_closed() {
                self.pipes[id] = None;
            }
        }
    }
}

static PIPE_TABLE: Mutex<PipeTable> = Mutex::new(PipeTable::new());

/// Create a new pipe. Returns (pipe_id) or None on failure.
pub fn create_pipe() -> Option<PipeId> {
    PIPE_TABLE.lock().create()
}

/// Read from a pipe
pub fn pipe_read(id: PipeId, buf: &mut [u8]) -> Result<usize, PipeError> {
    let mut table = PIPE_TABLE.lock();
    let pipe = table.get_mut(id).ok_or(PipeError::InvalidPipe)?;

    if pipe.available() == 0 {
        if pipe.write_closed() {
            return Ok(0); // EOF
        }
        return Err(PipeError::WouldBlock);
    }

    Ok(pipe.read(buf))
}

/// Write to a pipe
pub fn pipe_write(id: PipeId, data: &[u8]) -> Result<usize, PipeError> {
    let mut table = PIPE_TABLE.lock();
    let pipe = table.get_mut(id).ok_or(PipeError::InvalidPipe)?;

    if pipe.read_closed() {
        return Err(PipeError::BrokenPipe);
    }

    let written = pipe.write(data);
    if written == 0 && !data.is_empty() {
        return Err(PipeError::WouldBlock);
    }

    Ok(written)
}

/// Number of bytes currently buffered in the pipe.  Returns 0 if invalid.
pub fn pipe_bytes_available(id: PipeId) -> usize {
    let table = PIPE_TABLE.lock();
    table.pipes.get(id)
        .and_then(|s| s.as_ref())
        .map(|p| p.available())
        .unwrap_or(0)
}

/// True if the pipe has space for at least one byte of write.
pub fn pipe_writable(id: PipeId) -> bool {
    let table = PIPE_TABLE.lock();
    match table.pipes.get(id).and_then(|s| s.as_ref()) {
        Some(p) => p.available() < PIPE_BUF_SIZE && !p.read_closed(),
        None => false,
    }
}

/// True if the write end has been closed (read returns 0/EOF).
pub fn pipe_write_closed(id: PipeId) -> bool {
    let table = PIPE_TABLE.lock();
    table.pipes.get(id)
        .and_then(|s| s.as_ref())
        .map(|p| p.write_closed())
        .unwrap_or(true)
}

/// True if the read end has been closed (writes get EPIPE).
pub fn pipe_read_closed(id: PipeId) -> bool {
    let table = PIPE_TABLE.lock();
    table.pipes.get(id)
        .and_then(|s| s.as_ref())
        .map(|p| p.read_closed())
        .unwrap_or(true)
}

/// Close the read end of a pipe
pub fn pipe_close_read(id: PipeId) {
    let mut table = PIPE_TABLE.lock();
    if let Some(pipe) = table.get_mut(id) {
        pipe.close_read();
    }
    table.cleanup(id);
}

/// Close the write end of a pipe
pub fn pipe_close_write(id: PipeId) {
    let mut table = PIPE_TABLE.lock();
    if let Some(pipe) = table.get_mut(id) {
        pipe.close_write();
    }
    table.cleanup(id);
}

/// Pipe errors
#[derive(Debug)]
pub enum PipeError {
    InvalidPipe,
    WouldBlock,
    BrokenPipe,
}
