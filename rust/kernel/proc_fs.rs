//! Implementation of proc_fs functionality
//!

use core::option::Option;

use bindings::{file, inode, loff_t};

use kernel::error::Result;

use crate::prelude::ENOMEM;

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
}

impl ProcOpsBuilder {
    /// Create a new builder
    pub const fn new(flags: u32) -> Self {
        Self {
            flags,
            proc_open: None,
            proc_read: None,
            proc_write: None,
        }
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

    const fn into_proc_ops(self) -> bindings::proc_ops {
        bindings::proc_ops {
            proc_flags: self.flags,
            proc_open: self.proc_open,
            proc_read: self.proc_read,
            proc_read_iter: None,
            proc_write: self.proc_write,
            proc_lseek: None,
            proc_release: None,
            proc_poll: None,
            proc_ioctl: None,
            proc_compat_ioctl: None,
            proc_mmap: None,
            proc_get_unmapped_area: None,
        }
    }
}

/// A proc directory entry.
pub struct ProcDirEntry(core::ptr::NonNull<bindings::proc_dir_entry>);

impl Drop for ProcDirEntry {
    fn drop(&mut self) {
        unsafe {
            bindings::proc_remove(self.0.as_ptr());
        }
    }
}

/// Create a proc entry with the filename `name`
pub fn proc_create(
    name: &core::ffi::CStr,
    mode: bindings::umode_t,
    dir_entry: Option<&ProcDirEntry>,
    proc_ops_builder: ProcOpsBuilder,
) -> Result<ProcDirEntry> {
    let pops = proc_ops_builder.into_proc_ops();
    let pde = unsafe {
        let dir_ent = dir_entry
            .map(|de| de.0.as_ptr())
            .unwrap_or_else(core::ptr::null_mut);
        bindings::proc_create(
            name.as_ptr() as *const core::ffi::c_char,
            mode,
            dir_ent,
            core::ptr::addr_of!(pops),
        )
    };
    match core::ptr::NonNull::new(pde) {
        None => Err(ENOMEM),
        Some(nn) => Ok(ProcDirEntry(nn)),
    }
}
