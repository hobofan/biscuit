//! JSON Web Algorithms
//!
//! Code for implementing JWA according to [RFC 7518](https://tools.ietf.org/html/rfc7518).
//!
//! Typically, you will not use these directly, but as part of a JWS or JWE.
use ring::{aead, digest, hmac, rand, signature};
use ring::constant_time::verify_slices_are_equal;
use ring::rand::SystemRandom;
use serde::Serialize;
use serde::de::DeserializeOwned;
use untrusted;

use errors::Error;
use jwk;
use jws::Secret;

/// AES GCM Tag Size, in bytes
const TAG_SIZE: usize = 128 / 8;
/// AES GCM Nonce length, in bytes
const NONCE_LENGTH: usize = 96 / 8;

#[derive(Debug, Eq, PartialEq, Copy, Clone, Serialize, Deserialize)]
/// Algorithms described by [RFC 7518](https://tools.ietf.org/html/rfc7518).
/// This enum is serialized `untagged`.
#[serde(untagged)]
pub enum Algorithm {
    /// Algorithms meant for Digital signature or MACs
    /// See [RFC7518#3](https://tools.ietf.org/html/rfc7518#section-3)
    Signature(SignatureAlgorithm),
    /// Algorithms meant for key management. The algorithms are either meant to
    /// encrypt a content encryption key or determine the content encryption key.
    /// See [RFC7518#4](https://tools.ietf.org/html/rfc7518#section-4)
    KeyManagement(KeyManagementAlgorithm),
    /// Algorithms meant for content encryption.
    /// See [RFC7518#5](https://tools.ietf.org/html/rfc7518#section-5)
    ContentEncryption(ContentEncryptionAlgorithm),
}

#[derive(Debug, Eq, PartialEq, Copy, Clone, Serialize, Deserialize)]
/// The algorithms supported for digital signature and MACs, defined by
/// [RFC7518#3](https://tools.ietf.org/html/rfc7518#section-3).
pub enum SignatureAlgorithm {
    /// No encryption/signature is included for the JWT.
    /// During verification, the signature _MUST BE_ empty or verification  will fail.
    #[serde(rename = "none")]
    None,
    /// HMAC using SHA-256
    HS256,
    /// HMAC using SHA-384
    HS384,
    /// HMAC using SHA-512
    HS512,
    /// RSASSA-PKCS1-v1_5 using SHA-256
    RS256,
    /// RSASSA-PKCS1-v1_5 using SHA-384
    RS384,
    /// RSASSA-PKCS1-v1_5 using SHA-512
    RS512,
    /// ECDSA using P-256 and SHA-256
    ES256,
    /// ECDSA using P-384 and SHA-384
    ES384,
    /// ECDSA using P-521 and SHA-512 --
    /// This variant is [unsupported](https://github.com/briansmith/ring/issues/268) and will probably never be.
    ES512,
    /// RSASSA-PSS using SHA-256 and MGF1 with SHA-256.
    /// The size of the salt value is the same size as the hash function output.
    PS256,
    /// RSASSA-PSS using SHA-384 and MGF1 with SHA-384
    /// The size of the salt value is the same size as the hash function output.
    PS384,
    /// RSASSA-PSS using SHA-512 and MGF1 with SHA-512
    /// The size of the salt value is the same size as the hash function output.
    PS512,
}

/// Algorithms for key management as defined in [RFC7518#4](https://tools.ietf.org/html/rfc7518#section-4)
#[derive(Debug, Eq, PartialEq, Copy, Clone, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum KeyManagementAlgorithm {
    /// RSAES-PKCS1-v1_5
    RSA1_5,
    /// RSAES OAEP using default parameters
    #[serde(rename = "RSA-OAEP")]
    RSA_OAEP,
    /// RSAES OAEP using SHA-256 and MGF1 with SHA-256
    #[serde(rename = "RSA-OAEP-256")]
    RSA_OAEP_256,
    /// AES Key Wrap using 128-bit key. _Unsupported_
    A128KW,
    /// AES Key Wrap using 192-bit key. _Unsupported_.
    /// This is [not supported](https://github.com/briansmith/ring/issues/112) by `ring`.
    A192KW,
    /// AES Key Wrap using 256-bit key. _Unsupported_
    A256KW,
    /// Direct use of a shared symmetric key
    #[serde(rename = "dir")]
    DirectSymmetricKey,
    /// ECDH-ES using Concat KDF
    #[serde(rename = "ECDH-ES")]
    ECDH_ES,
    /// ECDH-ES using Concat KDF and "A128KW" wrapping
    #[serde(rename = "ECDH-ES+A128KW")]
    ECDH_ES_A128KW,
    /// ECDH-ES using Concat KDF and "A192KW" wrapping
    #[serde(rename = "ECDH-ES+A192KW")]
    ECDH_ES_A192KW,
    /// ECDH-ES using Concat KDF and "A256KW" wrapping
    #[serde(rename = "ECDH-ES+A256KW")]
    ECDH_ES_A256KW,
    /// Key wrapping with AES GCM using 128-bit key	alg
    A128GCMKW,
    /// Key wrapping with AES GCM using 192-bit key alg.
    /// This is [not supported](https://github.com/briansmith/ring/issues/112) by `ring`.
    A192GCMKW,
    /// Key wrapping with AES GCM using 256-bit key	alg
    A256GCMKW,
    /// PBES2 with HMAC SHA-256 and "A128KW" wrapping
    #[serde(rename = "PBES2-HS256+A128KW")]
    PBES2_HS256_A128KW,
    /// PBES2 with HMAC SHA-384 and "A192KW" wrapping
    #[serde(rename = "PBES2-HS384+A192KW")]
    PBES2_HS384_A192KW,
    /// PBES2 with HMAC SHA-512 and "A256KW" wrapping
    #[serde(rename = "PBES2-HS512+A256KW")]
    PBES2_HS512_A256KW,
}

