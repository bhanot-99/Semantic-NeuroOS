//! memfd creation + shared mmap.
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;

use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};

pub fn create_memfd(name: &str, size: usize) -> io::Result<OwnedFd> {
    let fd = memfd_create(name, MemfdFlags::CLOEXEC)?;
    ftruncate(&fd, size as u64)?;
    Ok(fd)
}

pub fn fd_size(fd: BorrowedFd) -> io::Result<usize> {
    let stat = rustix::fs::fstat(fd)?;
    Ok(stat.st_size as usize)
}

/// A shared (`MAP_SHARED`) mapping of `len` bytes of `fd`. Unmapped on drop.
pub struct SharedMap {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is backed by shared memory intended for concurrent
// access from multiple threads/processes; all access to the mapped bytes
// goes through atomics or is externally synchronized by the seqlock
// protocol in `ring.rs`.
unsafe impl Send for SharedMap {}
unsafe impl Sync for SharedMap {}

impl SharedMap {
    pub fn new(fd: BorrowedFd, len: usize) -> io::Result<Self> {
        // SAFETY: `fd` is a valid, open file descriptor with at least `len`
        // bytes (callers size the memfd with `ftruncate` before mapping),
        // and we own the returned mapping until `Drop::drop` unmaps it.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                fd,
                0,
            )
        }?;
        let ptr = NonNull::new(ptr as *mut u8)
            .ok_or_else(|| io::Error::other("mmap returned a null pointer on success"))?;
        Ok(Self { ptr, len })
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for SharedMap {
    fn drop(&mut self) {
        // SAFETY: this mapping was created by `mmap` in `new` with the same
        // pointer and length, and is not used again after this point.
        unsafe {
            let _ = munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}

pub fn memfd_as_fd(fd: &OwnedFd) -> BorrowedFd<'_> {
    fd.as_fd()
}
