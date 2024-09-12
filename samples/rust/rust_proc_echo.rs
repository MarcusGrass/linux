use kernel::pr_cont;
use kernel::prelude::*;

module! {
    type: RustProcEcho,
    name: "rust_print",
    author: "Rust for Linux Contributors",
    description: "Rust proc file echo example",
    license: "GPL",
}

struct RustProcEcho;

impl kernel::Module for RustProcEcho {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Loaded rust /proc/echo");

        Ok(Self)
    }
}