/// Describes the type of operations that the key management algorithm
/// supports with respect to a Content Encryption Key (CEK)
#[derive(Debug, Eq, PartialEq, Copy, Clone, Serialize, Deserialize)]
pub enum KeyManagementAlgorithmType {
    /// Wraps a randomly generated CEK using a symmetric encryption algorithm
    SymmetricKeyWrapping,
    /// Encrypt a randomly generated CEK using an asymmetric encryption algorithm,
    AsymmetricKeyEncryption,
    /// A key agreement algorithm to pick a CEK
    DirectKeyAgreement,
    /// A key agreement algorithm used to pick a symmetric CEK and wrap the CEK with a symmetric encryption algorithm
    KeyAgreementWithKeyWrapping,
    /// A user defined symmetric shared key is the CEK
    DirectEncryption,
}

/// Algorithms meant for content encryption.
/// See [RFC7518#5](https://tools.ietf.org/html/rfc7518#section-5)
#[derive(Debug, Eq, PartialEq, Copy, Clone, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum ContentEncryptionAlgorithm {
    /// AES_128_CBC_HMAC_SHA_256 authenticated encryption algorithm	enc
    #[serde(rename = "A128CBC-HS256")]
    A128CBC_HS256,
    /// AES_192_CBC_HMAC_SHA_384 authenticated encryption algorithm	enc
    #[serde(rename = "A192CBC-HS384")]
    A192CBC_HS384,
    /// AES_256_CBC_HMAC_SHA_512 authenticated encryption algorithm	enc
    #[serde(rename = "A256CBC-HS512")]
    A256CBC_HS512,
    /// AES GCM using 128-bit key
    A128GCM,
    /// AES GCM using 192-bit key
    /// This is [not supported](https://github.com/briansmith/ring/issues/112) by `ring`.
    A192GCM,
    /// AES GCM using 256-bit key
    A256GCM,
}

/// The result returned from an encryption operation
// TODO: Might have to turn this into an enum
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct EncryptionResult {
    /// The initialization vector, or nonce used in the encryption
    pub nonce: Vec<u8>,
    /// The encrypted payload
    pub encrypted: Vec<u8>,
    /// The authentication tag
    pub tag: Vec<u8>,
    /// Additional authenticated data that is integrity protected but not encrypted
    pub additional_data: Vec<u8>,
}

impl Default for SignatureAlgorithm {
    fn default() -> Self {
        SignatureAlgorithm::HS256
    }
}

impl Default for KeyManagementAlgorithm {
    fn default() -> Self {
        KeyManagementAlgorithm::DirectSymmetricKey
    }
}

impl Default for ContentEncryptionAlgorithm {
    fn default() -> Self {
        ContentEncryptionAlgorithm::A128GCM
    }
}

impl SignatureAlgorithm {
    /// Take some bytes and sign it according to the algorithm and secret provided.
    pub fn sign(&self, data: &[u8], secret: &Secret) -> Result<Vec<u8>, Error> {
        use self::SignatureAlgorithm::*;

        match *self {
            None => Self::sign_none(secret),
            HS256 | HS384 | HS512 => Self::sign_hmac(data, secret, self),
            RS256 | RS384 | RS512 | PS256 | PS384 | PS512 => Self::sign_rsa(data, secret, self),
            ES256 | ES384 | ES512 => Self::sign_ecdsa(data, secret, self),
        }
    }

    /// Verify signature based on the algorithm and secret provided.
    pub fn verify(&self, expected_signature: &[u8], data: &[u8], secret: &Secret) -> Result<bool, Error> {
        use self::SignatureAlgorithm::*;

        match *self {
            None => Self::verify_none(expected_signature, secret),
            HS256 | HS384 | HS512 => Self::verify_hmac(expected_signature, data, secret, self),
            RS256 | RS384 | RS512 | PS256 | PS384 | PS512 | ES256 | ES384 | ES512 => {
                Self::verify_public_key(expected_signature, data, secret, self)
            }
        }
    }

    /// Returns the type of operations the key is meant for
    fn sign_none(secret: &Secret) -> Result<Vec<u8>, Error> {
        match *secret {
            Secret::None => {}
            _ => Err("Invalid secret type. `None` should be provided".to_string())?,
        };
        Ok(vec![])
    }

    fn sign_hmac(data: &[u8], secret: &Secret, algorithm: &SignatureAlgorithm) -> Result<Vec<u8>, Error> {
        let secret = match *secret {
            Secret::Bytes(ref secret) => secret,
            _ => Err("Invalid secret type. A byte array is required".to_string())?,
        };

        let digest = match *algorithm {
            SignatureAlgorithm::HS256 => &digest::SHA256,
            SignatureAlgorithm::HS384 => &digest::SHA384,
            SignatureAlgorithm::HS512 => &digest::SHA512,
            _ => unreachable!("Should not happen"),
        };
        let key = hmac::SigningKey::new(digest, &secret);
        Ok(hmac::sign(&key, data).as_ref().to_vec())
    }

