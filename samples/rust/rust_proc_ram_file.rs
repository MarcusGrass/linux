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
    proc_fs::{proc_create, ProcDirEntry, ProcHandler, ProcOps, Whence},
    sync::{
        lock::{mutex::MutexBackend, Guard},
        Mutex,
    },
    uaccess::{UserSliceReader, UserSliceWriter},
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

    fn write(&self, user: UserSliceReader, offset: Option<usize>) -> Result<usize> {
        let mut shared_ram = self.lock_buf_inner()?;
        let Some(inner) = shared_ram.as_mut() else {
            return Err(EBUSY);
        };
        let buf = user;
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

    fn read(&self, buf: UserSliceWriter, offset: usize) -> Result<(usize, usize)> {
        let shared_ram = self.lock_buf_inner()?;
        let Some(inner) = shared_ram.as_ref() else {
            return Err(EBUSY);
        };
        let mut buf = buf;
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

struct ProcHand;

impl ProcHandler<'static> for ProcHand {
    const OPEN: kernel::proc_fs::ProcOpen<'static> = &popen;

    const READ: kernel::proc_fs::ProcRead<'static> = &pread;

    const WRITE: kernel::proc_fs::ProcWrite<'static> = &pwrite;

    const LSEEK: kernel::proc_fs::ProcLseek<'static> = &plseek;
}

#[inline]
fn popen(_inode: &kernel::bindings::inode, _file: &kernel::bindings::file) -> Result<i32> {
    Ok(0)
}

fn pread(
    _file: &kernel::bindings::file,
    user_slice: UserSliceWriter,
    offset: &kernel::bindings::loff_t,
) -> Result<(usize, usize)> {
    let Ok(offset) = usize::try_from(*offset) else {
        return Err(EINVAL);
    };
    let (read, next_offset) = FILE_DATA.read(user_slice, offset)?;
    Ok((read, next_offset))
}

fn pwrite(
    file: &kernel::bindings::file,
    user_slice_reader: UserSliceReader,
    offset: &kernel::bindings::loff_t,
) -> Result<(usize, usize)> {
    let len = user_slice_reader.len();
    let Ok(offset) = usize::try_from(*offset) else {
        return Err(EINVAL);
    };
    let offset = if file_is_append(file) {
        None
    } else {
        Some(offset)
    };

    let next_offset = FILE_DATA.write(user_slice_reader, offset)?;
    Ok((len, next_offset))
}

fn plseek(
    file: &kernel::bindings::file,
    offset: kernel::bindings::loff_t,
    whence: Whence,
) -> Result<kernel::bindings::loff_t> {
    let off = FILE_DATA.lseek(file.f_pos, offset, whence)?;
    let Ok(output) = kernel::bindings::loff_t::try_from(off) else {
        // Todo: Should be EOVERFLOW afaik
        return Err(EINVAL);
    };
    Ok(output)
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static FILE_DATA: SharedRamFile = SharedRamFile::uninit();

const POPS: ProcOps<'static, ProcHand> = ProcOps::<'static, ProcHand>::new(0);

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

#[inline]
fn file_is_append(file: &kernel::bindings::file) -> bool {
    file.f_flags & kernel::bindings::O_APPEND != 0
}
