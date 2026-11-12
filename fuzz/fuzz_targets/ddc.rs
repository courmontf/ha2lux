#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let vcp = data.first().copied().unwrap_or(0x10);
    let _ = ha2lux_core::ddc::parse_get_vcp_reply(data, vcp);
    let _ = ha2lux_core::ddc::decode_request(data);
});
