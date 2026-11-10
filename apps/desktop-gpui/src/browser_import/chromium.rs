//! `ChromiumCookies.ts` and `ChromiumKeys.ts`: Chromium's cookie key, and its encrypted cookies.
//!
//! macOS and Linux encrypt with AES-128-CBC under a PBKDF2-HMAC-SHA1 key: on macOS from the
//! Keychain's "Safe Storage" password, on Linux `peanuts` (`v10`) or the Secret Service's
//! password (`v11`). Windows encrypts with AES-256-GCM under a key DPAPI wraps in `Local State`.

use std::collections::HashSet;
use std::path::Path;

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockDecryptMut as _, KeyIvInit as _};
use rusqlite::types::Value;
use sha2::{Digest as _, Sha256};

use super::sources::{Chromium, Platform};
use super::{CookieRead, ImportedCookie, Reason, SameSite};
use crate::sqlite::Snapshot;

/// `OSCrypt`'s CBC mode uses a fixed IV of 16 spaces.
const CBC_IV: [u8; 16] = [b' '; 16];
const GCM_NONCE: usize = 12;
const GCM_TAG: usize = 16;
// Chromium derives its macOS and Linux keys; Windows keeps its key in `Local State`.
#[cfg(any(unix, test))]
const SALT: &[u8] = b"saltysalt";
/// Seconds from 1601 (Chromium's epoch) to 1970.
const WEBKIT_EPOCH_OFFSET: i64 = 11_644_473_600;

/// The keys a profile's cookies may be encrypted under.
#[derive(Default)]
pub(super) struct Keys {
    cbc_v10: Option<[u8; 16]>,
    cbc_v11: Option<[u8; 16]>,
    /// Why Linux had no `v11` key; it fails the import only when nothing else decrypted.
    cbc_v11_error: Option<Reason>,
    /// Some Linux clients encrypt with an empty passphrase (crbug.com/1195256).
    cbc_empty: Option<[u8; 16]>,
    gcm_v10: Option<[u8; 32]>,
}

#[cfg(any(unix, test))]
fn hmac_sha1(key: &[u8], message: &[&[u8]]) -> [u8; 20] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..20].copy_from_slice(&sha1_smol::Sha1::from(key).digest().bytes());
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = sha1_smol::Sha1::new();
    inner.update(&block.map(|byte| byte ^ 0x36));
    for part in message {
        inner.update(part);
    }
    let mut outer = sha1_smol::Sha1::new();
    outer.update(&block.map(|byte| byte ^ 0x5c));
    outer.update(&inner.digest().bytes());
    outer.digest().bytes()
}

/// PBKDF2-HMAC-SHA1's first block, which holds a 16-byte key.
#[cfg(any(unix, test))]
fn pbkdf2_sha1(passphrase: &[u8], salt: &[u8], iterations: u32) -> [u8; 20] {
    let mut u = hmac_sha1(passphrase, &[salt, &1u32.to_be_bytes()]);
    let mut block = u;
    for _ in 1..iterations {
        u = hmac_sha1(passphrase, &[&u]);
        block.iter_mut().zip(u).for_each(|(out, byte)| *out ^= byte);
    }
    block
}

#[cfg(any(unix, test))]
fn derive(passphrase: &[u8], iterations: u32) -> [u8; 16] {
    let block = pbkdf2_sha1(passphrase, SALT, iterations);
    let mut key = [0; 16];
    key.copy_from_slice(&block[..16]);
    key
}

/// Linux keys: `v10` always uses Chromium's fallback passphrase; the keyring's password unlocks
/// `v11`. A missing keyring or key leaves `v10` readable; a refusal stops the import.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn linux_keys(keyring: Result<Vec<u8>, Reason>) -> Result<Keys, Reason> {
    let (cbc_v11, cbc_v11_error) = match keyring {
        Ok(secret) => (Some(derive(&secret, 1)), None),
        Err(Reason::NeedsKeychainApproval) => return Err(Reason::NeedsKeychainApproval),
        Err(reason) => (None, Some(reason)),
    };
    Ok(Keys {
        cbc_v10: Some(derive(b"peanuts", 1)),
        cbc_v11,
        cbc_v11_error,
        cbc_empty: Some(derive(b"", 1)),
        gcm_v10: None,
    })
}

