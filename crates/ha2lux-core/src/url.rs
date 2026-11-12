//! URL de Home Assistant : normalisation, validation, classification LAN (spec §8.2).

use std::net::{Ipv4Addr, Ipv6Addr};

use thiserror::Error;

/// Longueur maximale d'une URL normalisée.
pub const MAX_URL_LEN: usize = 255;

const LOCAL_SUFFIXES: [&str; 4] = [".local", ".lan", ".internal", ".home.arpa"];

/// Erreurs de validation d'URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum UrlError {
    /// Entrée vide.
    #[error("l'adresse est vide")]
    Empty,
    /// Dépasse [`MAX_URL_LEN`].
    #[error("the address exceeds {MAX_URL_LEN} characters")]
    TooLong,
    /// Schéma autre que http/https.
    #[error("only the http and https schemes are accepted")]
    UnsupportedScheme,
    /// Identifiants (`utilisateur:mot-de-passe@`) interdits dans l'URL.
    #[error("the address must not contain credentials (the token is entered separately)")]
    UserInfo,
    /// Hôte ambigu ou mal formé (zéros initiaux, entier nu, antislash, etc.).
    #[error("invalid or ambiguous host")]
    InvalidHost,
}

/// Longueur du schéma si `s` commence par `schéma://`.
fn scheme_len(s: &str) -> Option<usize> {
    let end = s.find("://")?;
    let scheme = &s[..end];
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    valid.then_some(end)
}

/// Retire les espaces, ajoute `http://` si aucun schéma, borne la taille.
/// La casse du schéma est conservée.
pub fn normalize_url(input: &str) -> Result<String, UrlError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(UrlError::Empty);
    }
    let url = if scheme_len(trimmed).is_some() {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    };
    if url.len() > MAX_URL_LEN {
        return Err(UrlError::TooLong);
    }
    Ok(url)
}

/// Vrai si l'URL utilise le schéma `http` (insensible à la casse).
pub fn is_http(url: &str) -> bool {
    url.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
}

enum Host {
    V4(Ipv4Addr),
    V6(Ipv6Addr),
    Name(String),
}

fn parse_ipv4_strict(s: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (slot, part) in octets.iter_mut().zip(parts) {
        let ok = !part.is_empty()
            && part.len() <= 3
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'));
        if !ok {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(Ipv4Addr::from(octets))
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn parse_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
}

/// Analyse `hôte[:port]` (sans informations d'utilisateur).
fn parse_host(hostport: &str) -> Option<Host> {
    if let Some(rest) = hostport.strip_prefix('[') {
        let (inside, after) = rest.split_once(']')?;
        if !after.is_empty() && !after.strip_prefix(':').is_some_and(parse_port) {
            return None;
        }
        let (addr, zone) = match inside.split_once('%') {
            Some((a, z)) => (a, Some(z)),
            None => (inside, None),
        };
        if zone.is_some_and(|z| {
            z.is_empty()
                || !z
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"%-_.".contains(&b))
        }) {
            return None;
        }
        return addr.parse().ok().map(Host::V6);
    }
    let host = match hostport.rsplit_once(':') {
        Some((h, port)) => {
            if !parse_port(port) {
                return None;
            }
            h
        }
        None => hostport,
    };
    let lowered = host.to_ascii_lowercase();
    let name = lowered.strip_suffix('.').unwrap_or(&lowered);
    if name.is_empty() {
        return None;
    }
    let numeric_like = name.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    if numeric_like {
        return parse_ipv4_strict(name).map(Host::V4);
    }
    if !name.split('.').all(valid_label) {
        return None;
    }
    Some(Host::Name(name.to_owned()))
}

/// Dernier label numérique ou hexadécimal (`0x…`) : forme ambiguë (entier nu).
fn ambiguous_name(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    last.bytes().all(|b| b.is_ascii_digit())
        || last
            .strip_prefix("0x")
            .is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_local_host(host: &Host) -> bool {
    match host {
        Host::V4(a) => a.is_private() || a.is_loopback() || a.is_link_local(),
        Host::V6(a) => a.is_loopback() || a.is_unique_local() || a.is_unicast_link_local(),
        Host::Name(n) => {
            if !n.contains('.') {
                n.starts_with(|c: char| c.is_ascii_alphabetic())
            } else {
                LOCAL_SUFFIXES.iter().any(|s| n.ends_with(s))
            }
        }
    }
}

/// Sépare schéma, informations d'utilisateur, `hôte[:port]`.
/// `None` si l'autorité est manifestement invalide (espace, antislash).
fn split_authority(url: &str) -> Option<(Option<&str>, &str)> {
    let rest = scheme_len(url).map_or(url, |n| &url[n + 3..]);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains(|c: char| c == '\\' || c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(match authority.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, authority),
    })
}

/// Vrai si l'adresse désigne un hôte « réseau local » : IPv4 privées
/// (10/8, 172.16/12, 192.168/16), lien-local 169.254/16, boucle locale ;
/// IPv6 ULA, lien-local, `::1` ; noms `.local`, `.lan`, `.internal`,
/// `.home.arpa` ou sans point. Toute forme ambiguë ou mal formée donne `false`.
pub fn is_local_url(url: &str) -> bool {
    split_authority(url)
        .and_then(|(_, hostport)| parse_host(hostport))
        .is_some_and(|h| is_local_host(&h))
}

/// Note à afficher à l'utilisateur lors de la configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlNote {
    /// Rien à signaler (https).
    None,
    /// Information : http sur réseau local.
    HttpOnLan,
    /// Avertissement : http hors réseau local (confirmation explicite requise).
    HttpPublic,
}

