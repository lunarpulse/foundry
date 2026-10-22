//! Product credential policy shared by product identity adapters.
//! Parameters are initial documented resource settings, not calibrated claims.

pub struct ProductPasswordKdf;

fn slots() -> &'static (std::sync::Mutex<usize>, std::sync::Condvar) {
    static SLOTS: std::sync::OnceLock<(std::sync::Mutex<usize>, std::sync::Condvar)> =
        std::sync::OnceLock::new();
    SLOTS.get_or_init(|| (std::sync::Mutex::new(0), std::sync::Condvar::new()))
}

struct KdfPermit;
impl KdfPermit {
    fn acquire() -> Self {
        let (used, ready) = slots();
        let mut active = used.lock().unwrap_or_else(|e| e.into_inner());
        while *active >= 2 {
            active = ready.wait(active).unwrap_or_else(|e| e.into_inner());
        }
        *active += 1;
        Self
    }
}
impl Drop for KdfPermit {
    fn drop(&mut self) {
        let (used, ready) = slots();
        let mut active = used.lock().unwrap_or_else(|e| e.into_inner());
        *active = active.saturating_sub(1);
        ready.notify_one();
    }
}

impl ProductPasswordKdf {
    pub fn validate_phc(phc: &str) -> bool {
        use argon2::password_hash::PasswordHash;
        let Ok(parsed) = PasswordHash::new(phc) else {
            return false;
        };
        parsed.algorithm.as_str() == "argon2id"
            && parsed.version == Some(argon2::Version::V0x13.into())
            && parsed.params.get_decimal("m") == Some(65_536)
            && parsed.params.get_decimal("t") == Some(3)
            && parsed.params.get_decimal("p") == Some(1)
            && parsed.salt.is_some()
            && parsed.hash.is_some()
    }

    pub fn hash(password: &str) -> Result<String, String> {
        use argon2::PasswordHasher as _;
        use argon2::password_hash::SaltString;
        use rand_core::RngCore as _;

        let _permit = KdfPermit::acquire();
        let mut salt_bytes = [0u8; 16];
        rand_core::OsRng
            .try_fill_bytes(&mut salt_bytes)
            .map_err(|e| format!("secure random source: {e}"))?;
        let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| e.to_string())?;
        configured_argon2()
            .hash_password(password.as_bytes(), &salt)
            .map(|phc| phc.to_string())
            .map_err(|e| e.to_string())
    }

    pub fn verify(password: &str, phc: &str) -> Result<bool, String> {
        use argon2::password_hash::{PasswordHash, PasswordVerifier};
        let Ok(parsed) = PasswordHash::new(phc) else {
            return Ok(false);
        };
        if parsed.algorithm.as_str() != "argon2id"
            || parsed.version != Some(argon2::Version::V0x13.into())
            || parsed.params.get_decimal("m") != Some(65_536)
            || parsed.params.get_decimal("t") != Some(3)
            || parsed.params.get_decimal("p") != Some(1)
        {
            return Ok(false);
        }
        let _permit = KdfPermit::acquire();
        match configured_argon2().verify_password(password.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }
}

fn configured_argon2() -> argon2::Argon2<'static> {
    let params =
        argon2::Params::new(65_536, 3, 1, Some(32)).expect("constant Argon2 parameters are valid");
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
}
