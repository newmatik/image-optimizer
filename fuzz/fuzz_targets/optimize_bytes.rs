#![no_main]

use imageopt_core::{optimize_bytes, OptimizeOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = optimize_bytes(data, &OptimizeOptions::default());
});