#[cfg(target_os = "macos")]
fn keys(chromium: &Chromium, _: Option<&Path>) -> Result<Keys, Reason> {
    use keyring_core::api::CredentialStoreApi as _;
    let store = apple_native_keyring_store::keychain::Store::new().map_err(|_| Reason::KeychainUnavailable)?;
    let entry = store
        .build(chromium.keychain_service, chromium.keychain_account, None)
        .map_err(|_| Reason::KeychainUnavailable)?;
    // The Keychain may ask the user first; this thread waits for the answer.
    let secret = match entry.get_secret() {
        Ok(secret) if !secret.is_empty() => secret,
        Ok(_) | Err(keyring_core::Error::NoEntry) => return Err(Reason::KeychainItemMissing),
        Err(_) => return Err(Reason::NeedsKeychainApproval),
    };
    Ok(Keys { cbc_v10: Some(derive(&secret, 1003)), ..Keys::default() })
}

/// The Secret Service item Chromium looks its key up by: the `application` attribute alone,
/// since items written by older keyrings carry no schema name.
#[cfg(all(unix, not(target_os = "macos")))]
fn keys(chromium: &Chromium, _: Option<&Path>) -> Result<Keys, Reason> {
    use keyring_core::api::CredentialStoreApi as _;
    fn reason(error: &keyring_core::Error) -> Reason {
        match error {
            keyring_core::Error::NoEntry => Reason::KeychainItemMissing,
            keyring_core::Error::NoStorageAccess(_) => Reason::NeedsKeychainApproval,
            _ => Reason::KeychainUnavailable,
        }
    }
    let Some(application) = chromium.linux_secret else { return Err(Reason::UnsupportedPlatform) };
    let keyring = (|| {
        let store = zbus_secret_service_keyring_store::Store::new().map_err(|_| Reason::KeychainUnavailable)?;
        let spec = std::collections::HashMap::from([("application", application)]);
        // A locked collection asks the user to unlock it, as libsecret's search does.
        let entries = store.search(&spec).map_err(|error| reason(&error))?;
        let secret =
            entries.first().ok_or(Reason::KeychainItemMissing)?.get_secret().map_err(|error| reason(&error))?;
        if secret.is_empty() { Err(Reason::KeychainItemMissing) } else { Ok(secret) }
    })();
    linux_keys(keyring)
}

/// `readWindowsKey`: `Local State`'s DPAPI-wrapped key. A browser that also wraps it with
/// App-Bound Encryption cannot be read by another app.
#[cfg(windows)]
fn keys(_: &Chromium, local_state: Option<&Path>) -> Result<Keys, Reason> {
    let contents = std::fs::read_to_string(local_state.ok_or(Reason::ReadFailed)?).map_err(|_| Reason::ReadFailed)?;
    let wrapped = wrapped_windows_key(&contents)?;
    let key =
        windows_dpapi::decrypt_data(&wrapped, windows_dpapi::Scope::User, None).map_err(|_| Reason::ReadFailed)?;
    Ok(Keys { gcm_v10: Some(key.try_into().map_err(|_| Reason::ReadFailed)?), ..Keys::default() })
}

/// `decodeWindowsWrappedKey`: the DPAPI blob after its `DPAPI` prefix.
#[cfg(any(windows, test))]
fn wrapped_windows_key(local_state: &str) -> Result<Vec<u8>, Reason> {
    use base64::Engine as _;
    let state: serde_json::Value = serde_json::from_str(local_state).map_err(|_| Reason::ReadFailed)?;
    let crypt = &state["os_crypt"];
    if !crypt["app_bound_encrypted_key"].is_null() {
        return Err(Reason::UnsupportedPlatform);
    }
    let encoded = crypt["encrypted_key"].as_str().ok_or(Reason::ReadFailed)?;
    let wrapped = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| Reason::ReadFailed)?;
    wrapped.strip_prefix(b"DPAPI").map(<[u8]>::to_vec).ok_or(Reason::ReadFailed)
}

