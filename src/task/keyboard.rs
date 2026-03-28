use conquer_once::spin::OnceCell;
use crossbeam_queue::ArrayQueue;
use core::{pin::Pin, task::{Poll, Context}};
use futures_util::stream::Stream;
use futures_util::task::AtomicWaker;

pub static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static WAKER: AtomicWaker = AtomicWaker::new();

/// Serial input queue — receives decoded ASCII bytes from COM1 interrupt
pub static SERIAL_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();

/// Input event from either PS/2 keyboard or serial port
pub enum InputEvent {
    /// PS/2 scancode (needs decoding via pc_keyboard crate)
    Scancode(u8),
    /// Serial byte (already decoded ASCII)
    SerialByte(u8),
}

/// Called by the serial interrupt handler
pub(crate) fn add_serial_byte(byte: u8) {
    if let Ok(queue) = SERIAL_QUEUE.try_get() {
        let _ = queue.push(byte);
        WAKER.wake();
    }
}

/// Called by the keyboard interrupt handler
///
/// Must not block or allocate.
pub(crate) fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if queue.push(scancode).is_err() {
            use crate::println;
            println!("WARNING: scancode queue full; dropping keyboard input");
        } else {
            WAKER.wake();
        }
    } else {
        use crate::println;
        println!("WARNING: scancode queue uninitialized");
    }
}

pub struct InputStream {
    _private: (),
}

impl InputStream {
    pub fn new() -> Self {
        SCANCODE_QUEUE.try_init_once(|| ArrayQueue::new(100))
            .expect("InputStream::new should only be called once");
        SERIAL_QUEUE.try_init_once(|| ArrayQueue::new(100))
            .expect("serial queue init failed");
        InputStream { _private: () }
    }
}

impl Stream for InputStream {
    type Item = InputEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<InputEvent>> {
        // Check serial queue first (serial input is pre-decoded)
        if let Ok(serial_queue) = SERIAL_QUEUE.try_get() {
            if let Some(byte) = serial_queue.pop() {
                return Poll::Ready(Some(InputEvent::SerialByte(byte)));
            }
        }

        // Check PS/2 scancode queue
        let queue = SCANCODE_QUEUE
            .try_get()
            .expect("scancode queue not initialized");

        if let Some(scancode) = queue.pop() {
            return Poll::Ready(Some(InputEvent::Scancode(scancode)));
        }

        WAKER.register(&cx.waker());

        // Double-check both queues after registering waker
        if let Ok(serial_queue) = SERIAL_QUEUE.try_get() {
            if let Some(byte) = serial_queue.pop() {
                WAKER.take();
                return Poll::Ready(Some(InputEvent::SerialByte(byte)));
            }
        }

        match queue.pop() {
            Some(scancode) => {
                WAKER.take();
                Poll::Ready(Some(InputEvent::Scancode(scancode)))
            }
            None => Poll::Pending,
        }
    }
}
