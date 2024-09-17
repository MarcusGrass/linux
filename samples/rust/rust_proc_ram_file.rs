// SPDX-License-Identifier: GPL-2.0

//! Rust simple proc file example.

use core::{
    cell::UnsafeCell,
    marker::{Send, Sync},
    sync::atomic::AtomicBool,
};

use kernel::{
    c_str,
    prelude::*,
    proc_fs::{proc_create, ProcDirEntry, ProcOps, ProcOpsBuilder, Whence},
    sync::{
        lock::{mutex::MutexBackend, Guard},
        Mutex,
    },
    uaccess::UserSlice,
};

module! {
    type: RustProcRamFile,
    name: "rust_proc_ram_file",
    author: "Rust for Linux Contributors",
    description: "Rust proc ram file example",
    license: "GPL",
}

struct RustProcRamFile;

struct SharedRamFile {
    data: UnsafeCell<Option<Pin<Box<Mutex<Option<SharedRamInnner>>>>>>,
}

unsafe impl Send for SharedRamFile {}
unsafe impl Sync for SharedRamFile {}

impl SharedRamFile {
    const fn uninit() -> Self {
        Self {
            data: UnsafeCell::new(None),
        }
    }

    unsafe fn init(&self, data: Pin<Box<Mutex<Option<SharedRamInnner>>>>) {
        unsafe {
            self.data.get().write(Some(data));
        }
        INITIALIZED.store(true, core::sync::atomic::Ordering::Release);
    }

    fn disable(&self) -> Option<SharedRamInnner> {
        INITIALIZED.store(false, core::sync::atomic::Ordering::Release);
        let data_ref = unsafe { self.data.get().as_ref().unwrap() };
        if let Some(inner_data) = data_ref {
            inner_data.lock().take()
        } else {
            None
        }
    }

    fn write(&self, user: UserSlice, offset: Option<usize>) -> Result<usize> {
        let mut shared_ram = self.lock_buf_inner()?;
        let Some(inner) = shared_ram.as_mut() else {
            return Err(EBUSY);
        };
        let buf = user.reader();
        let len = buf.len();
        pr_info!("Wants write {len} bytes, offset={offset:?}\n");

        let cur: &mut alloc::vec::Vec<u8> = &mut inner.buf;
        let offset = offset.unwrap_or_else(|| cur.len());
        if offset == 0 {
            pr_info!(
                "Wants write {len} bytes from start into vec: {:p} with cap={}, len={}\n",
                cur.as_ptr(),
                cur.capacity(),
                cur.len(),
            );
            cur.clear();
            pr_info!(
                "Reserved {len} bytes for write, cur vec at: {:p}\n",
                cur.as_ptr()
            );
            buf.read_all(cur, GFP_KERNEL)?;
            pr_info!(
                "Wrote {len} bytes from start, currently has {} bytes\n",
                cur.len()
            );
            return Ok(cur.len());
        }
        if offset > cur.len() {
            return Err(EINVAL);
        }
        pr_info!("Wants write {len} bytes from offset={offset}\n");
        for _byte in cur.drain(offset..) {}
        buf.read_all(cur, GFP_KERNEL)?;
        Ok(cur.len())
    }

    fn read(&self, buf: UserSlice, offset: usize) -> Result<(usize, usize)> {
        let shared_ram = self.lock_buf_inner()?;
        let Some(inner) = shared_ram.as_ref() else {
            return Err(EBUSY);
        };
        let mut buf = buf.writer();
        pr_info!("Wants read max {} bytes at offset={offset}\n", buf.len());
        let cur: &[u8] = inner.buf.as_slice();
        let Some(wants_section) = cur.get(offset..) else {
            // EOF
            return Ok((0, offset));
        };
        if buf.len() >= wants_section.len() {
            buf.write_slice(wants_section)?;
            Ok((wants_section.len(), offset + wants_section.len()))
        } else {
            buf.write_slice(&wants_section[..buf.len()])?;
            Ok((buf.len(), offset + buf.len()))
        }
    }

    fn lseek(
        &self,
        cur_offset: kernel::bindings::loff_t,
        offset: kernel::bindings::loff_t,
        whence: Whence,
    ) -> Result<usize> {
        let shared_ram = self.lock_buf_inner()?;
        let Some(inner): Option<&SharedRamInnner> = shared_ram.as_ref() else {
            return Err(EBUSY);
        };
        match whence {
            Whence::SeekSet | Whence::SeekData => {
                let Ok(offset) = usize::try_from(offset) else {
                    return Err(EINVAL);
                };
                if inner.buf.len() >= offset {
                    Ok(offset)
                } else {
                    Err(EINVAL)
                }
            }
            Whence::SeekCur => {
                let Ok(offset) = usize::try_from(cur_offset + offset) else {
                    return Err(EINVAL);
                };
                if inner.buf.len() >= offset {
                    Ok(offset)
                } else {
                    Err(EINVAL)
                }
            }
            Whence::SeekEnd => {
                let Ok(offset) =
                    usize::try_from(inner.buf.len() as kernel::bindings::loff_t + offset)
                else {
                    return Err(EINVAL);
                };
                if inner.buf.len() >= offset {
                    Ok(offset)
                } else {
                    Err(EINVAL)
                }
            }
            Whence::SeekHole => Ok(inner.buf.len()),
        }
    }

