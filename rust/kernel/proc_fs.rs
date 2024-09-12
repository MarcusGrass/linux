//! Implementation of proc_fs functionality
//!

use crate::types::Opaque;

use kernel::error::Result;

/// Proc options
pub struct ProcOps(Opaque<bindings::proc_ops>);

/// A proc directory entry.
pub struct ProcDirEntry(*mut bindings::proc_dir_entry);

impl Drop for ProcDirEntry {
    fn drop(&mut self) {
        unsafe {
            bindings::proc_remove(self.0);
        }
    }
}

/// Create a proc entry with the filename `name`
pub fn proc_create(
    name: &core::ffi::CStr,
    mode: bindings::umode_t,
    dir_entry: Option<&ProcDirEntry>,
    proc_ops: &ProcOps,
) -> Result<ProcDirEntry> {
    let pde = unsafe {
        let dir_ent = dir_entry.map(|de| de.0).unwrap_or_else(core::ptr::null_mut);
        bindings::proc_create(
            name.as_ptr() as *const core::ffi::c_char,
            mode,
            dir_ent,
            proc_ops.0.get(),
        )
    };
    let pde = ProcDirEntry(pde);
    Ok(pde)
}