/// URL validée et normalisée.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedUrl {
    /// URL normalisée, sans informations d'utilisateur.
    pub url: String,
    /// Hôte sur réseau local.
    pub local: bool,
    /// Note de sécurité associée.
    pub note: UrlNote,
}

/// Normalise puis valide une URL saisie par l'utilisateur : http/https
/// seulement, pas d'identifiants, hôte non ambigu.
pub fn check_url(input: &str) -> Result<CheckedUrl, UrlError> {
    let url = normalize_url(input)?;
    let scheme = scheme_len(&url).map(|n| url[..n].to_ascii_lowercase());
    if !matches!(scheme.as_deref(), Some("http" | "https")) {
        return Err(UrlError::UnsupportedScheme);
    }
    let (user, hostport) = split_authority(&url).ok_or(UrlError::InvalidHost)?;
    if user.is_some() {
        return Err(UrlError::UserInfo);
    }
    let host = parse_host(hostport).ok_or(UrlError::InvalidHost)?;
    if let Host::Name(name) = &host
        && ambiguous_name(name)
    {
        return Err(UrlError::InvalidHost);
    }
    let local = is_local_host(&host);
    let note = match (is_http(&url), local) {
        (false, _) => UrlNote::None,
        (true, true) => UrlNote::HttpOnLan,
        (true, false) => UrlNote::HttpPublic,
    };
    Ok(CheckedUrl { url, local, note })
}

/// Clé de stockage du jeton : `hôte:port` en minuscules, sans identifiants, chemin ni
/// paramètres (le port par défaut du schéma est rendu explicite). Deux adresses qui
/// désignent le même serveur ont la même clé ; **changer d'hôte ou de port change la clé**,
/// donc le jeton enregistré pour l'ancien serveur n'est jamais présenté au nouveau.
///
/// Renvoie `None` si l'adresse est refusée par [`check_url`].
pub fn authority_key(input: &str) -> Option<String> {
    let checked = check_url(input).ok()?;
    let https = checked
        .url
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"));
    let (_, hostport) = split_authority(&checked.url)?;
    let host = parse_host(hostport)?;
    let (host_text, explicit_port) = match &host {
        Host::V6(a) => (
            format!("[{a}]"),
            hostport.rsplit_once("]:").map(|(_, p)| p.to_owned()),
        ),
        Host::V4(a) => (
            a.to_string(),
            hostport.rsplit_once(':').map(|(_, p)| p.to_owned()),
        ),
        Host::Name(n) => (
            n.clone(),
            hostport.rsplit_once(':').map(|(_, p)| p.to_owned()),
        ),
    };
    let port = explicit_port
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(if https { 443 } else { 80 });
    Some(format!("{host_text}:{port}"))
}