    fn sign_rsa(data: &[u8], secret: &Secret, algorithm: &SignatureAlgorithm) -> Result<Vec<u8>, Error> {
        let key_pair = match *secret {
            Secret::RSAKeyPair(ref key_pair) => key_pair,
            _ => Err("Invalid secret type. A RSAKeyPair is required".to_string())?,
        };
        let mut signing_state = signature::RSASigningState::new(key_pair.clone())?;
        let rng = rand::SystemRandom::new();
        let mut signature = vec![0; signing_state.key_pair().public_modulus_len()];
        let padding_algorithm: &signature::RSAEncoding = match *algorithm {
            SignatureAlgorithm::RS256 => &signature::RSA_PKCS1_SHA256,
            SignatureAlgorithm::RS384 => &signature::RSA_PKCS1_SHA384,
            SignatureAlgorithm::RS512 => &signature::RSA_PKCS1_SHA512,
            SignatureAlgorithm::PS256 => &signature::RSA_PSS_SHA256,
            SignatureAlgorithm::PS384 => &signature::RSA_PSS_SHA384,
            SignatureAlgorithm::PS512 => &signature::RSA_PSS_SHA512,
            _ => unreachable!("Should not happen"),
        };
        signing_state
            .sign(padding_algorithm, &rng, data, &mut signature)?;
        Ok(signature)
    }

    fn sign_ecdsa(_data: &[u8], _secret: &Secret, _algorithm: &SignatureAlgorithm) -> Result<Vec<u8>, Error> {
        // Not supported at the moment by ring
        // Tracking issues:
        //  - P-256: https://github.com/briansmith/ring/issues/207
        //  - P-384: https://github.com/briansmith/ring/issues/209
        //  - P-521: Probably never: https://github.com/briansmith/ring/issues/268
        Err(Error::UnsupportedOperation)
    }

    fn verify_none(expected_signature: &[u8], secret: &Secret) -> Result<bool, Error> {
        match *secret {
            Secret::None => {}
            _ => Err("Invalid secret type. `None` should be provided".to_string())?,
        };
        Ok(expected_signature.is_empty())
    }

    fn verify_hmac(expected_signature: &[u8],
                   data: &[u8],
                   secret: &Secret,
                   algorithm: &SignatureAlgorithm)
                   -> Result<bool, Error> {
        let actual_signature = Self::sign_hmac(data, secret, algorithm)?;
        Ok(verify_slices_are_equal(expected_signature.as_ref(), actual_signature.as_ref()).is_ok())
    }

    fn verify_public_key(expected_signature: &[u8],
                         data: &[u8],
                         secret: &Secret,
                         algorithm: &SignatureAlgorithm)
                         -> Result<bool, Error> {
        let public_key = match *secret {
            Secret::PublicKey(ref public_key) => public_key,
            _ => Err("Invalid secret type. A PublicKey is required".to_string())?,
        };
        let public_key_der = untrusted::Input::from(public_key.as_slice());

        let verification_algorithm: &signature::VerificationAlgorithm = match *algorithm {
            SignatureAlgorithm::RS256 => &signature::RSA_PKCS1_2048_8192_SHA256,
            SignatureAlgorithm::RS384 => &signature::RSA_PKCS1_2048_8192_SHA384,
            SignatureAlgorithm::RS512 => &signature::RSA_PKCS1_2048_8192_SHA512,
            SignatureAlgorithm::PS256 => &signature::RSA_PSS_2048_8192_SHA256,
            SignatureAlgorithm::PS384 => &signature::RSA_PSS_2048_8192_SHA384,
            SignatureAlgorithm::PS512 => &signature::RSA_PSS_2048_8192_SHA512,
            SignatureAlgorithm::ES256 => &signature::ECDSA_P256_SHA256_ASN1,
            SignatureAlgorithm::ES384 => &signature::ECDSA_P384_SHA384_ASN1,
            SignatureAlgorithm::ES512 => Err(Error::UnsupportedOperation)?,
            _ => unreachable!("Should not happen"),
        };

        let message = untrusted::Input::from(data);
        let expected_signature = untrusted::Input::from(expected_signature);
        match signature::verify(verification_algorithm,
                                public_key_der,
                                message,
                                expected_signature) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}

impl KeyManagementAlgorithm {
    /// Returns the type of operations that the algorithm is intended to support
    pub fn algorithm_type(&self) -> KeyManagementAlgorithmType {
        use self::KeyManagementAlgorithm::*;

        match *self {
            A128KW |
            A192KW |
            A256KW |
            A128GCMKW |
            A192GCMKW |
            A256GCMKW |
            PBES2_HS256_A128KW |
            PBES2_HS384_A192KW |
            PBES2_HS512_A256KW => KeyManagementAlgorithmType::SymmetricKeyWrapping,
            RSA1_5 | RSA_OAEP | RSA_OAEP_256 => KeyManagementAlgorithmType::AsymmetricKeyEncryption,
            DirectSymmetricKey => KeyManagementAlgorithmType::DirectEncryption,
            ECDH_ES => KeyManagementAlgorithmType::DirectKeyAgreement,
            ECDH_ES_A128KW | ECDH_ES_A192KW | ECDH_ES_A256KW => KeyManagementAlgorithmType::KeyAgreementWithKeyWrapping,
        }
    }

    /// Retrieve the Content Encryption Key (CEK) based on the algorithm for encryption
    pub fn cek<T>(&self, content_alg: ContentEncryptionAlgorithm, key: &jwk::JWK<T>) -> Result<jwk::JWK<::Empty>, Error>
        where T: Serialize + DeserializeOwned
    {
        use self::KeyManagementAlgorithm::*;

        match *self {
            DirectSymmetricKey => self.cek_direct(key),
            A128GCMKW | A256GCMKW => self.cek_aes_gcm(content_alg),
            _ => Err(Error::UnsupportedOperation),
        }
    }

    fn cek_direct<T>(&self, key: &jwk::JWK<T>) -> Result<jwk::JWK<::Empty>, Error>
        where T: Serialize + DeserializeOwned
    {
        match key.key_type() {
            jwk::KeyType::Octect => Ok(key.clone_without_additional()),
            others => Err(unexpected_key_type_error!(jwk::KeyType::Octect, others)),
        }
    }

