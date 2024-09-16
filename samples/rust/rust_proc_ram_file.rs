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
    proc_fs::{proc_create, ProcDirEntry, ProcOps, ProcOpsBuilder},
    sync::Mutex,
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

    fn write(&self, user: UserSlice, offset: usize) -> Result<usize> {
        let mut buf = user.reader();
        let len = buf.len();
        if !INITIALIZED.load(core::sync::atomic::Ordering::Acquire) {
            pr_info!("Try write uninit, {len} bytes\n");
            return Err(EBUSY);
        }
        pr_info!("Write is init\n");
        let data = unsafe { self.data.get().as_ref().unwrap() };
        let Some(data_present) = data else {
            pr_info!("Try read uninit data, {len} bytes\n");
            return Err(EBUSY);
        };
        pr_info!("Write data is present\n");
        let mut shared_ram = data_present.lock();

        let Some(inner) = shared_ram.as_mut() else {
            pr_info!("Try write empty, {len} bytes\n");
            return Err(EBUSY);
        };
        pr_info!("Wants write {len} bytes, offset={offset}\n");

        let cur: &mut alloc::vec::Vec<u8> = &mut inner.buf;
        if offset == 0 || offset == len {
            pr_info!(
                "Wants write {len} bytes from start into vec: {:p} with cap={}, len={}\n",
                cur.as_ptr(),
                cur.capacity(),
                cur.len(),
            );
            cur.reserve(len, GFP_KERNEL)?;
            pr_info!(
                "Reserved {len} bytes for write, cur vec at: {:p}\n",
                cur.as_ptr()
            );
            buf.read_slice(&mut cur.as_mut_slice()[..len])?;
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
        let total_space_needed = offset + len;
        for _byte in cur.drain(offset..) {}
        cur.reserve(total_space_needed, GFP_KERNEL)?;
        buf.read_all(cur, GFP_KERNEL)?;
        Ok(cur.len())
    }

    fn read(&self, buf: UserSlice, offset: usize) -> Result<(usize, usize)> {
        let mut buf = buf.writer();
        if !INITIALIZED.load(core::sync::atomic::Ordering::Acquire) {
            pr_info!("Try read uninit, {} bytes\n", buf.len());
            return Err(EBUSY);
        }
        pr_info!("Read is init\n");
        let data = unsafe { self.data.get().as_ref().unwrap() };
        let Some(data_present) = data else {
            pr_info!("Try read uninit data, {} bytes\n", buf.len());
            return Err(EBUSY);
        };
        pr_info!("Read is present\n");
        let shared_ram = data_present.lock();
        let Some(inner) = shared_ram.as_ref() else {
            pr_info!("Try read empty, {} bytes\n", buf.len());
            return Err(EBUSY);
        };
        pr_info!("Wants read max {} bytes\n", buf.len());
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
}

struct SharedRamInnner {
    buf: alloc::vec::Vec<u8>,
    _pde: ProcDirEntry<'static>,
}

const POPS: ProcOps = ProcOpsBuilder::new(0)
    .with_open(proc_open)
    .with_read(proc_read)
    .with_write(proc_write)
    .into_proc_ops();

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static FILE_DATA: SharedRamFile = SharedRamFile::uninit();

impl kernel::Module for RustProcRamFile {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Loading rust /proc/rust-proc-file\n");
        let pde = proc_create(c_str!("rust-proc-file"), 0666, None, &POPS)?;

        let sri = SharedRamInnner {
            buf: alloc::vec::Vec::new(),
            _pde: pde,
        };
        pr_info!("Created empty SRI with vec at ptr={:p}\n", sri.buf.as_ptr());
        let lock = kernel::new_mutex!(Some(sri), "proc_ram_mutex");
        pr_info!("Created new mutex\n");
        let m = Box::pin_init(lock, GFP_KERNEL)?;
        pr_info!("Initialized new boxed mutex\n");
        unsafe {
            FILE_DATA.init(m);
            pr_info!("Initialized file data.\n");
        }
        Ok(Self)
    }
}

impl Drop for RustProcRamFile {
    fn drop(&mut self) {
        let _ = FILE_DATA.disable();
    }
}

unsafe extern "C" fn proc_open(
    inode: *mut kernel::bindings::inode,
    file: *mut kernel::bindings::file,
) -> i32 {
    pr_info!("Proc open");
    unsafe { kernel::proc_fs::nonseekable_open(inode, file) }
}

unsafe extern "C" fn proc_read(
    _file: *mut kernel::bindings::file,
    buf: *mut core::ffi::c_char,
    buf_cap: usize,
    read_offset: *mut kernel::bindings::loff_t,
) -> isize {
    pr_info!("Got read");
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
    _file: *mut kernel::bindings::file,
    buf: *const core::ffi::c_char,
    buf_cap: usize,
    write_offset: *mut kernel::bindings::loff_t,
) -> isize {
    let buf = buf as *const u8 as usize;
    pr_info!("Input buf at {buf:x}");
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
