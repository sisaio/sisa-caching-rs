//! Feeds arbitrary bytes to the backplane worker's message reader. It must never panic, and a
//! message it accepts must serialize back to the same message.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|payload: &[u8]| sisa_caching_hybrid::backplane::fuzz_receive(payload));
