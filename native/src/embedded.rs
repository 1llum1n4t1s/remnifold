pub const WORKER_BUNDLE: &str = include_str!(concat!(env!("OUT_DIR"), "/worker.mjs"));
include!(concat!(env!("OUT_DIR"), "/migrations.rs"));