    fn cek_aes_gcm(&self, content_alg: ContentEncryptionAlgorithm) -> Result<jwk::JWK<::Empty>, Error> {
        let key = content_alg.generate_key()?;
        Ok(jwk::JWK {
               algorithm: jwk::AlgorithmParameters::OctectKey {
                   value: key,
                   key_type: Default::default(),
               },
               common: jwk::CommonParameters {
                   public_key_use: Some(jwk::PublicKeyUse::Encryption),
                   algorithm: Some(Algorithm::ContentEncryption(content_alg)),
                   ..Default::default()
               },
               additional: Default::default(),
           })
    }

    /// Encrypt or wrap a key with the provided algorithm
    pub fn encrypt<T: Serialize + DeserializeOwned>(&self,
                                                    payload: &[u8],
                                                    key: &jwk::JWK<T>)
                                                    -> Result<EncryptionResult, Error> {
        use self::KeyManagementAlgorithm::*;

        match *self {
            A128GCMKW | A192GCMKW | A256GCMKW => self.aes_gcm_encrypt(payload, key),
            DirectSymmetricKey => Ok(Default::default()),
            _ => Err(Error::UnsupportedOperation),
        }
    }

    /// Decrypt or unwrap a CEK with the provided algorithm
    pub fn decrypt<T: Serialize + DeserializeOwned>(&self,
                                                    encrypted: &EncryptionResult,
                                                    content_alg: ContentEncryptionAlgorithm,
                                                    key: &jwk::JWK<T>)
                                                    -> Result<jwk::JWK<::Empty>, Error> {
        use self::KeyManagementAlgorithm::*;

        match *self {
            A128GCMKW | A192GCMKW | A256GCMKW => self.aes_gcm_decrypt(encrypted, content_alg, key),
            DirectSymmetricKey => Ok(key.clone_without_additional()),
            _ => Err(Error::UnsupportedOperation),
        }
    }

    fn aes_gcm_encrypt<T: Serialize + DeserializeOwned>(&self,
                                                        payload: &[u8],
                                                        key: &jwk::JWK<T>)
                                                        -> Result<EncryptionResult, Error> {
        use self::KeyManagementAlgorithm::*;

        let algorithm = match *self {
            A128GCMKW => &aead::AES_128_GCM,
            A256GCMKW => &aead::AES_256_GCM,
            _ => Err(Error::UnsupportedOperation)?,
        };
        aes_gcm_encrypt(algorithm, payload, &[], key)
    }

    fn aes_gcm_decrypt<T: Serialize + DeserializeOwned>(&self,
                                                        encrypted: &EncryptionResult,
                                                        content_alg: ContentEncryptionAlgorithm,
                                                        key: &jwk::JWK<T>)
                                                        -> Result<jwk::JWK<::Empty>, Error> {
        use self::KeyManagementAlgorithm::*;

        let algorithm = match *self {
            A128GCMKW => &aead::AES_128_GCM,
            A256GCMKW => &aead::AES_256_GCM,
            _ => Err(Error::UnsupportedOperation)?,
        };

        let cek = aes_gcm_decrypt(algorithm, encrypted, key)?;
        Ok(jwk::JWK {
               algorithm: jwk::AlgorithmParameters::OctectKey {
                   value: cek,
                   key_type: Default::default(),
               },
               common: jwk::CommonParameters {
                   public_key_use: Some(jwk::PublicKeyUse::Encryption),
                   algorithm: Some(Algorithm::ContentEncryption(content_alg)),
                   ..Default::default()
               },
               additional: Default::default(),
           })
    }
}

impl ContentEncryptionAlgorithm {
    /// Convenience function to generate a new random key with the required length
    pub fn generate_key(&self) -> Result<Vec<u8>, Error> {
        use self::ContentEncryptionAlgorithm::*;

        let length: usize = match *self {
            A128GCM => 128 / 8,
            A256GCM => 256 / 8,
            _ => Err(Error::UnsupportedOperation)?,
        };

        let mut key: Vec<u8> = vec![0; length];
        rng().fill(&mut key)?;
        Ok(key)
    }

    /// Encrypt some payload with the provided algorith
    pub fn encrypt<T: Serialize + DeserializeOwned>(&self,
                                                    payload: &[u8],
                                                    aad: &[u8],
                                                    key: &jwk::JWK<T>)
                                                    -> Result<EncryptionResult, Error> {
        use self::ContentEncryptionAlgorithm::*;

        match *self {
            A128GCM | A192GCM | A256GCM => self.aes_gcm_encrypt(payload, aad, key),
            _ => Err(Error::UnsupportedOperation),
        }

    }

    /// Decrypt some payload with the provided algorith,
    pub fn decrypt<T: Serialize + DeserializeOwned>(&self,
                                                    encrypted: &EncryptionResult,
                                                    key: &jwk::JWK<T>)
                                                    -> Result<Vec<u8>, Error> {
        use self::ContentEncryptionAlgorithm::*;

        match *self {
            A128GCM | A192GCM | A256GCM => self.aes_gcm_decrypt(encrypted, key),
            _ => Err(Error::UnsupportedOperation),
        }
    }

    fn aes_gcm_encrypt<T: Serialize + DeserializeOwned>(&self,
                                                        payload: &[u8],
                                                        aad: &[u8],
                                                        key: &jwk::JWK<T>)
                                                        -> Result<EncryptionResult, Error> {
        use self::ContentEncryptionAlgorithm::*;

        let algorithm = match *self {
            A128GCM => &aead::AES_128_GCM,
            A256GCM => &aead::AES_256_GCM,
            _ => Err(Error::UnsupportedOperation)?,
        };
        aes_gcm_encrypt(algorithm, payload, aad, key)
    }

