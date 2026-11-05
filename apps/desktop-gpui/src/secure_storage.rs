//! Secrets at rest, kept the way Electron's `safeStorage` keeps them: Windows encrypts
//! with DPAPI for the current user; macOS and Linux encrypt with AES-256-GCM under a
//! random key that lives in the login Keychain or the Secret Service (GNOME Keyring,
//! KWallet), so only this user's unlocked session can read the stored data.
//!
//! The Electron app's encrypted files are its own (`saved-environments.json`), so this
//! format only needs to read what this app wrote.

/// Encrypts `data` for the current user.
///
/// # Errors
///
/// When the platform's credential store is unavailable or refuses access.
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    imp::protect(data)
}

/// Decrypts what [`protect`] returned.
///
/// # Errors
///
/// When the credential store is unavailable, or `data` was not written by this user's key.
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    imp::unprotect(data)
}

#[cfg(windows)]
mod imp {
    use windows_dpapi::{Scope, decrypt_data, encrypt_data};

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        encrypt_data(data, Scope::User, None).map_err(|error| format!("{error:#}"))
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        decrypt_data(data, Scope::User, None).map_err(|error| format!("{error:#}"))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::Mutex;

    use super::sealed::{self, Key};

    /// The Keychain / Secret Service item holding the key. It is not Electron's
    /// `T3 Code Safe Storage` item, so neither app overwrites the other's key.
    const SERVICE: &str = "T3 Code Native Safe Storage";
    const ACCOUNT: &str = "T3 Code";

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        sealed::seal(&key()?, data)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        sealed::open(&key()?, data)
    }

    /// Reads the key once per run (macOS may ask the user to allow it); a refusal is not
    /// cached, so unlocking the keyring later works without a restart.
    fn key() -> Result<Key, String> {
        static KEY: Mutex<Option<Key>> = Mutex::new(None);
        let mut cached = KEY.lock().expect("safe storage key lock poisoned");
        if let Some(key) = *cached {
            return Ok(key);
        }
        let key = load_or_create()?;
        *cached = Some(key);
        Ok(key)
    }

    fn load_or_create() -> Result<Key, String> {
        use keyring_core::api::CredentialStoreApi as _;
        #[cfg(target_os = "macos")]
        let store = apple_native_keyring_store::keychain::Store::new().map_err(describe)?;
        #[cfg(not(target_os = "macos"))]
        let store = zbus_secret_service_keyring_store::Store::new().map_err(describe)?;
        let entry = store.build(SERVICE, ACCOUNT, None).map_err(describe)?;
        match entry.get_secret() {
            Ok(secret) => {
                Key::try_from(secret.as_slice()).map_err(|_| "The stored safe storage key is invalid.".into())
            }
            Err(keyring_core::Error::NoEntry) => {
                let key = sealed::random_key()?;
                entry.set_secret(&key).map_err(describe)?;
                Ok(key)
            }
            Err(error) => Err(describe(error)),
        }
    }

    fn describe(error: keyring_core::Error) -> String {
        let store = if cfg!(target_os = "macos") { "the Keychain" } else { "the Secret Service keyring" };
        format!("Could not use {store}: {error}")
    }
}

/// AES-256-GCM with a random nonce: `VERSION || nonce || ciphertext+tag`.
#[cfg(any(not(windows), test))]
mod sealed {
    use aes_gcm::aead::{Aead as _, KeyInit as _};
    use aes_gcm::{Aes256Gcm, Nonce};

    pub type Key = [u8; 32];

    const VERSION: &[u8] = b"g1";
    const NONCE_LEN: usize = 12;

    pub fn random_key() -> Result<Key, String> {
        let mut key = [0; 32];
        getrandom::fill(&mut key).map_err(|error| error.to_string())?;
        Ok(key)
    }

    pub fn seal(key: &Key, data: &[u8]) -> Result<Vec<u8>, String> {
        let mut nonce = [0; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        let ciphertext = Aes256Gcm::new(key.into())
            .encrypt(&Nonce::from(nonce), data)
            .map_err(|_| "Could not encrypt the secret.".to_owned())?;
        Ok([VERSION, &nonce, &ciphertext].concat())
    }

    pub fn open(key: &Key, data: &[u8]) -> Result<Vec<u8>, String> {
        let invalid = || "The stored secret is unreadable.".to_owned();
        let rest = data.strip_prefix(VERSION).ok_or_else(invalid)?;
        let (nonce, ciphertext) = rest.split_first_chunk::<NONCE_LEN>().ok_or_else(invalid)?;
        Aes256Gcm::new(key.into()).decrypt(&Nonce::from(*nonce), ciphertext).map_err(|_| invalid())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn sealed_secrets_open_only_with_their_key() {
            let key = random_key().unwrap();
            let sealed = seal(&key, b"bearer-token").unwrap();
            assert!(sealed.starts_with(VERSION));
            assert!(!sealed.windows(12).any(|w| w == b"bearer-token"), "the secret is not stored in the clear");
            assert_eq!(open(&key, &sealed).unwrap(), b"bearer-token");
            assert_ne!(seal(&key, b"bearer-token").unwrap(), sealed, "each seal takes a fresh nonce");

            let other = random_key().unwrap();
            assert!(open(&other, &sealed).is_err(), "another key cannot read it");
            let mut tampered = sealed;
            *tampered.last_mut().unwrap() ^= 1;
            assert!(open(&key, &tampered).is_err(), "a changed byte fails authentication");
            assert!(open(&key, b"g1short").is_err());
            assert!(open(&key, b"").is_err());
        }
    }
}
