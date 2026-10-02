//! Admit recovery data before its encoded copy is written or read.
pub(crate) const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(
    store: &rook_store::Store,
    key: &str,
) -> crate::Result<Option<T>> {
    store
        .kv_get_limited(key, MAX_JSON_BYTES)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
        .transpose()
}

pub(crate) fn save_json(
    store: &rook_store::Store,
    key: &str,
    value: &impl serde::Serialize,
) -> crate::Result<()> {
    let bytes = encode(value)?;
    store.kv_set(key, &bytes)?;
    store.flush()?;
    Ok(())
}

pub(crate) fn encode(value: &impl serde::Serialize) -> crate::Result<Vec<u8>> {
    encode_with_limit(value, MAX_JSON_BYTES)
}

pub(crate) fn encode_with_limit(value: &impl serde::Serialize, limit: usize) -> crate::Result<Vec<u8>> {
    struct Bounded(Vec<u8>, usize);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.1.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other(format!("serialized state exceeds {} bytes", self.1)));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoded = Bounded(Vec::new(), limit);
    serde_json::to_writer(&mut encoded, value)?;
    Ok(encoded.0)
}