/// Chromium 127+ (schema 24) prefixes the value with its host's SHA-256, binding it to that host.
fn strip_domain_binding(plaintext: &[u8], host: &str, schema: i64) -> Option<String> {
    let value =
        if schema < 24 { plaintext } else { plaintext.strip_prefix(Sha256::digest(host.as_bytes()).as_slice())? };
    Some(String::from_utf8_lossy(value).into_owned())
}

fn decrypt_cbc(payload: &[u8], key: &[u8; 16], host: &str, schema: i64) -> Option<String> {
    let mut buffer = payload.to_vec();
    let decryptor = cbc::Decryptor::<aes::Aes128>::new(key.into(), &CBC_IV.into());
    let plaintext = decryptor.decrypt_padded_mut::<Pkcs7>(&mut buffer).ok()?;
    strip_domain_binding(plaintext, host, schema)
}

fn decrypt_gcm(payload: &[u8], key: &[u8; 32], host: &str, schema: i64) -> Option<String> {
    use aes_gcm::aead::{Aead as _, KeyInit as _};
    if payload.len() < GCM_NONCE + GCM_TAG {
        return None;
    }
    let (nonce, sealed) = payload.split_at(GCM_NONCE);
    let plaintext = aes_gcm::Aes256Gcm::new(key.into()).decrypt(aes_gcm::Nonce::from_slice(nonce), sealed).ok()?;
    strip_domain_binding(&plaintext, host, schema)
}

/// `decryptChromiumValue`: `None` when no key opens it.
fn decrypt_value(encrypted: &[u8], keys: &Keys, host: &str, schema: i64, platform: Platform) -> Option<String> {
    if encrypted.is_empty() {
        return Some(String::new());
    }
    let (prefix, payload) = encrypted.split_at(encrypted.len().min(3));
    // Windows `v20` records are app-bound; no key here opens them.
    if platform == Platform::Windows {
        let key = keys.gcm_v10.as_ref().filter(|_| prefix == b"v10")?;
        return decrypt_gcm(payload, key, host, schema);
    }
    let with = |key: Option<&[u8; 16]>| {
        let key = key?;
        decrypt_cbc(payload, key, host, schema).or_else(|| decrypt_cbc(payload, keys.cbc_empty.as_ref()?, host, schema))
    };
    match prefix {
        b"v10" => with(keys.cbc_v10.as_ref()),
        b"v11" => with(keys.cbc_v11.as_ref()),
        _ => strip_domain_binding(encrypted, host, schema),
    }
}

/// `readChromiumCookies`: the key first, so a refused Keychain prompt is what the user sees.
pub(super) fn read(chromium: &Chromium, database: &Path, local_state: Option<&Path>) -> Result<CookieRead, Reason> {
    let keys = keys(chromium, local_state)?;
    let snapshot = Snapshot::open(database).map_err(|_| Reason::ReadFailed)?;
    read_database(snapshot.connection(), &keys, Platform::CURRENT)
}

/// `0` no restriction, `1` lax, `2` strict, `-1` unspecified.
fn same_site(column: i64) -> SameSite {
    match column {
        0 => SameSite::NoRestriction,
        1 => SameSite::Lax,
        2 => SameSite::Strict,
        _ => SameSite::Unspecified,
    }
}

struct Row {
    host: String,
    name: String,
    value: String,
    encrypted: Vec<u8>,
    path: String,
    expires: i64,
    secure: bool,
    http_only: bool,
    same_site: i64,
    /// Partitioned (CHIPS) cookies, which a WebView cannot store separately.
    partitioned: bool,
}

