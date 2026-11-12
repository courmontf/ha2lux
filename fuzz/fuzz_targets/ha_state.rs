#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = ha2lux_ha::reading::parse_state_body(data);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = ha2lux_ha::sensors::parse_sensor_lines(text);
    }
});
