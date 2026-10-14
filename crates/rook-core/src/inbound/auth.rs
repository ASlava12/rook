use super::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;
fn auth_error(message: impl Into<String>) -> CoreError {
    CoreError::InboundAuthentication(message.into())
}
pub(super) fn headers(
    route: &str,
    id: &str,
    timestamp: &str,
    signature: &str,
    size: usize,
    settings: &Settings,
    now: u64,
) -> Result<()> {
    if !settings::name(route)
        || id.is_empty()
        || id.len() > 128
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        || timestamp.is_empty()
        || timestamp.len() > 20
        || !timestamp.bytes().all(|b| b.is_ascii_digit())
        || signature.len() != 67
        || !signature.starts_with("v1=")
        || !signature[3..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(auth_error("invalid inbound authentication headers"));
    }
    let at = timestamp.parse::<u64>().map_err(|_| auth_error("invalid inbound timestamp"))?;
    if now.abs_diff(at) > settings.timestamp_window_secs {
        return Err(auth_error("inbound signature timestamp is outside its replay window"));
    }
    if size > settings.body_bytes {
        return Err(auth_error("inbound body exceeds configured byte limit"));
    }
    Ok(())
}
pub(super) fn verify(
    route: &str,
    id: &str,
    timestamp: &str,
    signature: &str,
    body: &[u8],
    secret: &[u8],
) -> Result<()> {
    if !(16..=4096).contains(&secret.len()) {
        return Err(auth_error("inbound credential must contain 16..4096 bytes"));
    }
    let code = hex::decode(&signature[3..]).map_err(|_| auth_error("invalid inbound signature"))?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(|_| auth_error("invalid inbound credential"))?;
    mac.update(b"rook-inbound-v1\0");
    for field in [route, id, timestamp] {
        mac.update(field.as_bytes());
        mac.update(b"\0");
    }
    mac.update(body);
    mac.verify_slice(&code).map_err(|_| auth_error("inbound authentication failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_hmac_vector_binds_the_route_delivery_timestamp_and_exact_json_bytes() {
        // Computed independently with Python's hmac/hashlib, not this verifier.
        let signature = "v1=51a36e79f2af4b5d82372c58b516992feb5c64a0c1d0567de1c5b4d2012a6583";
        headers("alpha", "delivery-1", "1700000000", signature, 13, &Settings::default(), 1700000000)
            .unwrap();
        verify("alpha", "delivery-1", "1700000000", signature, br#"{"code":"42"}"#, b"0123456789abcdef")
            .unwrap();
        for (route, id, at, body) in [
            ("beta", "delivery-1", "1700000000", br#"{"code":"42"}"#.as_slice()),
            ("alpha", "delivery-2", "1700000000", br#"{"code":"42"}"#.as_slice()),
            ("alpha", "delivery-1", "1700000001", br#"{"code":"42"}"#.as_slice()),
            ("alpha", "delivery-1", "1700000000", br#"{"code":"43"}"#.as_slice()),
        ] {
            assert!(verify(route, id, at, signature, body, b"0123456789abcdef").is_err());
        }
        assert!(
            headers("alpha", "delivery-1", "1700000000", signature, 13, &Settings::default(), 1700000301)
                .is_err()
        );
    }
}
