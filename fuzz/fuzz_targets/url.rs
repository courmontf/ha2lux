#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let _ = ha2lux_core::url::check_url(text);
    let _ = ha2lux_core::url::authority_key(text);
});
