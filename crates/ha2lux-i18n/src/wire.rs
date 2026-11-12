//! Messages localisables qui voyagent dans une erreur D-Bus.
//!
//! Le démon ne connaît pas la langue de l'appelant : il envoie une **clé de
//! catalogue** et des arguments, que chaque client traduit. Format :
//! `@clé?nom=valeur&nom=valeur`, avec `%`, `&`, `=` et les caractères de
//! contrôle encodés en `%XX`. Un message qui ne commence pas par `@` est du
//! texte brut (ancien démon, erreur de transport) et reste affiché tel quel.

use std::fmt::Write as _;

use fluent_bundle::FluentArgs;

use crate::{Localizer, arg};

fn escape(value: &str, out: &mut String) {
    for c in value.chars() {
        if matches!(c, '%' | '&' | '=') || c.is_control() {
            let mut buf = [0_u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                // Écrire dans une `String` ne peut pas échouer.
                let _ = write!(out, "%{byte:02X}");
            }
        } else {
            out.push(c);
        }
    }
}

fn unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut raw = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                raw.push(byte);
                i += 3;
            }
            None => {
                raw.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

/// Encode `key` et ses arguments pour le transport.
pub fn encode(key: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = format!("@{key}");
    for (i, (name, value)) in args.iter().enumerate() {
        out.push(if i == 0 { '?' } else { '&' });
        out.push_str(name);
        out.push('=');
        escape(&value.to_string(), &mut out);
    }
    out
}

/// Décode un message encodé par [`encode`] ; `None` pour du texte brut.
pub fn decode(wire: &str) -> Option<(String, Vec<(String, String)>)> {
    let rest = wire.strip_prefix('@')?;
    let (key, query) = rest.split_once('?').unwrap_or((rest, ""));
    if key.is_empty() {
        return None;
    }
    let args = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.to_owned(), unescape(value)))
        .collect();
    Some((key.to_owned(), args))
}

impl Localizer {
    /// Traduit un message reçu du démon. Le texte brut est rendu tel quel, et une
    /// clé inconnue de cette version garde sa forme encodée (diagnostic lisible).
    pub fn localize(&self, wire: &str) -> String {
        let Some((key, values)) = decode(wire) else {
            return wire.to_owned();
        };
        let mut args = FluentArgs::new();
        for (name, value) in &values {
            match value.parse::<i64>() {
                Ok(number) => args.set(name.as_str(), arg(number)),
                Err(_) => args.set(name.as_str(), arg(value.as_str())),
            }
        }
        let text = self.translate(&key, Some(&args));
        if text == key { wire.to_owned() } else { text }
    }
}

/// Traduit un message du démon dans la langue du processus.
pub fn localize(wire: &str) -> String {
    crate::current().localize(wire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_with_special_characters() {
        let wire = encode("k", &[("a", &"x&y=z%é\n"), ("n", &42)]);
        assert!(!wire.contains('\n'));
        let (key, args) = decode(&wire).expect("décodé");
        assert_eq!(key, "k");
        assert_eq!(
            args,
            vec![
                ("a".to_owned(), "x&y=z%é\n".to_owned()),
                ("n".to_owned(), "42".to_owned())
            ]
        );
    }

    #[test]
    fn plain_text_is_not_decoded() {
        assert_eq!(decode("jeton vide"), None);
        assert_eq!(decode("@"), None);
        assert_eq!(decode("@?a=1"), None);
    }

    #[test]
    fn key_without_arguments() {
        assert_eq!(decode("@seule"), Some(("seule".to_owned(), vec![])));
    }

    #[test]
    fn malformed_escapes_do_not_panic() {
        let (_, args) = decode("@k?a=%&b=%Z1&c=%4").expect("décodé");
        assert_eq!(args.len(), 3);
    }

    #[test]
    fn localize_translates_known_keys_and_keeps_the_rest() {
        let fr = Localizer::try_new("fr").expect("catalogue fr");
        let wire = encode("i18n-self-test", &[("name", &"monde")]);
        assert_eq!(fr.localize(&wire), "bonjour monde");
        assert_eq!(fr.localize("texte brut"), "texte brut");
        assert_eq!(fr.localize("@clef-inconnue?a=1"), "@clef-inconnue?a=1");
    }
}