fn read_database(connection: &rusqlite::Connection, keys: &Keys, platform: Platform) -> Result<CookieRead, Reason> {
    let failed = |_| Reason::ReadFailed;
    let version: Value = connection
        .query_row("SELECT value FROM meta WHERE key = 'version' LIMIT 1", [], |row| row.get(0))
        .map_err(failed)?;
    let schema = match version {
        Value::Integer(version) => version,
        Value::Text(version) => version.trim().parse().map_err(|_| Reason::ReadFailed)?,
        _ => return Err(Reason::ReadFailed),
    };
    if schema < 0 {
        return Err(Reason::ReadFailed);
    }
    let partition = if schema >= 15 { "top_frame_site_key" } else { "''" };
    let sql = format!(
        "SELECT host_key, name, value, encrypted_value, path, expires_utc / 1000000, is_secure, is_httponly, \
         samesite, {partition} FROM cookies"
    );
    let mut statement = connection.prepare(&sql).map_err(failed)?;
    let rows = statement
        .query_map([], |row| {
            Ok(Row {
                host: row.get(0)?,
                name: row.get(1)?,
                value: row.get(2)?,
                encrypted: row.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
                path: row.get(4)?,
                expires: row.get(5)?,
                secure: row.get::<_, i64>(6)? == 1,
                http_only: row.get::<_, i64>(7)? == 1,
                same_site: row.get(8)?,
                partitioned: !row.get::<_, String>(9)?.is_empty(),
            })
        })
        .map_err(failed)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failed)?;

    let mut read = CookieRead::default();
    let mut undecryptable_hosts = HashSet::new();
    let mut skip = |read: &mut CookieRead, host: &str| {
        read.undecryptable += 1;
        let host = super::bare_host(host);
        if undecryptable_hosts.insert(host.to_owned()) {
            read.undecryptable_hosts.push(host.to_owned());
        }
    };
    for row in &rows {
        let value = if row.partitioned {
            None
        } else if row.encrypted.is_empty() {
            Some(row.value.clone())
        } else {
            decrypt_value(&row.encrypted, keys, &row.host, schema, platform)
        };
        let Some(value) = value else {
            skip(&mut read, &row.host);
            continue;
        };
        let (host, domain) = ImportedCookie::scoped(&row.host);
        read.cookies.push(ImportedCookie {
            host,
            domain,
            name: row.name.clone(),
            value,
            path: row.path.clone(),
            secure: row.secure,
            http_only: row.http_only,
            expires: (row.expires > 0).then(|| row.expires - WEBKIT_EPOCH_OFFSET),
            same_site: same_site(row.same_site),
        });
    }
    if read.cookies.is_empty()
        && let Some(reason) = keys.cbc_v11_error
        && rows.iter().any(|row| !row.partitioned && row.encrypted.starts_with(b"v11"))
    {
        return Err(reason);
    }
    Ok(read)
}

#[cfg(test)]
mod tests {
    use aes::cipher::BlockEncryptMut as _;

    use super::*;

    fn hex(bytes: &[u8]) -> String {
        crate::hex::lower_hex(bytes)
    }