    fn aes_gcm_decrypt<T: Serialize + DeserializeOwned>(&self,
                                                        encrypted: &EncryptionResult,
                                                        key: &jwk::JWK<T>)
                                                        -> Result<Vec<u8>, Error> {
        use self::ContentEncryptionAlgorithm::*;

        let algorithm = match *self {
            A128GCM => &aead::AES_128_GCM,
            A256GCM => &aead::AES_256_GCM,
            _ => Err(Error::UnsupportedOperation)?,
        };
        aes_gcm_decrypt(algorithm, encrypted, key)
    }
}

/// Return a psuedo random number generator
// FIXME: This should not be public
pub fn rng() -> &'static SystemRandom {
    use std::ops::Deref;

    lazy_static! {
        static ref RANDOM: SystemRandom = SystemRandom::new();
    }

    RANDOM.deref()
}

/// Encrypt a payload with AES GCM
fn aes_gcm_encrypt<T: Serialize + DeserializeOwned>(algorithm: &'static aead::Algorithm,
                                                    payload: &[u8],
                                                    aad: &[u8],
                                                    key: &jwk::JWK<T>)
                                                    -> Result<EncryptionResult, Error> {

    // JWA needs a 128 bit tag length. We need to assert that the algorithm has 128 bit tag length
    assert_eq!(algorithm.tag_len(), TAG_SIZE);
    // Also the nonce (or initialization vector) needs to be 96 bits
    assert_eq!(algorithm.nonce_len(), NONCE_LENGTH);

    let key = key.algorithm.octect_key()?;
    let sealing_key = aead::SealingKey::new(algorithm, key)?;

    let mut in_out: Vec<u8> = payload.to_vec();
    in_out.append(&mut vec![0; TAG_SIZE]);

    let mut nonce: Vec<u8> = vec![0; NONCE_LENGTH];
    rng().fill(&mut nonce)?;

    let size = aead::seal_in_place(&sealing_key, &nonce, aad, &mut in_out, TAG_SIZE)?;
    Ok(EncryptionResult {
           nonce: nonce,
           encrypted: in_out[0..(size - TAG_SIZE)].to_vec(),
           tag: in_out[(size - TAG_SIZE)..size].to_vec(),
           additional_data: aad.to_vec(),
       })
}

/// Decrypts a payload with AES GCM
fn aes_gcm_decrypt<T: Serialize + DeserializeOwned>(algorithm: &'static aead::Algorithm,
                                                    encrypted: &EncryptionResult,
                                                    key: &jwk::JWK<T>)
                                                    -> Result<Vec<u8>, Error> {
    // JWA needs a 128 bit tag length. We need to assert that the algorithm has 128 bit tag length
    assert_eq!(algorithm.tag_len(), TAG_SIZE);
    // Also the nonce (or initialization vector) needs to be 96 bits
    assert_eq!(algorithm.nonce_len(), NONCE_LENGTH);

    let key = key.algorithm.octect_key()?;
    let opening_key = aead::OpeningKey::new(algorithm, key)?;

    let mut in_out = encrypted.encrypted.to_vec();
    in_out.append(&mut encrypted.tag.to_vec());

    let plaintext = aead::open_in_place(&opening_key,
                                        &encrypted.nonce,
                                        &encrypted.additional_data,
                                        0,
                                        &mut in_out)?;
    Ok(plaintext.to_vec())
}

#[cfg(test)]
mod tests {
    use ring::constant_time::verify_slices_are_equal;

    use super::*;
    use CompactPart;
    use jwa;

    #[test]
    fn sign_and_verify_none() {
        let expected_signature: Vec<u8> = vec![];
        let actual_signature = not_err!(SignatureAlgorithm::None.sign("payload".to_string().as_bytes(), &Secret::None));
        assert_eq!(expected_signature, actual_signature);

        let valid = not_err!(SignatureAlgorithm::None.verify(vec![].as_slice(),
                                                             "payload".to_string().as_bytes(),
                                                             &Secret::None));
        assert!(valid);
    }

    #[test]
    fn sign_and_verify_hs256() {
        let expected_base64 = "uC_LeRrOxXhZuYm0MKgmSIzi5Hn9-SMmvQoug3WkK6Q";
        let expected_bytes: Vec<u8> = not_err!(CompactPart::from_base64(&expected_base64));

        let actual_signature = not_err!(SignatureAlgorithm::HS256.sign("payload".to_string().as_bytes(),
                                                                       &Secret::bytes_from_str("secret")));
        assert_eq!(&*not_err!(actual_signature.to_base64()), expected_base64);

        let valid = not_err!(SignatureAlgorithm::HS256.verify(expected_bytes.as_slice(),
                                                              "payload".to_string().as_bytes(),
                                                              &Secret::bytes_from_str("secret")));
        assert!(valid);
    }

