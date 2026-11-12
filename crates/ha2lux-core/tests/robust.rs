//! Robustness: no input (even hostile) must make a parser panic,
//! and the security invariants hold for all inputs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ha2lux_core::config::Config;
use ha2lux_core::url::{authority_key, check_url, is_local_url, normalize_url};
use ha2lux_core::{MinuteOfDay, ddc};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn url_parsers_never_panic(input in "\\PC{0,200}") {
        let _ = normalize_url(&input);
        let _ = check_url(&input);
        let _ = is_local_url(&input);
        let _ = authority_key(&input);
    }

    #[test]
    fn url_parsers_survive_url_like_noise(
        scheme in "(http|https|ftp|file|javascript|)",
        sep in "(://|:/|:|//|)",
        user in "[a-z@:%.]{0,8}",
        host in "[a-zA-Z0-9.\\[\\]:%-]{0,30}",
        tail in "[/?#a-z0-9%.:@-]{0,30}",
    ) {
        let input = format!("{scheme}{sep}{user}{host}{tail}");
        let _ = normalize_url(&input);
        let _ = check_url(&input);
        let _ = authority_key(&input);
    }

    /// An accepted address is always http(s), with no embedded credentials.
    #[test]
    fn validated_urls_are_http_without_credentials(input in "\\PC{0,120}") {
        // `normalize_url` only normalizes; it is `check_url` that validates.
        if let Ok(checked) = check_url(&input) {
            let url = checked.url;
            prop_assert!(url.starts_with("http://") || url.starts_with("https://"), "{url}");
            let authority = url.split("://").nth(1).unwrap().split(['/', '?', '#']).next().unwrap();
            prop_assert!(!authority.contains('@'), "{url}");
        }
    }

    #[test]
    fn config_parser_never_panics(input in "\\PC{0,400}") {
        let _ = Config::parse(&input);
    }

    #[test]
    fn config_parser_survives_toml_like_noise(
        key in "[a-z_.]{1,12}",
        value in "(\\[|\\]|\"|[0-9]{1,5}|true|false|-[0-9]{1,3}|[a-z]{1,6}|\\{ time = \"[0-9:]{0,5}\", level = [0-9-]{1,4} \\})",
        table in "(\\[general\\]|\\[auto\\]|\\[schedule\\]|\\[idle\\]|\\[presets\\]|\\[\\[displays\\]\\]|\\[nope\\])",
    ) {
        let _ = Config::parse(&format!("{table}\n{key} = {value}\n"));
    }

    #[test]
    fn time_parser_never_panics(input in "\\PC{0,12}") {
        let _ = MinuteOfDay::parse(&input);
    }

    #[test]
    fn ddc_frame_parsers_never_panic(
        frame in proptest::collection::vec(any::<u8>(), 0..64),
        vcp in any::<u8>(),
    ) {
        let _ = ddc::parse_get_vcp_reply(&frame, vcp);
        let _ = ddc::decode_request(&frame);
    }
}
