#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let _ = ha2lux_core::config::Config::parse(text);
});