    /// To generate the signature, use
    ///
    /// ```sh
    /// echo -n "payload" | openssl dgst -sha256 -sign test/fixtures/rsa_private_key.pem | base64
    /// ```
    ///
    /// The base64 encoding from this command will be in `STANDARD` form and not URL_SAFE.
    #[test]
    fn sign_and_verify_rs256() {
        let private_key = Secret::rsa_keypair_from_file("test/fixtures/rsa_private_key.der").unwrap();
        let payload = "payload".to_string();
        let payload_bytes = payload.as_bytes();
        // This is standard base64
        let expected_signature = "JIHqiBfUknrFPDLT0gxyoufD06S43ZqWN_PzQqHZqQ-met7kZmkSTYB_rUyotLMxlKkuXdnvKmWm\
                                  dwGAHWEwDvb5392pCmAAtmUIl6LormxJptWYb2PoF5jmtX_lwV8y4RYIh54Ai51162VARQCKAsxL\
                                  uH772MEChkcpjd31NWzaePWoi_IIk11iqy6uFWmbLLwzD_Vbpl2C6aHR3vQjkXZi05gA3zksjYAh\
                                  j-m7GgBt0UFOE56A4USjhQwpb4g3NEamgp51_kZ2ULi4Aoo_KJC6ynIm_pR6rEzBgwZjlCUnE-6o\
                                  5RPQZ8Oau03UDVH2EwZe-Q91LaWRvkKjGg5Tcw";
        let expected_signature_bytes: Vec<u8> = not_err!(CompactPart::from_base64(&expected_signature));

        let actual_signature = not_err!(SignatureAlgorithm::RS256.sign(payload_bytes, &private_key));
        assert_eq!(&*not_err!(actual_signature.to_base64()), expected_signature);

        let public_key = Secret::public_key_from_file("test/fixtures/rsa_public_key.der").unwrap();
        let valid = not_err!(SignatureAlgorithm::RS256.verify(expected_signature_bytes.as_slice(),
                                                              payload_bytes,
                                                              &public_key));
        assert!(valid);
    }

    /// This signature is non-deterministic.
    #[test]
    fn sign_and_verify_ps256_round_trip() {
        let private_key = Secret::rsa_keypair_from_file("test/fixtures/rsa_private_key.der").unwrap();
        let payload = "payload".to_string();
        let payload_bytes = payload.as_bytes();

        let actual_signature = not_err!(SignatureAlgorithm::PS256.sign(payload_bytes, &private_key));

        let public_key = Secret::public_key_from_file("test/fixtures/rsa_public_key.der").unwrap();
        let valid = not_err!(SignatureAlgorithm::PS256.verify(actual_signature.as_slice(), payload_bytes, &public_key));
        assert!(valid);
    }

    /// To generate a (non-deterministic) signature:
    ///
    /// ```sh
    /// echo -n "payload" | openssl dgst -sha256 -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:-1 \
    ///    -sign test/fixtures/rsa_private_key.pem | base64
    /// ```
    ///
    /// The base64 encoding from this command will be in `STANDARD` form and not URL_SAFE.
    #[test]
    fn verify_ps256() {
        use data_encoding::base64;

        let payload = "payload".to_string();
        let payload_bytes = payload.as_bytes();
        let signature = "TiMXtt3Wmv/a/tbLWuJPDlFYMfuKsD7U5lbBUn2mBu8DLMLj1EplEZNmkB8w65BgUijnu9hxmhwv\
                         ET2k7RrsYamEst6BHZf20hIK1yE/YWaktbVmAZwUDdIpXYaZn8ukTsMT06CDrVk6RXF0EPMaSL33\
                         tFNPZpz4/3pYQdxco/n6DpaR5206wsur/8H0FwoyiFKanhqLb1SgZqyc+SXRPepjKc28wzBnfWl4\
                         mmlZcJ2xk8O2/t1Y1/m/4G7drBwOItNl7EadbMVCetYnc9EILv39hjcL9JvaA9q0M2RB75DIu8SF\
                         9Kr/l+wzUJjWAHthgqSBpe15jLkpO8tvqR89fw==";
        let signature_bytes: Vec<u8> = not_err!(base64::decode(signature.as_bytes()));
        let public_key = Secret::public_key_from_file("test/fixtures/rsa_public_key.der").unwrap();
        let valid = not_err!(SignatureAlgorithm::PS256.verify(signature_bytes.as_slice(), payload_bytes, &public_key));
        assert!(valid);
    }

    #[test]
    #[should_panic(expected = "UnsupportedOperation")]
    fn sign_ecdsa() {
        let private_key = Secret::Bytes("secret".to_string().into_bytes()); // irrelevant
        let payload = "payload".to_string();
        let payload_bytes = payload.as_bytes();

        SignatureAlgorithm::ES256
            .sign(payload_bytes, &private_key)
            .unwrap();
    }

    /// Test case from https://github.com/briansmith/ring/blob/c5b8113/src/ec/suite_b/ecdsa_verify_tests.txt#L248
    #[test]
    fn verify_es256() {
        use data_encoding::hex;

        let payload = "sample".to_string();
        let payload_bytes = payload.as_bytes();
        let public_key = "0460FED4BA255A9D31C961EB74C6356D68C049B8923B61FA6CE669622E60F29FB67903FE1008B8BC99A41AE9E9562\
                          8BC64F2F1B20C2D7E9F5177A3C294D4462299";
        let public_key = Secret::PublicKey(not_err!(hex::decode(public_key.as_bytes())));
        let signature = "3046022100EFD48B2AACB6A8FD1140DD9CD45E81D69D2C877B56AAF991C34D0EA84EAF3716022100F7CB1C942D657C\
                         41D436C7A1B6E29F65F3E900DBB9AFF4064DC4AB2F843ACDA8";
        let signature_bytes: Vec<u8> = not_err!(hex::decode(signature.as_bytes()));
        let valid = not_err!(SignatureAlgorithm::ES256.verify(signature_bytes.as_slice(), payload_bytes, &public_key));
        assert!(valid);
    }

