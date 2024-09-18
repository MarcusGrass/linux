//! Implementation of proc_fs functionality
//!

use core::{marker::PhantomData, option::Option};

use bindings::{file, inode, loff_t, proc_ops};

use kernel::error::Result;

use crate::{
    prelude::{EINVAL, ENOMEM},
    uaccess::{UserSlice, UserSliceReader, UserSliceWriter},
};

/// Type alias for open function signature
pub type ProcOpen<'a> = &'a dyn Fn(&inode, &file) -> Result<i32>;
/// Type alias for read function signature
pub type ProcRead<'a> = &'a dyn Fn(&file, UserSliceWriter, &loff_t) -> Result<(usize, usize)>;
/// Type alias for write function signature
pub type ProcWrite<'a> = &'a dyn Fn(&file, UserSliceReader, &loff_t) -> Result<(usize, usize)>;
/// Type alias for lseek function signature
pub type ProcLseek<'a> = &'a dyn Fn(&file, loff_t, Whence) -> Result<loff_t>;

/// Proc file ops handler
pub trait ProcHandler<'a> {
    /// Open handler
    const OPEN: ProcOpen<'a>;
    /// Read handler
    const READ: ProcRead<'a>;
    /// Write handler
    const WRITE: ProcWrite<'a>;
    /// Lseek handler
    const LSEEK: ProcLseek<'a>;
}

/// Todo: linux doc link
#[repr(u32)]
pub enum Whence {
    /// Todo
    SeekSet = kernel::bindings::SEEK_SET,
    /// Todo
    SeekCur = kernel::bindings::SEEK_CUR,
    /// Todo
    SeekEnd = kernel::bindings::SEEK_END,
    /// Todo
    SeekData = kernel::bindings::SEEK_DATA,
    /// Todo
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

/// Usable wrapper for proc ops
pub struct ProcOps<'a, T>
where
    T: ProcHandler<'a>,
{
    ops: bindings::proc_ops,
    _pd: PhantomData<&'a T>,
}
impl<'a, T> ProcOps<'a, T>
where
    T: ProcHandler<'a>,
{
    /// Create new ProcOps from a handler and flags
    pub const fn new(proc_flags: u32) -> Self {
        Self {
            ops: proc_ops {
                proc_flags,
                proc_open: Some(ProcOps::<'a, T>::proc_open),
                proc_read: Some(ProcOps::<'a, T>::proc_read),
                proc_read_iter: None,
                proc_write: Some(ProcOps::<'a, T>::proc_write),
                proc_lseek: Some(ProcOps::<'a, T>::proc_lseek),
                proc_release: None,
                proc_poll: None,
                proc_ioctl: None,
                proc_compat_ioctl: None,
                proc_mmap: None,
                proc_get_unmapped_area: None,
            },
            _pd: PhantomData,
        }
    }
    unsafe extern "C" fn proc_open(
        inode: *mut kernel::bindings::inode,
        file: *mut kernel::bindings::file,
    ) -> i32 {
        unsafe {
            // Todo: This is likely a UB-risk, need a better abstraction than
            // casting this as a mutable reference, since it's unknown what else
            // in the kernel may be mutably referencing this.
            let Some(inode_ref) = inode.as_ref() else {
                return EINVAL.to_errno();
            };
            let Some(file_ref) = file.as_ref() else {
                return EINVAL.to_errno();
            };
            match (T::OPEN)(inode_ref, file_ref) {
                Ok(code) => code,
                Err(e) => e.to_errno(),
            }
        }
    }
    unsafe extern "C" fn proc_read(
        file: *mut kernel::bindings::file,
        buf: *mut core::ffi::c_char,
        buf_cap: usize,
        read_offset: *mut kernel::bindings::loff_t,
    ) -> isize {
        let file_ref = unsafe {
            match file.as_ref() {
                Some(f) => f,
                None => {
                    return EINVAL.to_errno() as isize;
                }
            }
        };
        let buf = buf as *mut u8 as usize;
        let buf_ref = UserSlice::new(buf, buf_cap);
        let buf_writer = buf_ref.writer();
        let offset = unsafe {
            let Some(offset_ref) = read_offset.as_mut() else {
                return EINVAL.to_errno() as isize;
            };
            offset_ref
        };
        match (T::READ)(file_ref, buf_writer, offset) {
            // Todo: Double check this conversion, only 'safe' if in large file mode
            Ok((read_bytes, next_offset)) => {
                unsafe {
                    read_offset.write(next_offset as i64);
                }
                read_bytes as isize
            }
            Err(e) => e.to_errno() as isize,
        }
    }
    unsafe extern "C" fn proc_write(
        file: *mut kernel::bindings::file,
        buf: *const core::ffi::c_char,
        buf_cap: usize,
        write_offset: *mut kernel::bindings::loff_t,
    ) -> isize {
        let file_ref = unsafe {
            match file.as_ref() {
                Some(f) => f,
                None => {
                    return EINVAL.to_errno() as isize;
                }
            }
        };
        let buf = buf as *mut u8 as usize;
        let buf_ref = UserSlice::new(buf, buf_cap);
        let buf_writer = buf_ref.reader();
        let offset = unsafe {
            let Some(offset_ref) = write_offset.as_mut() else {
                return EINVAL.to_errno() as isize;
            };
            offset_ref
        };
        match (T::WRITE)(file_ref, buf_writer, offset) {
            // Todo: Double check this conversion, only 'safe' if in large file mode
            Ok((written_bytes, next_offset)) => {
                unsafe {
                    write_offset.write(next_offset as i64);
                }
                written_bytes as isize
            }
            Err(e) => e.to_errno() as isize,
        }
    }
    unsafe extern "C" fn proc_lseek(
        file: *mut kernel::bindings::file,
        offset: kernel::bindings::loff_t,
        whence: core::ffi::c_int,
    ) -> kernel::bindings::loff_t {
        let Ok(whence_u32) = u32::try_from(whence) else {
            return EINVAL.to_errno().into();
        };
        let Ok(whence) = Whence::try_from(whence_u32) else {
            return EINVAL.to_errno().into();
        };
        let file_ref = unsafe {
            let Some(file_ref) = file.as_ref() else {
                return EINVAL.to_errno().into();
            };
            file_ref
        };
        match (T::LSEEK)(file_ref, offset, whence) {
            core::result::Result::Ok(offs) => offs,
            core::result::Result::Err(e) => {
                return e.to_errno().into();
            }
        }
    }
}

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

/// Nonseekable open
#[inline]
pub fn rust_nonseekable(i: &mut inode, f: &mut file) -> Result<i32> {
    unsafe { Ok(nonseekable_open(i as *mut inode, f as *mut file)) }
}

/// Open as nonseekable, can be used as part of a supplied `unsafe extern "C"` `proc_open`
#[inline]
pub unsafe extern "C" fn nonseekable_open(inode: *mut inode, file: *mut file) -> i32 {
    unsafe { bindings::nonseekable_open(inode, file) }
}

/// Create a proc entry with the filename `name`
pub fn proc_create<'a, T>(
    name: &'static kernel::str::CStr,
    mode: bindings::umode_t,
    dir_entry: Option<&ProcDirEntry<'a>>,
    proc_ops: &'a ProcOps<'a, T>,
) -> Result<ProcDirEntry<'a>>
where
    T: ProcHandler<'a>,
{
    let pops = core::ptr::addr_of!(proc_ops.ops);
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
