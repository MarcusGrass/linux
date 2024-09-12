//! Implementation of proc_fs functionality
//!

use crate::types::Opaque;

/// Proc options
pub struct ProcOps(Opaque<bindings::proc_ops>);

/// A proc directory entry, currently uninstantiable/unimplemented.
///
/// Consequences are that files can only be created under `/proc`, and no subdirectories, as of
/// now.
pub struct ProcDirEntry(Opaque<bindings::proc_dir_entry>);

/// Create a proc entry with the filename `name`
pub fn proc_create(
    name: &core::ffi::CStr,
    mode: bindings::umode_t,
    dir_entry: Option<&ProcDirEntry>,
    proc_ops: &ProcOps,
) -> kernel::prelude::Result<()> {
    unsafe {
        let dir_ent = dir_entry
            .map(|de| de.0.get())
            .unwrap_or_else(core::ptr::null_mut);
        bindings::proc_create(
            name.as_ptr() as *const core::ffi::c_char,
            mode,
            dir_ent,
            proc_ops.0.get(),
        );
    }
    Ok(())
}
