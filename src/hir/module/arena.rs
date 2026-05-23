use std::cell::RefCell;

/// An arena that holds heap-allocated source code strings
pub(crate) struct SourceArena {
    sources: RefCell<Vec<Box<str>>>,
}

impl SourceArena {
    pub fn new() -> Self {
        Self {
            sources: RefCell::new(Vec::new()),
        }
    }

    /// Allocates a string in the arena, returning a reference bound to the lifetime of `self`.
    pub fn alloc(&self, src: String) -> &str {
        let boxed = src.into_boxed_str();
        let ptr = boxed.as_ptr();
        let len = boxed.len();
        self.sources.borrow_mut().push(boxed);

        // SAFETY: The `Box<str>` is allocated on the heap. Even if `sources` (the vector of Boxes)
        // reallocates, the actual character buffers pointed to by the boxes never move.
        // Therefore, we can cast the raw pointer to a reference with the lifetime of `self`.
        unsafe {
            let slice = std::slice::from_raw_parts(ptr, len);
            std::str::from_utf8_unchecked(slice)
        }
    }
}