    fn lock_buf_inner(&self) -> Result<Guard<'_, Option<SharedRamInnner>, MutexBackend>> {
        if !INITIALIZED.load(core::sync::atomic::Ordering::Acquire) {
            return Err(EBUSY);
        }
        let data = unsafe { self.data.get().as_ref().unwrap() };
        let Some(data_present) = data else {
            return Err(EBUSY);
        };
        let shared_ram = data_present.lock();
        Ok(shared_ram)
    }
}

struct SharedRamInnner {
    buf: alloc::vec::Vec<u8>,
    _pde: ProcDirEntry<'static>,
}

const POPS: ProcOps = ProcOpsBuilder::new(0)
    .with_open(proc_open)
    .with_read(proc_read)
    .with_write(proc_write)
    .with_lseek(proc_lseek)
    .into_proc_ops();

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static FILE_DATA: SharedRamFile = SharedRamFile::uninit();

impl kernel::Module for RustProcRamFile {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        let pde = proc_create(c_str!("rust-proc-file"), 0666, None, &POPS)?;

        let sri = SharedRamInnner {
            buf: alloc::vec::Vec::new(),
            _pde: pde,
        };
        let lock = kernel::new_mutex!(Some(sri), "proc_ram_mutex");
        let m = Box::pin_init(lock, GFP_KERNEL)?;
        unsafe {
            FILE_DATA.init(m);
        }
        pr_info!("Loaded /proc/rust-proc-file\n");
        Ok(Self)
    }
}

impl Drop for RustProcRamFile {
    fn drop(&mut self) {
        let _ = FILE_DATA.disable();
    }
}

unsafe extern "C" fn proc_open(
    _inode: *mut kernel::bindings::inode,
    _file: *mut kernel::bindings::file,
) -> i32 {
    0
}

fn file_is_append(file: *mut kernel::bindings::file) -> Result<bool> {
    unsafe {
        let f = file.as_ref().ok_or_else(|| EINVAL)?;
        Ok(f.f_flags & kernel::bindings::O_APPEND != 0)
    }
}

unsafe extern "C" fn proc_read(
    _file: *mut kernel::bindings::file,
    buf: *mut core::ffi::c_char,
    buf_cap: usize,
    read_offset: *mut kernel::bindings::loff_t,
) -> isize {
    let buf = buf as *mut u8 as usize;
    let buf_ref = UserSlice::new(buf, buf_cap);
    let offset = unsafe {
        let Some(offset_ref) = read_offset.as_mut() else {
            return EINVAL.to_errno() as isize;
        };
        offset_ref
    };
    let Ok(offset) = usize::try_from(*offset) else {
        return EINVAL.to_errno() as isize;
    };
    let next_offset = FILE_DATA.read(buf_ref, offset);
    let (read, next_offset) = match next_offset {
        core::result::Result::Ok(no) => no,
        core::result::Result::Err(e) => {
            return e.to_errno() as isize;
        }
    };
    let Ok(next_offset) = kernel::bindings::loff_t::try_from(next_offset) else {
        return EINVAL.to_errno() as isize;
    };
    unsafe {
        read_offset.write(next_offset);
    }

    let Ok(ret) = isize::try_from(read) else {
        return EINVAL.to_errno() as isize;
    };
    ret
}

unsafe extern "C" fn proc_write(
    file: *mut kernel::bindings::file,
    buf: *const core::ffi::c_char,
    buf_cap: usize,
    write_offset: *mut kernel::bindings::loff_t,
) -> isize {
    let buf = buf as *const u8 as usize;
    let user_buf = UserSlice::new(buf, buf_cap);
    let offset = unsafe {
        let Some(offset_ref) = write_offset.as_mut() else {
            return EINVAL.to_errno() as isize;
        };
        offset_ref
    };
    let Ok(offset) = usize::try_from(*offset) else {
        return EINVAL.to_errno() as isize;
    };
    let offset = match file_is_append(file) {
        Ok(is_append) => {
            if is_append {
                None
            } else {
                Some(offset)
            }
        }
        Err(e) => {
            return e.to_errno() as isize;
        }
    };
    let next_offset = FILE_DATA.write(user_buf, offset);
    let next_offset = match next_offset {
        core::result::Result::Ok(no) => no,
        core::result::Result::Err(e) => {
            return e.to_errno() as isize;
        }
    };
    let Ok(next_offset) = kernel::bindings::loff_t::try_from(next_offset) else {
        return EINVAL.to_errno() as isize;
    };
    unsafe {
        write_offset.write(next_offset);
    }

    let Ok(ret) = isize::try_from(buf_cap) else {
        return EINVAL.to_errno() as isize;
    };
    ret
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
    let off = match FILE_DATA.lseek(file_ref.f_pos, offset, whence) {
        core::result::Result::Ok(offs) => offs,
        core::result::Result::Err(e) => {
            return e.to_errno().into();
        }
    };
    pr_info!(
        "Seek pos={}, offset={offset}, next_off={off}",
        file_ref.f_pos
    );
    let Ok(output) = kernel::bindings::loff_t::try_from(off) else {
        // Todo: Should be EOVERFLOW afaik
        return EINVAL.to_errno().into();
    };
    output
}
