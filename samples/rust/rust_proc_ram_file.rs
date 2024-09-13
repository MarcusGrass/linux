// SPDX-License-Identifier: GPL-2.0

//! Rust simple proc file example.

use core::{
    marker::{Send, Sync},
    sync::atomic::AtomicBool,
};

use kernel::{
    c_str,
    prelude::*,
    proc_fs::{proc_create, ProcDirEntry, ProcOpsBuilder},
    sync::{Arc, Mutex},
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
    initialized: AtomicBool,
    data: Option<Arc<Mutex<SharedRamInnner>>>,
}

unsafe impl Send for SharedRamFile {}
unsafe impl Sync for SharedRamFile {}

impl SharedRamFile {
    const fn uninit() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            data: None,
        }
    }

    fn init(&mut self, data: Arc<Mutex<SharedRamInnner>>) {
        self.data = Some(data);
        self.initialized
            .store(true, core::sync::atomic::Ordering::Release);
    }

    fn disable(&mut self) -> Option<Arc<Mutex<SharedRamInnner>>> {
        self.initialized
            .store(false, core::sync::atomic::Ordering::Release);
        self.data.take()
    }

    fn write(&mut self, buf: &[u8], offset: usize) -> Result<usize> {
        let Some(inner) = self.data.as_ref() else {
            return Err(EBUSY);
        };

        let mut shared_ram = inner.lock();
        let cur: &mut alloc::vec::Vec<u8> = &mut shared_ram.buf;
        if offset == 0 || offset == buf.len() {
            cur.extend_from_slice(buf, GFP_KERNEL)?;
            return Ok(cur.len());
        }
        if offset > cur.len() {
            return Err(EINVAL);
        }
        let total_space_needed = offset + buf.len();
        for _byte in cur.drain(offset..) {}
        cur.reserve(total_space_needed, GFP_KERNEL)?;
        cur.extend_from_slice(buf, GFP_KERNEL)?;
        Ok(cur.len())
    }

    fn read(&mut self, buf: &mut [u8], offset: usize) -> Result<(usize, usize)> {
        let Some(inner) = self.data.as_ref() else {
            return Err(EBUSY);
        };
        let shared_ram = inner.lock();
        let cur: &[u8] = shared_ram.buf.as_slice();
        let Some(wants_section) = cur.get(offset..) else {
            // EOF
            return Ok((0, offset));
        };
        if buf.len() >= wants_section.len() {
            buf[..wants_section.len()].copy_from_slice(wants_section);
            Ok((wants_section.len(), offset + wants_section.len()))
        } else {
            buf.copy_from_slice(&wants_section[..buf.len()]);
            Ok((buf.len(), offset + buf.len()))
        }
    }
}

struct SharedRamInnner {
    buf: alloc::vec::Vec<u8>,
    _pde: ProcDirEntry,
}

const POPS: ProcOpsBuilder = ProcOpsBuilder::new(0)
    .with_open(kernel::proc_fs::nonseekable_open)
    .with_read(proc_read)
    .with_write(proc_write);

static mut FILE_DATA: SharedRamFile = SharedRamFile::uninit();

impl kernel::Module for RustProcRamFile {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Loading rust /proc/rust-proc-file");
        let pde = proc_create(c_str!("rust-proc-file"), 0666, None, POPS)?;

        let sri = SharedRamInnner {
            buf: alloc::vec::Vec::new(),
            _pde: pde,
        };
        let lock = kernel::new_mutex!(sri, "proc_ram_mutex");
        let arc = Arc::pin_init(lock, GFP_KERNEL)?;
        unsafe {
            FILE_DATA.init(arc);
        }
        Ok(Self)
    }
}

impl Drop for RustProcRamFile {
    fn drop(&mut self) {
        unsafe {
            let data = FILE_DATA.disable();
            if let Some(inner) = data {
                if inner.into_unique_or_drop().is_none() {
                    pr_warn!("Dropping reference to shared inner, but it is currently being used");
                }
            }
        }
    }
}

unsafe extern "C" fn proc_read(
    _file: *mut kernel::bindings::file,
    buf: *mut core::ffi::c_char,
    buf_cap: usize,
    read_offset: *mut kernel::bindings::loff_t,
) -> isize {
    let buf = buf as *mut u8;
    let buf_ref = unsafe { core::slice::from_raw_parts_mut(buf, buf_cap) };
    let offset = unsafe {
        let Some(offset_ref) = read_offset.as_mut() else {
            return EINVAL.to_errno() as isize;
        };
        offset_ref
    };
    let Ok(offset) = usize::try_from(*offset) else {
        return EINVAL.to_errno() as isize;
    };
    let next_offset = unsafe { FILE_DATA.read(buf_ref, offset) };
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
    let buf = buf as *const u8;
    let buf_ref = unsafe { core::slice::from_raw_parts(buf, buf_cap) };
    let offset = unsafe {
        let Some(offset_ref) = write_offset.as_mut() else {
            return EINVAL.to_errno() as isize;
        };
        offset_ref
    };
    let Ok(offset) = usize::try_from(*offset) else {
        return EINVAL.to_errno() as isize;
    };
    let next_offset = unsafe { FILE_DATA.write(buf_ref, offset) };
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
