//! Implementation of proc_fs functionality
//!

use core::{marker::PhantomData, mem::offset_of, option::Option};

use bindings::{file, inode, loff_t, proc_ops};

use kernel::error::Result;

use crate::{
    prelude::{EINVAL, ENOMEM},
    uaccess::{UserSlice, UserSliceReader, UserSliceWriter},
};

/// Type alias for open function signature
pub type ProcOpen<'a> = &'a dyn Fn(&mut ProcOpFileHandle) -> Result<i32>;
/// Type alias for read function signature
pub type ProcRead<'a> =
    &'a dyn Fn(&mut ProcOpFileHandle, UserSliceWriter, loff_t) -> Result<(usize, usize)>;
/// Type alias for write function signature
pub type ProcWrite<'a> =
    &'a dyn Fn(&mut ProcOpFileHandle, UserSliceReader, loff_t) -> Result<(usize, usize)>;
/// Type alias for lseek function signature
pub type ProcLseek<'a> = &'a dyn Fn(&mut ProcOpFileHandle, loff_t, Whence) -> Result<loff_t>;

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

/// lseek valid variants [See the lseek docs for more detail](https://man7.org/linux/man-pages/man2/lseek.2.html)
#[repr(u32)]
#[derive(Copy, Clone, Debug)]
pub enum Whence {
    /// See above doc link
    SeekSet = kernel::bindings::SEEK_SET,
    /// See above doc link
    SeekCur = kernel::bindings::SEEK_CUR,
    /// See above doc link
    SeekEnd = kernel::bindings::SEEK_END,
    /// See above doc link
    SeekData = kernel::bindings::SEEK_DATA,
    /// See above doc link
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

/// A safe wrapper for the kernel's `file`-structure, file contains a lot of
/// fields which have different synchronization requirements.
/// This provides a safer subset for reading/writing some select data to/from the `file`
pub struct ProcOpFileHandle(core::ptr::NonNull<kernel::bindings::file>);

impl ProcOpFileHandle {
    /// Gets the file flags
    pub fn get_flags(&self) -> core::ffi::c_uint {
        unsafe {
            // Safety: flags are only set on open, any time
            // code gets here it's not going to be modified anymore
            let flags_offset = offset_of!(kernel::bindings::file, f_flags);
            self.0
                .cast::<u8>()
                .add(flags_offset)
                .cast::<core::ffi::c_uint>()
                .read()
        }
    }

    fn pos_ptr(&self) -> core::ptr::NonNull<kernel::bindings::loff_t> {
        unsafe {
            let pos_offset = offset_of!(kernel::bindings::file, f_pos);
            self.0
                .cast::<u8>()
                .add(pos_offset)
                .cast::<kernel::bindings::loff_t>()
        }
    }

    /// Reading the position is inescapably subject to raciness.  
    /// The kernel will, on this file-pointer, update position after each read and write,
    /// see ksys_read, ksys_write at read_write.c.  
    /// If the file is opened with f_mode `FMODE_ATOMIC_POS`, the proc-function is run under
    /// a pos-lock if needed, in which case this is safe from data-races.  
    /// Which means its up to the user to make sure concurrent read-writes doesn't happen if
    /// they want the offset to make sense.
    /// # Safety:
    /// This number may or may not make any sense, bounds checking is still necessary
    /// to retain safety
    pub unsafe fn read_pos_unsync(&self) -> kernel::bindings::loff_t {
        unsafe { self.pos_ptr().read() }
    }

