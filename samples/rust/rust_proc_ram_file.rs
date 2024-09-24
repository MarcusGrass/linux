// SPDX-License-Identifier: GPL-2.0

//! Rust simple proc file example.
//! The module creates a file under `/proc/rust-proc-file` which functions similarly to a regular
//! file, backed by a memory buffer contained in this module.
//!
//! It's read, write, seekable, appendable by anyone by default

use core::{mem::MaybeUninit, option::Option};

use kernel::{
    c_str,
    prelude::*,
    proc_fs::{proc_create, ProcDirEntry, ProcHandler, ProcOpFileHandle, ProcOps, Whence},
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

mod backing_data {
    use core::cell::UnsafeCell;

    use kernel::sync::lock::{mutex::MutexBackend, Lock};

    use super::*;

    static mut MAYBE_UNUNIT_DATA_SLOT: MaybeUninit<Mutex<Option<alloc::vec::Vec<u8>>>> =
        MaybeUninit::uninit();

    struct SingleAccessPdeStore(UnsafeCell<Option<ProcDirEntry<'static>>>);
    unsafe impl Sync for SingleAccessPdeStore {}
    static ENTRY: SingleAccessPdeStore = SingleAccessPdeStore(UnsafeCell::new(None));

    /// Initialize the backing data of this module, letting new
    /// users access it.
    /// # Safety
    /// Safe if only called once during the module's lifetime
    pub(super) unsafe fn init_data(
        lock_ready: impl PinInit<Lock<Option<alloc::vec::Vec<u8>>, MutexBackend>>,
    ) -> Result<()> {
        unsafe {
            let slot = MAYBE_UNUNIT_DATA_SLOT.as_mut_ptr();
            lock_ready.__pinned_init(slot)?;
        }
        Ok(())
    }

    /// Write PDE into static memory
    /// # Safety
    /// Any concurrent access is unsafe.  
    pub(super) unsafe fn set_pde(pde: ProcDirEntry<'static>) {
        unsafe {
            ENTRY.0.get().write(Some(pde));
        }
    }

    /// Get's the initialized data as a static reference
    /// # Safety
    /// Safe only if called after initialization, otherwise
    /// it will return a pointer to uninitialized memory.  
    pub(super) unsafe fn get_initialized_data() -> &'static Mutex<Option<alloc::vec::Vec<u8>>> {
        unsafe { MAYBE_UNUNIT_DATA_SLOT.assume_init_ref() }
    }

    /// Remove the PDE
    /// # Safety
    /// While safe to invoke regardless of PDE initalization,
    /// any concurrent access is unsafe.  
    pub(super) unsafe fn take_pde() -> Option<ProcDirEntry<'static>> {
        unsafe {
            let mut_ref = ENTRY.0.get().as_mut()?;
            mut_ref.take()
        }
    }
}

impl kernel::Module for RustProcRamFile {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        const POPS: ProcOps<'static, ProcHand> = ProcOps::<'static, ProcHand>::new(0);
        // Struct defined inline since this is the only safe place for it to be used
        struct ProcHand;

        impl ProcHand {
            unsafe fn popen(file: &mut ProcOpFileHandle) -> Result<i32> {
                unsafe {
                    with_data(|d| {
                        let flags = file.get_flags();
                        if flags & kernel::bindings::O_TRUNC != 0 {
                            d.clear();
                        }
                        Ok(())
                    })?;
                }
                Ok(0)
            }