    /// Test case from https://github.com/briansmith/ring/blob/c5b8113/src/ec/suite_b/ecdsa_verify_tests.txt#L283
    #[test]
    fn verify_es384() {
        use data_encoding::hex;

        let payload = "sample".to_string();
        let payload_bytes = payload.as_bytes();
        let public_key = "04EC3A4E415B4E19A4568618029F427FA5DA9A8BC4AE92E02E06AAE5286B300C64DEF8F0EA9055866064A25451548\
                          0BC138015D9B72D7D57244EA8EF9AC0C621896708A59367F9DFB9F54CA84B3F1C9DB1288B231C3AE0D4FE7344FD25\
                          33264720";
        let public_key = Secret::PublicKey(not_err!(hex::decode(public_key.as_bytes())));
        let signature = "306602310094EDBB92A5ECB8AAD4736E56C691916B3F88140666CE9FA73D64C4EA95AD133C81A648152E44ACF96E36\
                         DD1E80FABE4602310099EF4AEB15F178CEA1FE40DB2603138F130E740A19624526203B6351D0A3A94FA329C145786E\
                         679E7B82C71A38628AC8";
        let signature_bytes: Vec<u8> = not_err!(hex::decode(signature.as_bytes()));
        let valid = not_err!(SignatureAlgorithm::ES384.verify(signature_bytes.as_slice(), payload_bytes, &public_key));
        assert!(valid);
    }

    #[test]
    #[should_panic(expected = "UnsupportedOperation")]
    fn verify_es512() {
        let payload: Vec<u8> = vec![];
        let signature: Vec<u8> = vec![];
        let public_key = Secret::PublicKey(vec![]);
        SignatureAlgorithm::ES512
            .verify(signature.as_slice(), payload.as_slice(), &public_key)
            .unwrap();
    }

    #[test]
    fn invalid_none() {
        let invalid_signature = "broken".to_string();
        let signature_bytes = invalid_signature.as_bytes();
        let valid = not_err!(SignatureAlgorithm::None.verify(signature_bytes,
                                                             "payload".to_string().as_bytes(),
                                                             &Secret::None));
        assert!(!valid);
    }

    #[test]
    fn invalid_hs256() {
        let invalid_signature = "broken".to_string();
        let signature_bytes = invalid_signature.as_bytes();
        let valid = not_err!(SignatureAlgorithm::HS256.verify(signature_bytes,
                                                              "payload".to_string().as_bytes(),
                                                              &Secret::Bytes("secret".to_string().into_bytes())));
        assert!(!valid);
    }

    #[test]
    fn invalid_rs256() {
        let public_key = Secret::public_key_from_file("test/fixtures/rsa_public_key.der").unwrap();
        let invalid_signature = "broken".to_string();
        let signature_bytes = invalid_signature.as_bytes();
        let valid = not_err!(SignatureAlgorithm::RS256.verify(signature_bytes,
                                                              "payload".to_string().as_bytes(),
                                                              &public_key));
        assert!(!valid);
    }

    #[test]
    fn invalid_ps256() {
        let public_key = Secret::public_key_from_file("test/fixtures/rsa_public_key.der").unwrap();
        let invalid_signature = "broken".to_string();
        let signature_bytes = invalid_signature.as_bytes();
        let valid = not_err!(SignatureAlgorithm::PS256.verify(signature_bytes,
                                                              "payload".to_string().as_bytes(),
                                                              &public_key));
        assert!(!valid);
    }

    #[test]
    fn invalid_es256() {
        let public_key = Secret::public_key_from_file("test/fixtures/rsa_public_key.der").unwrap();
        let invalid_signature = "broken".to_string();
        let signature_bytes = invalid_signature.as_bytes();
        let valid = not_err!(SignatureAlgorithm::ES256.verify(signature_bytes,
                                                              "payload".to_string().as_bytes(),
                                                              &public_key));
        assert!(!valid);
    }

    #[test]
    fn rng_is_created() {
        let rng = rng();
        let mut random: Vec<u8> = vec![0; 8];
        rng.fill(&mut random).unwrap();
    }

