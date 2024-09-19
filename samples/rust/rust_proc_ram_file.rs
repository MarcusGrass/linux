// SPDX-License-Identifier: GPL-2.0

//! Rust simple proc file example.

use core::{mem::MaybeUninit, option::Option, sync::atomic::Ordering};

use kernel::{
    c_str,
    prelude::*,
    proc_fs::{proc_create, ProcDirEntry, ProcHandler, ProcOps, Whence},
    sync::Mutex,
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

static INITIALIZED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::Acquire)
}

fn set_init() {
    INITIALIZED.store(true, Ordering::Release);
}

fn set_uninit() {
    INITIALIZED.store(false, Ordering::Release);
}

static mut MAYBE_UNUNIT_DATA_SLOT: MaybeUninit<Mutex<Option<SharedRamInnner>>> =
    MaybeUninit::uninit();

const POPS: ProcOps<'static, ProcHand> = ProcOps::<'static, ProcHand>::new(0);

impl kernel::Module for RustProcRamFile {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        let pde = proc_create(c_str!("rust-proc-file"), 0666, None, &POPS)?;

        let sri = SharedRamInnner {
            buf: alloc::vec::Vec::new(),
            _pde: pde,
        };
        let lock = kernel::new_mutex!(Some(sri), "proc_ram_mutex");
        unsafe {
            let slot = MAYBE_UNUNIT_DATA_SLOT.as_mut_ptr();
            lock.__pinned_init(slot)?;
        }
        set_init();
        pr_info!("Loaded /proc/rust-proc-file\n");
        Ok(Self)
    }
}

impl Drop for RustProcRamFile {
    fn drop(&mut self) {
        // Any new readers will bounce after this toggle
        set_uninit();
        let data_owned = unsafe { MAYBE_UNUNIT_DATA_SLOT.assume_init_ref() };
        data_owned.lock().take();
        // There is theoretically a race-condition, where module-users are currently in a
        // proc handler, the handler itself is 'static, so the kernel will be trusted
        // to keep function-related memory initialized until it's no longer needed.
        // There is a race-condition where it's impossible that the file can be removed and it's made sure that all users
        // get a 'graceful' exit, ie. all users who can see a file and start a proc-op gets to
        // finish it. This is because the module recording that a user has entered, and removing
        // the proc-entry can't happen atomically together. It's impossible to ensure that there
        // isn't a gap between a user entering the proc-handler, then recording its presense, and
        // removing the proc-entry and checking if the user registered.
    }
}

fn with_data<T, F: FnOnce(&mut SharedRamInnner) -> Result<T>>(func: F) -> Result<T> {
    if !is_initialized() {
        return Err(EBUSY);
    }
    let value = unsafe { MAYBE_UNUNIT_DATA_SLOT.assume_init_ref() };
    let mut guard = value.lock();
    if let Some(inner) = guard.as_mut() {
        (func)(inner)
    } else {
        return Err(EBUSY);
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
    mut user_slice: UserSliceWriter,
    offset: &kernel::bindings::loff_t,
) -> Result<(usize, usize)> {
    let Ok(offset) = usize::try_from(*offset) else {
        return Err(EINVAL);
    };
    with_data(move |inner| {
        let len = user_slice.len();
        pr_info!("Wants read max {len} bytes at offset={offset}\n");
        let cur: &[u8] = inner.buf.as_slice();
        let Some(wants_section) = cur.get(offset..) else {
            // EOF
            return Ok((0, offset));
        };
        if len >= wants_section.len() {
            user_slice.write_slice(wants_section)?;
            Ok((wants_section.len(), offset + wants_section.len()))
        } else {
            user_slice.write_slice(&wants_section[..len])?;
            Ok((len, offset + len))
        }
    })
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
    let next_offset = with_data(move |inner| {
        let buf = user_slice_reader;
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
    })?;

    Ok((len, next_offset))
}

fn plseek(
    file: &kernel::bindings::file,
    offset: kernel::bindings::loff_t,
    whence: Whence,
) -> Result<kernel::bindings::loff_t> {
    let off = with_data(|inner| match whence {
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
            let Ok(offset) = usize::try_from(file.f_pos + offset) else {
                return Err(EINVAL);
            };
            if inner.buf.len() >= offset {
                Ok(offset)
            } else {
                Err(EINVAL)
            }
        }
        Whence::SeekEnd => {
            let Ok(offset) = usize::try_from(inner.buf.len() as kernel::bindings::loff_t + offset)
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
    })?;
    let Ok(output) = kernel::bindings::loff_t::try_from(off) else {
        // Todo: Should be EOVERFLOW afaik
        return Err(EINVAL);
    };
    Ok(output)
}

#[inline]
fn file_is_append(file: &kernel::bindings::file) -> bool {
    file.f_flags & kernel::bindings::O_APPEND != 0
}