            /// Read handler
            /// # Safety
            /// Safe only if run as part of this module's process handler
            unsafe fn pread(
                _file: &mut ProcOpFileHandle,
                mut user_slice: UserSliceWriter,
                offset: kernel::bindings::loff_t,
            ) -> Result<(usize, usize)> {
                let Ok(offset) = usize::try_from(offset) else {
                    return Err(EINVAL);
                };
                unsafe {
                    with_data(move |inner| {
                        let len = user_slice.len();
                        let cur: &[u8] = inner.as_slice();
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
            }

            /// Write handler
            /// # Safety
            /// Safe only if run as part of this module's process handler
            unsafe fn pwrite(
                file: &mut ProcOpFileHandle,
                user_slice_reader: UserSliceReader,
                offset: kernel::bindings::loff_t,
            ) -> Result<(usize, usize)> {
                let len = user_slice_reader.len();
                let Ok(offset) = usize::try_from(offset) else {
                    return Err(EINVAL);
                };
                let offset = if flags_are_append(file.get_flags()) {
                    None
                } else {
                    Some(offset)
                };
                let next_offset = unsafe {
                    with_data(move |cur| {
                        let mut buf = user_slice_reader;
                        let len = buf.len();

                        let offset = offset.unwrap_or_else(|| cur.len());
                        // Illegal start offset
                        if offset > cur.len() {
                            return Err(EINVAL);
                        }
                        // Append
                        if offset == cur.len() {
                            buf.read_all(cur, GFP_KERNEL)?;
                            return Ok(cur.len());
                        }
                        if len + offset > cur.len() {
                            // Needs more space, first write into the space available, then extend
                            buf.read_slice(&mut cur.as_mut_slice()[offset..])?;
                            buf.read_all(cur, GFP_KERNEL)?;
                            Ok(cur.len())
                        } else {
                            // Has enough space, overwrite the wanted section
                            buf.read_slice(&mut cur.as_mut_slice()[offset..offset + len])?;
                            Ok(offset + len)
                        }
                    })?
                };

                Ok((len, next_offset))
            }

            /// Seek handler
            /// # Safety
            /// Safe only if run as part of this module's process handler
            unsafe fn plseek(
                file: &mut ProcOpFileHandle,
                offset: kernel::bindings::loff_t,
                whence: Whence,
            ) -> Result<kernel::bindings::loff_t> {
                let off = unsafe {
                    with_data(|inner| match whence {
                        Whence::SeekSet | Whence::SeekData => {
                            let Ok(offset) = usize::try_from(offset) else {
                                return Err(EINVAL);
                            };
                            if inner.len() >= offset {
                                Ok(offset)
                            } else {
                                Err(EINVAL)
                            }
                        }
                        Whence::SeekCur => {
                            let start_pos = file.read_pos_unsync();
                            let Ok(offset) = usize::try_from(start_pos + offset) else {
                                return Err(EINVAL);
                            };
                            if inner.len() >= offset {
                                Ok(offset)
                            } else {
                                Err(EINVAL)
                            }
                        }
                        Whence::SeekEnd => {
                            let Ok(offset) =
                                usize::try_from(inner.len() as kernel::bindings::loff_t + offset)
                            else {
                                return Err(EINVAL);
                            };
                            if inner.len() >= offset {
                                Ok(offset)
                            } else {
                                Err(EINVAL)
                            }
                        }
                        Whence::SeekHole => Ok(inner.len()),
                    })?
                };
                let Ok(output) = kernel::bindings::loff_t::try_from(off) else {
                    // Todo: Should be EOVERFLOW afaik
                    return Err(EINVAL);
                };
                unsafe {
                    file.write_pos_unsync(output);
                }
                Ok(output)
            }
        }

        let data = alloc::vec::Vec::new();
        let lock = kernel::new_mutex!(Some(data), "proc_ram_mutex");
        unsafe {
            // Safety: Only place this is called, has to be invoked before `proc_create`
            backing_data::init_data(lock)?
        }

        // This is technically unsound, e.g. READ is not safe to invoke until
        // `init_data` has been called, but could theoretically be invoked in a safe context before
        // then, so don't, it's ordered like this for a reason.
        impl ProcHandler<'static> for ProcHand {
            const OPEN: kernel::proc_fs::ProcOpen<'static> = &|f| unsafe { Self::popen(f) };

            const READ: kernel::proc_fs::ProcRead<'static> =
                &|f, u, o| unsafe { Self::pread(f, u, o) };

            const WRITE: kernel::proc_fs::ProcWrite<'static> =
                &|f, u, o| unsafe { Self::pwrite(f, u, o) };

            const LSEEK: kernel::proc_fs::ProcLseek<'static> =
                &|f, o, w| unsafe { Self::plseek(f, o, w) };
        }

        let pde = proc_create(c_str!("rust-proc-file"), 0666, None, &POPS)?;
        unsafe {
            // Safety: Only place this is called, no concurrent access
            backing_data::set_pde(pde);
        }
        pr_info!("Loaded /proc/rust-proc-file\n");
        Ok(Self)
    }
}

impl Drop for RustProcRamFile {
    fn drop(&mut self) {
        // Remove the PDE if initialized
        // Drop it to remove the proc entry
        unsafe {
            // Safety:
            // Runs at most once, no concurrent access
            backing_data::take_pde();
        }

        // Remove and deallocate the data
        unsafe {
            // Safety:
            // This module is only instantiated if data is initialized, therefore
            // the data is initialized when this destructor is run.
            backing_data::get_initialized_data().lock().take();
        }
        // There is theoretically a race-condition, where module-users are currently in a
        // proc handler, the handler itself is 'static, so the kernel will be trusted
        // to keep function-related memory initialized until it's no longer needed.
        // There is a race-condition where it's impossible that the file can be removed and it's made sure that all users
        // get a 'graceful' exit, ie. all users who can see a file and start a proc-op gets to
        // finish it. This is because the module recording that a user has entered, and removing
        // the proc-entry can't happen atomically together. It's impossible to ensure that there
        // isn't a gap between a user entering the proc-handler, then recording its presense, and
        // removing the proc-entry and checking if the user registered.
        // In that case, the user will get an EBUSY
    }
}

/// Execute a function with the initialized module data
/// # Safety
/// Only safe if invoked after initialization
unsafe fn with_data<T, F: FnOnce(&mut alloc::vec::Vec<u8>) -> Result<T>>(func: F) -> Result<T> {
    let value = unsafe { backing_data::get_initialized_data() };
    let mut guard = value.lock();
    if let Some(inner) = guard.as_mut() {
        (func)(inner)
    } else {
        return Err(EBUSY);
    }
}

#[inline]
fn flags_are_append(flags: core::ffi::c_uint) -> bool {
    flags & kernel::bindings::O_APPEND != 0
}