    /// Same raciness problems as [`Self::read_pos_unsync`].
    /// # Safety:
    /// This number may or may not make any sense, bounds checking is still necessary
    /// to retain safety
    pub unsafe fn write_pos_unsync(&self, offset: loff_t) {
        unsafe {
            self.pos_ptr().write(offset);
        }
    }
}

/// Wrapper for the kernel type `proc_ops`
/// Roughly a translation of the expected `extern "C"`-function pointers that
/// the kernel expects into Rust-functions with a few more helpful types.
pub struct ProcOps<'a, T>
where
    T: ProcHandler<'static>,
{
    ops: bindings::proc_ops,
    _pd: PhantomData<&'a T>,
}
impl<'a, T> ProcOps<'a, T>
where
    T: ProcHandler<'static>,
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
        _inode: *mut kernel::bindings::inode,
        file: *mut kernel::bindings::file,
    ) -> i32 {
        let mut file_ref = if let Some(ptr) = core::ptr::NonNull::new(file) {
            ProcOpFileHandle(ptr)
        } else {
            return EINVAL.to_errno() as i32;
        };
        match (T::OPEN)(&mut file_ref) {
            Ok(code) => code,
            Err(e) => e.to_errno(),
        }
    }
    unsafe extern "C" fn proc_read(
        file: *mut kernel::bindings::file,
        buf: *mut core::ffi::c_char,
        buf_cap: usize,
        read_offset: *mut kernel::bindings::loff_t,
    ) -> isize {
        let mut file_ref = if let Some(ptr) = core::ptr::NonNull::new(file) {
            ProcOpFileHandle(ptr)
        } else {
            return EINVAL.to_errno() as isize;
        };
        let buf = buf as *mut u8 as usize;
        let buf_ref = UserSlice::new(buf, buf_cap);
        let buf_writer = buf_ref.writer();
        let Some(offset_non_null) = core::ptr::NonNull::new(read_offset) else {
            // Only null if stream, unsupported
            return EINVAL.to_errno() as isize;
        };
        let offset = unsafe { offset_non_null.read() };
        match (T::READ)(&mut file_ref, buf_writer, offset) {
            // Todo: Double check this conversion, only 'safe' if in large file mode
            Ok((read_bytes, next_offset)) => {
                unsafe {
                    offset_non_null.write(next_offset as kernel::bindings::loff_t);
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
        let mut file_ref = if let Some(ptr) = core::ptr::NonNull::new(file) {
            ProcOpFileHandle(ptr)
        } else {
            return EINVAL.to_errno() as isize;
        };

        let buf = buf as *mut u8 as usize;
        let buf_ref = UserSlice::new(buf, buf_cap);
        let buf_writer = buf_ref.reader();
        let Some(offset_non_null) = core::ptr::NonNull::new(write_offset) else {
            // Only null if opened as a stream, unsupported
            return EINVAL.to_errno() as isize;
        };
        let offset = unsafe { offset_non_null.read() };

        match (T::WRITE)(&mut file_ref, buf_writer, offset) {
            // Todo: Double check this conversion, only 'safe' if in large file mode
            Ok((written_bytes, next_offset)) => {
                unsafe {
                    offset_non_null.write(next_offset as i64);
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
        let mut file_ref = if let Some(ptr) = core::ptr::NonNull::new(file) {
            ProcOpFileHandle(ptr)
        } else {
            return EINVAL.to_errno().into();
        };
        match (T::LSEEK)(&mut file_ref, offset, whence) {
            core::result::Result::Ok(offs) => offs,
            core::result::Result::Err(e) => {
                return e.to_errno().into();
            }
        }
    }
}

/// A proc directory entry.
/// When drop, the directory is removed.  
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

/// Nonseekable open, required if not implementing `lseek`
#[inline]
pub fn rust_nonseekable(i: &mut inode, f: &mut file) -> Result<i32> {
    unsafe {
        Ok(kernel::bindings::nonseekable_open(
            i as *mut inode,
            f as *mut file,
        ))
    }
}

/// Create a proc entry with the filename `name`
pub fn proc_create<'a, T>(
    name: &'static kernel::str::CStr,
    mode: bindings::umode_t,
    dir_entry: Option<&ProcDirEntry<'a>>,
    proc_ops: &'a ProcOps<'a, T>,
) -> Result<ProcDirEntry<'a>>
where
    T: ProcHandler<'static>,
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
