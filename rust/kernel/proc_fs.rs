//! Implementation of proc_fs functionality
//!

use core::option::Option;

use bindings::{file, inode, loff_t};

use kernel::error::Result;

use crate::prelude::{EINVAL, ENOMEM};

/// Proc options builder
pub struct ProcOpsBuilder {
    flags: u32,
    proc_open: Option<unsafe extern "C" fn(arg1: *mut inode, arg2: *mut file) -> core::ffi::c_int>,
    proc_read: Option<
        unsafe extern "C" fn(
            arg1: *mut file,
            arg2: *mut core::ffi::c_char,
            arg3: usize,
            arg4: *mut loff_t,
        ) -> isize,
    >,
    proc_write: Option<
        unsafe extern "C" fn(
            arg1: *mut file,
            arg2: *const core::ffi::c_char,
            arg3: usize,
            arg4: *mut loff_t,
        ) -> isize,
    >,
    proc_lseek: Option<
        unsafe extern "C" fn(arg1: *mut file, arg2: loff_t, arg3: core::ffi::c_int) -> loff_t,
    >,
}

#[repr(u32)]
pub enum Whence {
    SeekSet = kernel::bindings::SEEK_SET,
    SeekCur = kernel::bindings::SEEK_CUR,
    SeekEnd = kernel::bindings::SEEK_END,
    SeekData = kernel::bindings::SEEK_DATA,
    SeekHole = kernel::bindings::SEEK_HOLE,
}

impl TryFrom<u32> for Whence {
    type Error = kernel::error::Error;

    fn try_from(value: u32) -> core::result::Result<Self, Self::Error> {
        Ok(match value {
            kernel::bindings::SEEK_SET => Self::SeekSet,
            kernel::bindings::SEEK_CUR => Self::SeekCur,
            kernel::bindings::SEEK_END => Self::SeekEnd,
            kernel::bindings::SEEK_DATA => Self::SeekData,
            kernel::bindings::SEEK_HOLE => Self::SeekHole,
            _ => return Err(EINVAL),
        })
    }
}

impl ProcOpsBuilder {
    /// Create a new builder
    pub const fn new(flags: u32) -> Self {
        Self {
            flags,
            proc_open: None,
            proc_read: None,
            proc_write: None,
            proc_lseek: None,
        }
    }

    pub const fn nonseekable_open(mut self) -> Self {
        self.proc_open = Some(nonseekable_open);
        self
    }

    /// Add an on-open callback
    pub const fn with_open(
        mut self,
        func: unsafe extern "C" fn(arg1: *mut inode, arg2: *mut file) -> core::ffi::c_int,
    ) -> Self {
        self.proc_open = Some(func);
        self
    }

    /// Add an on-read callback
    pub const fn with_read(
        mut self,
        func: unsafe extern "C" fn(
            arg1: *mut file,
            arg2: *mut core::ffi::c_char,
            arg3: usize,
            arg4: *mut loff_t,
        ) -> isize,
    ) -> Self {
        self.proc_read = Some(func);
        self
    }

    /// Add an on-write callback
    pub const fn with_write(
        mut self,
        func: unsafe extern "C" fn(
            arg1: *mut file,
            arg2: *const core::ffi::c_char,
            arg3: usize,
            arg4: *mut loff_t,
        ) -> isize,
    ) -> Self {
        self.proc_write = Some(func);
        self
    }

    pub const fn with_lseek(
        mut self,
        func: unsafe extern "C" fn(arg1: *mut file, arg2: loff_t, arg3: core::ffi::c_int) -> loff_t,
    ) -> Self {
        self.proc_lseek = Some(func);
        self
    }

    /// Construct a usable ProcOps-wrapper
    pub const fn into_proc_ops(self) -> ProcOps {
        ProcOps(bindings::proc_ops {
            proc_flags: self.flags,
            proc_open: self.proc_open,
            proc_read: self.proc_read,
            proc_read_iter: None,
            proc_write: self.proc_write,
            proc_lseek: self.proc_lseek,
            proc_release: None,
            proc_poll: None,
            proc_ioctl: None,
            proc_compat_ioctl: None,
            proc_mmap: None,
            proc_get_unmapped_area: None,
        })
    }
}

/// Usable wrapper for proc ops
pub struct ProcOps(bindings::proc_ops);

/// A proc directory entry.
pub struct ProcDirEntry<'a> {
    ptr: core::ptr::NonNull<bindings::proc_dir_entry>,
    _pd: core::marker::PhantomData<&'a ()>,
}

impl<'a> Drop for ProcDirEntry<'a> {
    fn drop(&mut self) {
        unsafe {
            bindings::proc_remove(self.ptr.as_ptr());
        }
    }
}

/// Open as nonseekable, can be used as part of a supplied `unsafe extern "C"` `proc_open`
#[inline]
pub unsafe extern "C" fn nonseekable_open(inode: *mut inode, file: *mut file) -> i32 {
    unsafe { bindings::nonseekable_open(inode, file) }
}

/// Create a proc entry with the filename `name`
pub fn proc_create<'a>(
    name: &'static kernel::str::CStr,
    mode: bindings::umode_t,
    dir_entry: Option<&ProcDirEntry<'a>>,
    proc_ops: &'a ProcOps,
) -> Result<ProcDirEntry<'a>> {
    let pops = core::ptr::addr_of!(proc_ops.0);
    let pde = unsafe {
        let dir_ent = dir_entry
            .map(|de| de.ptr.as_ptr())
            .unwrap_or_else(core::ptr::null_mut);
        bindings::proc_create(
            name.as_ptr() as *const core::ffi::c_char,
            mode,
            dir_ent,
            pops,
        )
    };
    match core::ptr::NonNull::new(pde) {
        None => Err(ENOMEM),
        Some(nn) => Ok(ProcDirEntry {
            ptr: nn,
            _pd: core::marker::PhantomData::default(),
        }),
    }
}