    #[test]
    fn pbkdf2_matches_rfc_6070() {
        assert_eq!(hex(&pbkdf2_sha1(b"password", b"salt", 1)), "0c60c80f961f0e71f3a9b524af6012062fe037a6");
        assert_eq!(hex(&pbkdf2_sha1(b"password", b"salt", 2)), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");
        assert_eq!(hex(&pbkdf2_sha1(b"password", b"salt", 4096)), "4b007901b765489abead49d926f721d065a429c1");
        // RFC 2202's key longer than SHA-1's block.
        let hashed = hmac_sha1(&[0xaa; 80], &[b"Test Using Larger Than Block-Size Key - Hash Key First"]);
        assert_eq!(hex(&hashed), "aa4ae5e15272d00e95705637ce8a3b55ed402112");
    }

    fn encrypt_cbc(plaintext: &[u8], key: &[u8; 16]) -> Vec<u8> {
        let mut buffer = plaintext.to_vec();
        buffer.resize(plaintext.len() + 16, 0);
        let encryptor = cbc::Encryptor::<aes::Aes128>::new(key.into(), &CBC_IV.into());
        encryptor.encrypt_padded_mut::<Pkcs7>(&mut buffer, plaintext.len()).unwrap().to_vec()
    }

    fn bound(host: &str, value: &str) -> Vec<u8> {
        [Sha256::digest(host.as_bytes()).as_slice(), value.as_bytes()].concat()
    }

    #[test]
    fn cbc_values_open_with_their_version_key_or_the_empty_one() {
        let keys = linux_keys(Ok(b"keyring-secret".to_vec())).unwrap();
        let v10 = [b"v10".as_slice(), &encrypt_cbc(b"plain", &derive(b"peanuts", 1))].concat();
        let v11 =
            [b"v11".as_slice(), &encrypt_cbc(&bound(".example.com", "bound"), &derive(b"keyring-secret", 1))].concat();
        let empty = [b"v11".as_slice(), &encrypt_cbc(b"empty", &derive(b"", 1))].concat();
        let decrypt = |value: &[u8], schema| decrypt_value(value, &keys, ".example.com", schema, Platform::Linux);
        assert_eq!(decrypt(&v10, 23).as_deref(), Some("plain"));
        assert_eq!(decrypt(&v11, 24).as_deref(), Some("bound"), "the host's hash is stripped");
        assert_eq!(decrypt_value(&v11, &keys, "other.com", 24, Platform::Linux), None, "bound to another host");
        assert_eq!(decrypt(&empty, 23).as_deref(), Some("empty"), "crbug.com/1195256");
        assert_eq!(decrypt(b"", 23).as_deref(), Some(""));
        assert_eq!(decrypt(b"raw", 23).as_deref(), Some("raw"), "unprefixed values are plaintext");
        assert_eq!(decrypt_value(&v10, &keys, ".example.com", 23, Platform::Windows), None, "Windows uses GCM");

        let mac = Keys { cbc_v10: Some(derive(b"safe-storage", 1003)), ..Keys::default() };
        let v10 = [b"v10".as_slice(), &encrypt_cbc(b"mac", &derive(b"safe-storage", 1003))].concat();
        assert_eq!(decrypt_value(&v10, &mac, "a.com", 23, Platform::Mac).as_deref(), Some("mac"));
        assert_eq!(decrypt_value(&v11, &mac, "a.com", 23, Platform::Mac), None, "no v11 key on macOS");
    }

    #[test]
    fn windows_values_are_gcm_under_the_local_state_key() {
        use aes_gcm::aead::{Aead as _, KeyInit as _};
        use base64::Engine as _;
        let key = [7u8; 32];
        let nonce = [1u8; GCM_NONCE];
        let sealed = aes_gcm::Aes256Gcm::new((&key).into())
            .encrypt(&aes_gcm::Nonce::from(nonce), b"windows".as_slice())
            .unwrap();
        let value = [b"v10".as_slice(), &nonce, &sealed].concat();
        let keys = Keys { gcm_v10: Some(key), ..Keys::default() };
        assert_eq!(decrypt_value(&value, &keys, "a.com", 23, Platform::Windows).as_deref(), Some("windows"));
        let app_bound = [b"v20".as_slice(), &nonce, &sealed].concat();
        assert_eq!(decrypt_value(&app_bound, &keys, "a.com", 23, Platform::Windows), None);
        assert_eq!(decrypt_value(b"v10short", &keys, "a.com", 23, Platform::Windows), None);

        let wrapped = base64::engine::general_purpose::STANDARD.encode(b"DPAPIblob");
        let state = |extra: &str| format!(r#"{{"os_crypt":{{"encrypted_key":"{wrapped}"{extra}}}}}"#);
        assert_eq!(wrapped_windows_key(&state("")), Ok(b"blob".to_vec()));
        assert_eq!(wrapped_windows_key(&state(r#","app_bound_encrypted_key":"x""#)), Err(Reason::UnsupportedPlatform));
        assert_eq!(wrapped_windows_key("{}"), Err(Reason::ReadFailed));
    }

    #[test]
    fn linux_keyring_refusal_stops_the_import_but_a_missing_keyring_does_not() {
        assert_eq!(linux_keys(Err(Reason::NeedsKeychainApproval)).err(), Some(Reason::NeedsKeychainApproval));
        let keys = linux_keys(Err(Reason::KeychainUnavailable)).unwrap();
        assert!(keys.cbc_v10.is_some() && keys.cbc_v11.is_none());
        assert_eq!(keys.cbc_v11_error, Some(Reason::KeychainUnavailable));
    }

    fn cookie_database(schema: &str, rows: &[(&str, &str, Vec<u8>, i64, i64)]) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE meta(key TEXT, value TEXT); INSERT INTO meta VALUES ('version', '{schema}');
                 CREATE TABLE cookies(host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB, path TEXT,
                   expires_utc INTEGER, is_secure INTEGER, is_httponly INTEGER, samesite INTEGER,
                   top_frame_site_key TEXT);"
            ))
            .unwrap();
        for (host, name, encrypted, expires, same_site) in rows {
            let partition = if *name == "partitioned" { "https://top.example" } else { "" };
            connection
                .execute(
                    "INSERT INTO cookies VALUES (?1, ?2, 'clear', ?3, '/', ?4, 1, 0, ?5, ?6)",
                    rusqlite::params![host, name, encrypted, expires, same_site, partition],
                )
                .unwrap();
        }
        connection
    }

    #[test]
    fn databases_read_their_rows_and_count_what_could_not_be_opened() {
        let keys = linux_keys(Err(Reason::KeychainItemMissing)).unwrap();
        let v10 = [b"v10".as_slice(), &encrypt_cbc(b"secret", &derive(b"peanuts", 1))].concat();
        let expires = (WEBKIT_EPOCH_OFFSET + 1_700_000_000) * 1_000_000;
        let connection = cookie_database(
            "20",
            &[
                (".example.com", "sid", v10, expires, 1),
                ("host.example.com", "plain", Vec::new(), 0, -1),
                (".locked.com", "v11", b"v11xxxxxxxxxxxxxxxx".to_vec(), 0, 0),
                ("top.com", "partitioned", Vec::new(), 0, 0),
            ],
        );
        let read = read_database(&connection, &keys, Platform::Linux).unwrap();
        let sid = &read.cookies[0];
        assert_eq!(
            (sid.host.as_str(), sid.domain.as_deref(), sid.value.as_str()),
            ("example.com", Some(".example.com"), "secret")
        );
        assert_eq!((sid.expires, sid.same_site, sid.secure), (Some(1_700_000_000), SameSite::Lax, true));
        let plain = &read.cookies[1];
        assert_eq!((plain.domain.as_deref(), plain.value.as_str(), plain.expires), (None, "clear", None), "host-only");
        assert_eq!(plain.same_site, SameSite::Unspecified);
        assert_eq!(read.undecryptable, 2);
        assert_eq!(read.undecryptable_hosts, ["locked.com", "top.com"]);

        let only_v11 = cookie_database("24", &[(".locked.com", "v11", b"v11xxxxxxxxxxxxxxxx".to_vec(), 0, 0)]);
        assert_eq!(
            read_database(&only_v11, &keys, Platform::Linux).err(),
            Some(Reason::KeychainItemMissing),
            "nothing decrypted for want of the keyring's key"
        );
        let old = cookie_database("not a number", &[]);
        assert_eq!(read_database(&old, &keys, Platform::Linux).err(), Some(Reason::ReadFailed));
    }
}
