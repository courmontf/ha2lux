//! T-URL: normalization and classification of Home Assistant URLs (spec §8.2).
use ha2lux_core::url::{
    UrlError, UrlNote, authority_key, check_url, is_http, is_local_url, normalize_url,
};

#[test]
fn t_url_01_unchanged() {
    for u in ["http://192.168.1.10:8123", "https://ha.example.com"] {
        assert_eq!(normalize_url(u).unwrap(), u);
    }
}

#[test]
fn t_url_02_scheme_added() {
    assert_eq!(
        normalize_url("192.168.1.10:8123").unwrap(),
        "http://192.168.1.10:8123"
    );
    assert_eq!(
        normalize_url("homeassistant.local:8123").unwrap(),
        "http://homeassistant.local:8123"
    );
    assert_eq!(
        normalize_url("ha.local:8123/path").unwrap(),
        "http://ha.local:8123/path"
    );
}

#[test]
fn t_url_03_trimmed() {
    assert_eq!(
        normalize_url("  http://192.168.1.10:8123 \t\r\n").unwrap(),
        "http://192.168.1.10:8123"
    );
    assert_eq!(normalize_url(" 10.0.0.5 ").unwrap(), "http://10.0.0.5");
}

#[test]
fn t_url_04_scheme_case_kept() {
    assert_eq!(normalize_url("HTTP://X").unwrap(), "HTTP://X");
}

#[test]
fn t_url_05_empty() {
    assert_eq!(normalize_url(""), Err(UrlError::Empty));
    assert_eq!(normalize_url("   "), Err(UrlError::Empty));
}

#[test]
fn t_url_06_length_bound() {
    let exact = format!("http://{}", "a".repeat(255 - 7));
    assert_eq!(exact.len(), 255);
    assert!(normalize_url(&exact).is_ok());
    assert_eq!(normalize_url(&format!("{exact}a")), Err(UrlError::TooLong));
    // The length is assessed after adding the scheme.
    assert_eq!(normalize_url(&"a".repeat(250)), Err(UrlError::TooLong));
}

#[test]
fn t_url_07_is_http() {
    assert!(is_http("HTTP://x"));
    assert!(is_http("http://x"));
    assert!(!is_http("https://x"));
    assert!(!is_http("httpx://x"));
    assert!(!is_http("htt"));
}

#[test]
fn t_url_08_local() {
    for u in [
        "192.168.1.10",
        "10.0.0.5",
        "172.16.0.1",
        "172.31.255.255",
        "127.0.0.1",
        "169.254.10.1",
        "homeassistant.local",
        "HA.LOCAL.",
        "ha.lan",
        "ha.internal",
        "ha.home.arpa",
        "homeassistant",
        "localhost",
        "[fd00::10]",
        "[fe80::1%25eth0]",
        "[::1]",
        "user:pw@192.168.1.10:8123/x?y#z",
        "http://192.168.1.10:8123",
        "https://ha.local:8123/x",
    ] {
        assert!(is_local_url(u), "{u} devrait être local");
    }
}

#[test]
fn t_url_09_not_local() {
    for u in [
        "ha.example.com",
        "8.8.8.8",
        "172.32.0.1",
        "172.15.0.1",
        "192.169.1.1",
        "[2001:db8::1]",
        "ha.local.example.com",
        "256.1.1.1.example",
        "1.2.3",
        "[::ffff:192.168.1.1]",
        "not a url",
        "http://",
        "134744072",
        "0x08080808",
        "010.0.0.1",
        "192.168.01.1",
        "evil.com\\@10.0.0.1",
        "[fe80::1]evil.com",
        ".",
        "evil%2elocal",
        "[1fc00::1]",
        "[fffffffff::1]",
        "8ball",
    ] {
        assert!(!is_local_url(u), "{u} ne devrait pas être local");
    }
}

#[test]
fn check_url_notes() {
    let lan = check_url("192.168.1.10:8123").unwrap();
    assert_eq!((lan.local, lan.note), (true, UrlNote::HttpOnLan));
    let public = check_url("http://ha.example.com").unwrap();
    assert_eq!((public.local, public.note), (false, UrlNote::HttpPublic));
    let tls = check_url("https://ha.example.com").unwrap();
    assert_eq!(tls.note, UrlNote::None);
    assert_eq!(
        check_url("HTTPS://HA.example.com").unwrap().note,
        UrlNote::None
    );
}

#[test]
fn check_url_rejects_ambiguous_forms() {
    for u in [
        "http://010.0.0.1",
        "http://192.168.01.1",
        "http://134744072",
        "http://0x08080808",
        "http://1.2.3",
        "http://evil.com\\@10.0.0.1",
        "http://[fe80::1]evil.com",
        "http://user:pw@192.168.1.10",
        "http://evil%2elocal",
        "http://",
        "ftp://ha.local",
        "http://ha.local:notaport",
        "http://ha.local:",
        "http://ha local",
    ] {
        assert!(check_url(u).is_err(), "{u} devrait être rejeté");
    }
}

#[test]
fn check_url_accepts_normal_forms() {
    for u in [
        "http://homeassistant.local:8123",
        "https://ha.example.com/",
        "http://[fd00::10]:8123",
        "ha.lan",
    ] {
        assert!(check_url(u).is_ok(), "{u}");
    }
}

#[test]
fn t_sec_04_the_token_key_is_the_normalized_host_and_port() {
    let key = |u: &str| authority_key(u).unwrap();
    assert_eq!(
        key("http://HomeAssistant.local:8123"),
        "homeassistant.local:8123"
    );
    assert_eq!(
        key("homeassistant.local:8123/path?x=1"),
        "homeassistant.local:8123",
        "chemin et paramètres ignorés"
    );
    assert_eq!(
        key("https://ha.example.com"),
        "ha.example.com:443",
        "port par défaut explicite"
    );
    assert_eq!(key("http://ha.example.com"), "ha.example.com:80");
    assert_eq!(
        key("https://HA.Example.COM:443/"),
        key("https://ha.example.com"),
        "même serveur, même clé"
    );
    assert_eq!(key("http://192.168.1.10:8123"), "192.168.1.10:8123");
    assert_eq!(key("http://[fd00::10]:8123"), "[fd00::10]:8123");
    // Changing host, port or scheme (hence the default port) changes the key: the token
    // saved for the old server is never presented to the new one.
    assert_ne!(key("http://ha.local:8123"), key("http://ha.local:8124"));
    assert_ne!(key("http://ha.local:8123"), key("http://evil.local:8123"));
    assert_ne!(key("http://ha.example.com"), key("https://ha.example.com"));
    // Rejected forms: no key.
    for bad in [
        "",
        "http://user:pw@ha.local",
        "ftp://ha.local",
        "http://010.0.0.1",
        "http://134744072",
        "not a url",
    ] {
        assert_eq!(authority_key(bad), None, "{bad}");
    }
}