    #[test]
    fn aes_gcm_128_encryption_round_trip() {
        const PAYLOAD: &'static str = "这个世界值得我们奋战！";
        let mut key: Vec<u8> = vec![0; 128/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let encrypted = not_err!(aes_gcm_encrypt(&aead::AES_128_GCM, PAYLOAD.as_bytes(), &vec![], &key));
        let decrypted = not_err!(aes_gcm_decrypt(&aead::AES_128_GCM, &encrypted, &key));

        let payload = not_err!(String::from_utf8(decrypted));
        assert_eq!(payload, PAYLOAD);
    }

    #[test]
    fn aes_gcm_256_encryption_round_trip() {
        const PAYLOAD: &'static str = "这个世界值得我们奋战！";
        let mut key: Vec<u8> = vec![0; 256/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let encrypted = not_err!(aes_gcm_encrypt(&aead::AES_256_GCM, PAYLOAD.as_bytes(), &vec![], &key));
        let decrypted = not_err!(aes_gcm_decrypt(&aead::AES_256_GCM, &encrypted, &key));

        let payload = not_err!(String::from_utf8(decrypted));
        assert_eq!(payload, PAYLOAD);
    }

    /// `KeyManagementAlgorithm::DirectSymmetricKey` returns the same key when CEK is requested
    #[test]
    fn dir_cek_returns_provided_key() {
        let mut key: Vec<u8> = vec![0; 256/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let cek_alg = KeyManagementAlgorithm::DirectSymmetricKey;
        let cek = not_err!(cek_alg.cek(jwa::ContentEncryptionAlgorithm::A256GCM, &key));

        assert!(verify_slices_are_equal(cek.octect_key().unwrap(), key.octect_key().unwrap()).is_ok());
    }

    /// `KeyManagementAlgorithm::A128GCMKW` returns a random key with the right length when CEK is requested
    #[test]
    fn cek_aes128gcmkw_returns_right_key_length() {
        let mut key: Vec<u8> = vec![0; 128/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let cek_alg = KeyManagementAlgorithm::A128GCMKW;
        let cek = not_err!(cek_alg.cek(jwa::ContentEncryptionAlgorithm::A128GCM, &key));
        assert_eq!(cek.octect_key().unwrap().len(), 128 / 8);
        assert!(verify_slices_are_equal(cek.octect_key().unwrap(), key.octect_key().unwrap()).is_err());

        let cek = not_err!(cek_alg.cek(jwa::ContentEncryptionAlgorithm::A256GCM, &key));
        assert_eq!(cek.octect_key().unwrap().len(), 256 / 8);
        assert!(verify_slices_are_equal(cek.octect_key().unwrap(), key.octect_key().unwrap()).is_err());
    }

    /// `KeyManagementAlgorithm::A256GCMKW` returns a random key with the right length when CEK is requested
    #[test]
    fn cek_aes256gcmkw_returns_right_key_length() {
        let mut key: Vec<u8> = vec![0; 256/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let cek_alg = KeyManagementAlgorithm::A256GCMKW;
        let cek = not_err!(cek_alg.cek(jwa::ContentEncryptionAlgorithm::A128GCM, &key));
        assert_eq!(cek.octect_key().unwrap().len(), 128 / 8);
        assert!(verify_slices_are_equal(cek.octect_key().unwrap(), key.octect_key().unwrap()).is_err());

        let cek = not_err!(cek_alg.cek(jwa::ContentEncryptionAlgorithm::A256GCM, &key));
        assert_eq!(cek.octect_key().unwrap().len(), 256 / 8);
        assert!(verify_slices_are_equal(cek.octect_key().unwrap(), key.octect_key().unwrap()).is_err());
    }

    #[test]
    fn aes128gcmkw_key_encryption_round_trip() {
        let mut key: Vec<u8> = vec![0; 128/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let cek_alg = KeyManagementAlgorithm::A128GCMKW;
        let enc_alg = jwa::ContentEncryptionAlgorithm::A128GCM; // determines the CEK
        let cek = not_err!(cek_alg.cek(enc_alg, &key));

        let encrypted_cek = not_err!(cek_alg.encrypt(cek.octect_key().unwrap(), &key));
        let decrypted_cek = not_err!(cek_alg.decrypt(&encrypted_cek, enc_alg, &key));

        assert!(verify_slices_are_equal(cek.octect_key().unwrap(),
                                        decrypted_cek.octect_key().unwrap())
                        .is_ok());
    }

    #[test]
    fn aes256gcmkw_key_encryption_round_trip() {
        let mut key: Vec<u8> = vec![0; 256/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let cek_alg = KeyManagementAlgorithm::A256GCMKW;
        let enc_alg = jwa::ContentEncryptionAlgorithm::A128GCM; // determines the CEK
        let cek = not_err!(cek_alg.cek(enc_alg, &key));

        let encrypted_cek = not_err!(cek_alg.encrypt(cek.octect_key().unwrap(), &key));
        let decrypted_cek = not_err!(cek_alg.decrypt(&encrypted_cek, enc_alg, &key));

        assert!(verify_slices_are_equal(cek.octect_key().unwrap(),
                                        decrypted_cek.octect_key().unwrap())
                        .is_ok());
    }

    /// `ContentEncryptionAlgorithm::A128GCM` generates CEK of the right length
    #[test]
    fn aes128gcm_key_length() {
        let enc_alg = jwa::ContentEncryptionAlgorithm::A128GCM;
        let cek = not_err!(enc_alg.generate_key());
        assert_eq!(cek.len(), 128 / 8);
    }

    /// `ContentEncryptionAlgorithm::A256GCM` generates CEK of the right length
    #[test]
    fn aes256gcm_key_length() {
        let enc_alg = jwa::ContentEncryptionAlgorithm::A256GCM;
        let cek = not_err!(enc_alg.generate_key());
        assert_eq!(cek.len(), 256 / 8);
    }

    #[test]
    fn aes128gcm_encryption_round_trip() {
        let mut key: Vec<u8> = vec![0; 128/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let payload = "狼よ、我が敵を食らえ！";
        let aad = "My servants never die!";
        let enc_alg = jwa::ContentEncryptionAlgorithm::A128GCM;
        let encrypted_payload = not_err!(enc_alg.encrypt(payload.as_bytes(), aad.as_bytes(), &key));

        let decrypted_payload = not_err!(enc_alg.decrypt(&encrypted_payload, &key));
        assert!(verify_slices_are_equal(payload.as_bytes(), &decrypted_payload).is_ok());
    }

    #[test]
    fn aes1256gcm_encryption_round_trip() {
        let mut key: Vec<u8> = vec![0; 256/8];
        not_err!(rng().fill(&mut key));

        let key = jwk::JWK::<::Empty> {
            common: Default::default(),
            additional: Default::default(),
            algorithm: jwk::AlgorithmParameters::OctectKey {
                key_type: Default::default(),
                value: key,
            },
        };

        let payload = "狼よ、我が敵を食らえ！";
        let aad = "My servants never die!";
        let enc_alg = jwa::ContentEncryptionAlgorithm::A256GCM;
        let encrypted_payload = not_err!(enc_alg.encrypt(payload.as_bytes(), aad.as_bytes(), &key));

        let decrypted_payload = not_err!(enc_alg.decrypt(&encrypted_payload, &key));
        assert!(verify_slices_are_equal(payload.as_bytes(), &decrypted_payload).is_ok());
    }
}
