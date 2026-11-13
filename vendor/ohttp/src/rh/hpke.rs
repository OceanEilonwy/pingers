use std::ops::Deref;

use ::hpke as rust_hpke;
use ::hpke::inout::InOutBuf;
use hpke::kem::DhP256HkdfSha256;
use rust_hpke::{
    Deserializable, OpModeR, OpModeS, Serializable,
    aead::{AeadCtxR, AeadCtxS, AeadTag, AesGcm128, ChaCha20Poly1305},
    kdf::HkdfSha256,
    kem::{Kem as KemTrait, X25519HkdfSha256, XWing},
    setup_receiver, setup_sender_with_rng,
};

use super::SymKey;
use crate::{
    Error, Res,
    crypto::{Decrypt, Encrypt},
    hpke::{Aead, Kdf, Kem},
};

/// Configuration for `Hpke`.
#[derive(Clone, Copy)]
pub struct Config {
    kem: Kem,
    kdf: Kdf,
    aead: Aead,
}

impl Config {
    pub fn new(kem: Kem, kdf: Kdf, aead: Aead) -> Self {
        Self { kem, kdf, aead }
    }

    pub fn kem(self) -> Kem {
        self.kem
    }

    pub fn kdf(self) -> Kdf {
        self.kdf
    }

    pub fn aead(self) -> Aead {
        self.aead
    }

    pub fn supported(self) -> bool {
        matches!(self.kem, Kem::X25519Sha256 | Kem::P256Sha256 | Kem::XWing)
            && self.kdf == Kdf::HkdfSha256
            && matches!(self.aead, Aead::Aes128Gcm | Aead::ChaCha20Poly1305)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            kem: Kem::X25519Sha256,
            kdf: Kdf::HkdfSha256,
            aead: Aead::Aes128Gcm,
        }
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum PublicKey {
    X25519(<X25519HkdfSha256 as KemTrait>::PublicKey),
    P256(<DhP256HkdfSha256 as KemTrait>::PublicKey),
    XWing(<XWing as KemTrait>::PublicKey),
}

impl PublicKey {
    #[allow(clippy::unnecessary_wraps)]
    pub fn key_data(&self) -> Res<Vec<u8>> {
        Ok(match self {
            Self::X25519(k) => Vec::from(k.to_bytes().as_slice()),
            Self::P256(k) => Vec::from(k.to_bytes().as_slice()),
            Self::XWing(k) => Vec::from(k.to_bytes().as_slice()),
        })
    }
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if let Ok(b) = self.key_data() {
            write!(f, "PublicKey {}", hex::encode(b))
        } else {
            write!(f, "Opaque PublicKey")
        }
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum PrivateKey {
    X25519(<X25519HkdfSha256 as KemTrait>::PrivateKey),
    P256(<DhP256HkdfSha256 as KemTrait>::PrivateKey),
    XWing(<XWing as KemTrait>::PrivateKey),
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        // Pingers patch: the unsafe-print-secrets feature is removed.
        write!(f, "Opaque PrivateKey")
    }
}

// TODO: Use macros here.  To do that, we needs concat_ident!(), but it's not ready.
// This is what a macro that uses concat_ident!() might produce, written out in full.
enum SenderContextX25519HkdfSha256HkdfSha256 {
    AesGcm128(Box<AeadCtxS<AesGcm128, HkdfSha256, X25519HkdfSha256>>),
    ChaCha20Poly1305(Box<AeadCtxS<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>>),
}

enum SenderContextP256HkdfSha256HkdfSha256 {
    AesGcm128(Box<AeadCtxS<AesGcm128, HkdfSha256, DhP256HkdfSha256>>),
    ChaCha20Poly1305(Box<AeadCtxS<ChaCha20Poly1305, HkdfSha256, DhP256HkdfSha256>>),
}

enum SenderContextXWingHkdfSha256HkdfSha256 {
    AesGcm128(Box<AeadCtxS<AesGcm128, HkdfSha256, XWing>>),
    ChaCha20Poly1305(Box<AeadCtxS<ChaCha20Poly1305, HkdfSha256, XWing>>),
}

enum SenderContextX25519HkdfSha256 {
    HkdfSha256(SenderContextX25519HkdfSha256HkdfSha256),
}

enum SenderContextP256HkdfSha256 {
    HkdfSha256(SenderContextP256HkdfSha256HkdfSha256),
}

enum SenderContextXWingHkdfSha256 {
    HkdfSha256(SenderContextXWingHkdfSha256HkdfSha256),
}

enum SenderContext {
    X25519HkdfSha256(SenderContextX25519HkdfSha256),
    P256HkdfSha256(SenderContextP256HkdfSha256),
    XWingHkdfSha256(SenderContextXWingHkdfSha256),
}

impl SenderContext {
    fn seal(&mut self, plaintext: &mut [u8], aad: &[u8]) -> Res<Vec<u8>> {
        let buf = InOutBuf::from(plaintext);
        Ok(match self {
            Self::X25519HkdfSha256(SenderContextX25519HkdfSha256::HkdfSha256(
                SenderContextX25519HkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                let tag = context.seal_inout_detached(buf, aad)?;
                Vec::from(tag.to_bytes().as_slice())
            }
            Self::X25519HkdfSha256(SenderContextX25519HkdfSha256::HkdfSha256(
                SenderContextX25519HkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                let tag = context.seal_inout_detached(buf, aad)?;
                Vec::from(tag.to_bytes().as_slice())
            }
            Self::P256HkdfSha256(SenderContextP256HkdfSha256::HkdfSha256(
                SenderContextP256HkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                let tag = context.seal_inout_detached(buf, aad)?;
                Vec::from(tag.to_bytes().as_slice())
            }
            Self::P256HkdfSha256(SenderContextP256HkdfSha256::HkdfSha256(
                SenderContextP256HkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                let tag = context.seal_inout_detached(buf, aad)?;
                Vec::from(tag.to_bytes().as_slice())
            }
            Self::XWingHkdfSha256(SenderContextXWingHkdfSha256::HkdfSha256(
                SenderContextXWingHkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                let tag = context.seal_inout_detached(buf, aad)?;
                Vec::from(tag.to_bytes().as_slice())
            }
            Self::XWingHkdfSha256(SenderContextXWingHkdfSha256::HkdfSha256(
                SenderContextXWingHkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                let tag = context.seal_inout_detached(buf, aad)?;
                Vec::from(tag.to_bytes().as_slice())
            }
        })
    }

    fn export(&self, info: &[u8], out_buf: &mut [u8]) -> Res<()> {
        match self {
            Self::X25519HkdfSha256(SenderContextX25519HkdfSha256::HkdfSha256(
                SenderContextX25519HkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::X25519HkdfSha256(SenderContextX25519HkdfSha256::HkdfSha256(
                SenderContextX25519HkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::P256HkdfSha256(SenderContextP256HkdfSha256::HkdfSha256(
                SenderContextP256HkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::P256HkdfSha256(SenderContextP256HkdfSha256::HkdfSha256(
                SenderContextP256HkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::XWingHkdfSha256(SenderContextXWingHkdfSha256::HkdfSha256(
                SenderContextXWingHkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::XWingHkdfSha256(SenderContextXWingHkdfSha256::HkdfSha256(
                SenderContextXWingHkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                context.export(info, out_buf)?;
            }
        }
        Ok(())
    }
}

pub trait Exporter {
    fn export(&self, info: &[u8], len: usize) -> Res<SymKey>;
}

#[allow(clippy::module_name_repetitions)]
pub struct HpkeS {
    context: SenderContext,
    enc: Vec<u8>,
    config: Config,
}

impl HpkeS {
    /// Create a new context that uses the KEM mode for sending.
    // Pingers patch: the caller supplies the randomness for the ephemeral key.
    pub fn new(
        config: Config,
        pk_r: &PublicKey,
        info: &[u8],
        rng: &mut impl rand_core::CryptoRng,
    ) -> Res<Self> {
        macro_rules! dispatch_hpkes_new {
            {
                ($c:expr, $pk:expr): [$( $(#[$meta:meta])* {
                    $kemid:path => $kem:path,
                    $kdfid:path => $kdf:path,
                    $aeadid:path => $aead:path,
                    $pke:path, $ctxt1:path, $ctxt2:path, $ctxt3:path $(,)?
                }),* $(,)?]
            } => {
                match ($c, $pk) {
                    $(
                        $(#[$meta])*
                        (
                            Config {
                                kem: $kemid,
                                kdf: $kdfid,
                                aead: $aeadid,
                            },
                            $pke(pk_r),
                        ) => {
                            let (enc, context) = setup_sender_with_rng::<$aead, $kdf, $kem>(
                                &OpModeS::Base,
                                pk_r,
                                info,
                                rng,
                            )?;
                            (
                                $ctxt1($ctxt2($ctxt3(Box::new(context)))),
                                Vec::from(enc.to_bytes().as_slice()),
                            )
                        }
                    )*
                    _ => return Err(Error::InvalidKeyType),
                }
            };
        }

        let (context, enc) = dispatch_hpkes_new! { (config, pk_r): [
            {
                Kem::X25519Sha256 => X25519HkdfSha256,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::Aes128Gcm => AesGcm128,
                PublicKey::X25519,
                SenderContext::X25519HkdfSha256,
                SenderContextX25519HkdfSha256::HkdfSha256,
                SenderContextX25519HkdfSha256HkdfSha256::AesGcm128,
            },
            {
                Kem::X25519Sha256 => X25519HkdfSha256,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::ChaCha20Poly1305 => ChaCha20Poly1305,
                PublicKey::X25519,
                SenderContext::X25519HkdfSha256,
                SenderContextX25519HkdfSha256::HkdfSha256,
                SenderContextX25519HkdfSha256HkdfSha256::ChaCha20Poly1305,
            },
            {
                Kem::P256Sha256 => DhP256HkdfSha256,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::Aes128Gcm => AesGcm128,
                PublicKey::P256,
                SenderContext::P256HkdfSha256,
                SenderContextP256HkdfSha256::HkdfSha256,
                SenderContextP256HkdfSha256HkdfSha256::AesGcm128,
            },
            {
                Kem::P256Sha256 => DhP256HkdfSha256,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::ChaCha20Poly1305 => ChaCha20Poly1305,
                PublicKey::P256,
                SenderContext::P256HkdfSha256,
                SenderContextP256HkdfSha256::HkdfSha256,
                SenderContextP256HkdfSha256HkdfSha256::ChaCha20Poly1305,
            },
            {
                Kem::XWing => XWing,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::Aes128Gcm => AesGcm128,
                PublicKey::XWing,
                SenderContext::XWingHkdfSha256,
                SenderContextXWingHkdfSha256::HkdfSha256,
                SenderContextXWingHkdfSha256HkdfSha256::AesGcm128,
            },
            {
                Kem::XWing => XWing,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::ChaCha20Poly1305 => ChaCha20Poly1305,
                PublicKey::XWing,
                SenderContext::XWingHkdfSha256,
                SenderContextXWingHkdfSha256::HkdfSha256,
                SenderContextXWingHkdfSha256HkdfSha256::ChaCha20Poly1305,
            },
        ]};

        Ok(Self {
            context,
            enc,
            config,
        })
    }

    pub fn config(&self) -> Config {
        self.config
    }

    /// Get the encapsulated KEM secret.
    #[allow(clippy::unnecessary_wraps)]
    pub fn enc(&self) -> Res<Vec<u8>> {
        Ok(self.enc.clone())
    }
}

impl Encrypt for HpkeS {
    fn seal(&mut self, aad: &[u8], pt: &[u8]) -> Res<Vec<u8>> {
        let mut buf = pt.to_owned();
        let mut tag = self.context.seal(&mut buf, aad)?;
        buf.append(&mut tag);
        Ok(buf)
    }

    fn alg(&self) -> Aead {
        self.config.aead()
    }
}

impl Exporter for HpkeS {
    fn export(&self, info: &[u8], len: usize) -> Res<SymKey> {
        let mut buf = vec![0; len];
        self.context.export(info, &mut buf)?;
        Ok(SymKey::from(buf))
    }
}

impl Deref for HpkeS {
    type Target = Config;
    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

enum ReceiverContextX25519HkdfSha256HkdfSha256 {
    AesGcm128(Box<AeadCtxR<AesGcm128, HkdfSha256, X25519HkdfSha256>>),
    ChaCha20Poly1305(Box<AeadCtxR<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>>),
}

enum ReceiverContextP256HkdfSha256HkdfSha256 {
    AesGcm128(Box<AeadCtxR<AesGcm128, HkdfSha256, DhP256HkdfSha256>>),
    ChaCha20Poly1305(Box<AeadCtxR<ChaCha20Poly1305, HkdfSha256, DhP256HkdfSha256>>),
}

enum ReceiverContextXWingHkdfSha256HkdfSha256 {
    AesGcm128(Box<AeadCtxR<AesGcm128, HkdfSha256, XWing>>),
    ChaCha20Poly1305(Box<AeadCtxR<ChaCha20Poly1305, HkdfSha256, XWing>>),
}

enum ReceiverContextX25519HkdfSha256 {
    HkdfSha256(ReceiverContextX25519HkdfSha256HkdfSha256),
}

enum ReceiverContextP256HkdfSha256 {
    HkdfSha256(ReceiverContextP256HkdfSha256HkdfSha256),
}

enum ReceiverContextXWingHkdfSha256 {
    HkdfSha256(ReceiverContextXWingHkdfSha256HkdfSha256),
}

enum ReceiverContext {
    X25519HkdfSha256(ReceiverContextX25519HkdfSha256),
    P256HkdfSha256(ReceiverContextP256HkdfSha256),
    XWingHkdfSha256(ReceiverContextXWingHkdfSha256),
}

impl ReceiverContext {
    fn open<'a>(&mut self, ciphertext: &'a mut [u8], aad: &[u8]) -> Res<&'a [u8]> {
        Ok(match self {
            Self::X25519HkdfSha256(ReceiverContextX25519HkdfSha256::HkdfSha256(
                ReceiverContextX25519HkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                if ciphertext.len() < AeadTag::<AesGcm128>::size() {
                    return Err(Error::Truncated);
                }
                let (ct, tag_slice) =
                    ciphertext.split_at_mut(ciphertext.len() - AeadTag::<AesGcm128>::size());
                let mut ct = InOutBuf::from(ct);
                let tag = AeadTag::<AesGcm128>::from_bytes(tag_slice)?;
                context.open_inout_detached(ct.reborrow(), aad, &tag)?;
                ct.into_out()
            }
            Self::X25519HkdfSha256(ReceiverContextX25519HkdfSha256::HkdfSha256(
                ReceiverContextX25519HkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                if ciphertext.len() < AeadTag::<ChaCha20Poly1305>::size() {
                    return Err(Error::Truncated);
                }
                let (ct, tag_slice) =
                    ciphertext.split_at_mut(ciphertext.len() - AeadTag::<ChaCha20Poly1305>::size());
                let mut ct = InOutBuf::from(ct);
                let tag = AeadTag::<ChaCha20Poly1305>::from_bytes(tag_slice)?;
                context.open_inout_detached(ct.reborrow(), aad, &tag)?;
                ct.into_out()
            }
            Self::P256HkdfSha256(ReceiverContextP256HkdfSha256::HkdfSha256(
                ReceiverContextP256HkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                if ciphertext.len() < AeadTag::<AesGcm128>::size() {
                    return Err(Error::Truncated);
                }
                let (ct, tag_slice) =
                    ciphertext.split_at_mut(ciphertext.len() - AeadTag::<AesGcm128>::size());
                let mut ct = InOutBuf::from(ct);
                let tag = AeadTag::<AesGcm128>::from_bytes(tag_slice)?;
                context.open_inout_detached(ct.reborrow(), aad, &tag)?;
                ct.into_out()
            }
            Self::P256HkdfSha256(ReceiverContextP256HkdfSha256::HkdfSha256(
                ReceiverContextP256HkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                if ciphertext.len() < AeadTag::<ChaCha20Poly1305>::size() {
                    return Err(Error::Truncated);
                }
                let (ct, tag_slice) =
                    ciphertext.split_at_mut(ciphertext.len() - AeadTag::<ChaCha20Poly1305>::size());
                let mut ct = InOutBuf::from(ct);
                let tag = AeadTag::<ChaCha20Poly1305>::from_bytes(tag_slice)?;
                context.open_inout_detached(ct.reborrow(), aad, &tag)?;
                ct.into_out()
            }
            Self::XWingHkdfSha256(ReceiverContextXWingHkdfSha256::HkdfSha256(
                ReceiverContextXWingHkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                if ciphertext.len() < AeadTag::<AesGcm128>::size() {
                    return Err(Error::Truncated);
                }
                let (ct, tag_slice) =
                    ciphertext.split_at_mut(ciphertext.len() - AeadTag::<AesGcm128>::size());
                let mut ct = InOutBuf::from(ct);
                let tag = AeadTag::<AesGcm128>::from_bytes(tag_slice)?;
                context.open_inout_detached(ct.reborrow(), aad, &tag)?;
                ct.into_out()
            }
            Self::XWingHkdfSha256(ReceiverContextXWingHkdfSha256::HkdfSha256(
                ReceiverContextXWingHkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                if ciphertext.len() < AeadTag::<ChaCha20Poly1305>::size() {
                    return Err(Error::Truncated);
                }
                let (ct, tag_slice) =
                    ciphertext.split_at_mut(ciphertext.len() - AeadTag::<ChaCha20Poly1305>::size());
                let mut ct = InOutBuf::from(ct);
                let tag = AeadTag::<ChaCha20Poly1305>::from_bytes(tag_slice)?;
                context.open_inout_detached(ct.reborrow(), aad, &tag)?;
                ct.into_out()
            }
        })
    }

    fn export(&self, info: &[u8], out_buf: &mut [u8]) -> Res<()> {
        match self {
            Self::X25519HkdfSha256(ReceiverContextX25519HkdfSha256::HkdfSha256(
                ReceiverContextX25519HkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::X25519HkdfSha256(ReceiverContextX25519HkdfSha256::HkdfSha256(
                ReceiverContextX25519HkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::P256HkdfSha256(ReceiverContextP256HkdfSha256::HkdfSha256(
                ReceiverContextP256HkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::P256HkdfSha256(ReceiverContextP256HkdfSha256::HkdfSha256(
                ReceiverContextP256HkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::XWingHkdfSha256(ReceiverContextXWingHkdfSha256::HkdfSha256(
                ReceiverContextXWingHkdfSha256HkdfSha256::AesGcm128(context),
            )) => {
                context.export(info, out_buf)?;
            }
            Self::XWingHkdfSha256(ReceiverContextXWingHkdfSha256::HkdfSha256(
                ReceiverContextXWingHkdfSha256HkdfSha256::ChaCha20Poly1305(context),
            )) => {
                context.export(info, out_buf)?;
            }
        }
        Ok(())
    }
}

#[allow(clippy::module_name_repetitions)]
pub struct HpkeR {
    context: ReceiverContext,
    config: Config,
}

impl HpkeR {
    /// Create a new context that uses the KEM mode for sending.
    #[allow(clippy::similar_names)]
    pub fn new(
        config: Config,
        _pk_r: &PublicKey,
        sk_r: &PrivateKey,
        enc: &[u8],
        info: &[u8],
    ) -> Res<Self> {
        macro_rules! dispatch_hpker_new {
            {
                ($c:ident, $sk:ident): [$( $(#[$meta:meta])* {
                    $kemid:path => $kem:path,
                    $kdfid:path => $kdf:path,
                    $aeadid:path => $aead:path,
                    $ske:path, $ctxt1:path, $ctxt2:path, $ctxt3:path $(,)?
            }),* $(,)?]
            } => {
                match ($c, $sk) {
                    $(
                        $(#[$meta])*
                        (
                            Config {
                                kem: $kemid,
                                kdf: $kdfid,
                                aead: $aeadid,
                            },
                            $ske(sk_r),
                        ) => {
                            let enc = <$kem as KemTrait>::EncappedKey::from_bytes(enc)?;
                            let context = setup_receiver::<$aead, $kdf, $kem>(
                                &OpModeR::Base,
                                sk_r,
                                &enc,
                                info,
                            )?;
                            $ctxt1($ctxt2($ctxt3(Box::new(context))))
                        }
                    )*
                    _ => return Err(Error::InvalidKeyType),
                }
            };
        }
        let context = dispatch_hpker_new! {(config, sk_r): [
            {
                Kem::X25519Sha256 => X25519HkdfSha256,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::Aes128Gcm => AesGcm128,
                PrivateKey::X25519,
                ReceiverContext::X25519HkdfSha256,
                ReceiverContextX25519HkdfSha256::HkdfSha256,
                ReceiverContextX25519HkdfSha256HkdfSha256::AesGcm128,
            },
            {
                Kem::X25519Sha256 => X25519HkdfSha256,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::ChaCha20Poly1305 => ChaCha20Poly1305,
                PrivateKey::X25519,
                ReceiverContext::X25519HkdfSha256,
                ReceiverContextX25519HkdfSha256::HkdfSha256,
                ReceiverContextX25519HkdfSha256HkdfSha256::ChaCha20Poly1305,
            },
            {
                Kem::P256Sha256 => DhP256HkdfSha256,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::Aes128Gcm => AesGcm128,
                PrivateKey::P256,
                ReceiverContext::P256HkdfSha256,
                ReceiverContextP256HkdfSha256::HkdfSha256,
                ReceiverContextP256HkdfSha256HkdfSha256::AesGcm128,
            },
            {
                Kem::P256Sha256 => DhP256HkdfSha256,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::ChaCha20Poly1305 => ChaCha20Poly1305,
                PrivateKey::P256,
                ReceiverContext::P256HkdfSha256,
                ReceiverContextP256HkdfSha256::HkdfSha256,
                ReceiverContextP256HkdfSha256HkdfSha256::ChaCha20Poly1305,
            },
            {
                Kem::XWing => XWing,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::Aes128Gcm => AesGcm128,
                PrivateKey::XWing,
                ReceiverContext::XWingHkdfSha256,
                ReceiverContextXWingHkdfSha256::HkdfSha256,
                ReceiverContextXWingHkdfSha256HkdfSha256::AesGcm128,
            },
            {
                Kem::XWing => XWing,
                Kdf::HkdfSha256 => HkdfSha256,
                Aead::ChaCha20Poly1305 => ChaCha20Poly1305,
                PrivateKey::XWing,
                ReceiverContext::XWingHkdfSha256,
                ReceiverContextXWingHkdfSha256::HkdfSha256,
                ReceiverContextXWingHkdfSha256HkdfSha256::ChaCha20Poly1305,
            },
        ]};

        Ok(Self { context, config })
    }

    pub fn config(&self) -> Config {
        self.config
    }

    pub fn decode_public_key(kem: Kem, k: &[u8]) -> Res<PublicKey> {
        Ok(match kem {
            Kem::X25519Sha256 => {
                PublicKey::X25519(<X25519HkdfSha256 as KemTrait>::PublicKey::from_bytes(k)?)
            }
            Kem::P256Sha256 => {
                PublicKey::P256(<DhP256HkdfSha256 as KemTrait>::PublicKey::from_bytes(k)?)
            }
            Kem::XWing => PublicKey::XWing(<XWing as KemTrait>::PublicKey::from_bytes(k)?),
        })
    }
}

impl Decrypt for HpkeR {
    fn open(&mut self, aad: &[u8], ct: &[u8]) -> Res<Vec<u8>> {
        let mut buf = ct.to_owned();
        let pt_len = self.context.open(&mut buf, aad)?.len();
        buf.truncate(pt_len);
        Ok(buf)
    }

    fn alg(&self) -> Aead {
        self.config.aead()
    }
}

impl Exporter for HpkeR {
    fn export(&self, info: &[u8], len: usize) -> Res<SymKey> {
        let mut buf = vec![0; len];
        self.context.export(info, &mut buf)?;
        Ok(SymKey::from(buf))
    }
}

impl Deref for HpkeR {
    type Target = Config;
    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

/// Generate a key pair for the identified KEM.
#[allow(clippy::unnecessary_wraps)]
// Pingers patch: the caller supplies the randomness.
pub fn generate_key_pair(
    kem: Kem,
    rng: &mut impl rand_core::CryptoRng,
) -> Res<(PrivateKey, PublicKey)> {
    let (sk, pk) = match kem {
        Kem::X25519Sha256 => {
            let (sk, pk) = X25519HkdfSha256::gen_keypair_with_rng(rng);
            (PrivateKey::X25519(sk), PublicKey::X25519(pk))
        }
        Kem::P256Sha256 => {
            let (sk, pk) = DhP256HkdfSha256::gen_keypair_with_rng(rng);
            (PrivateKey::P256(sk), PublicKey::P256(pk))
        }
        Kem::XWing => {
            let (sk, pk) = XWing::gen_keypair_with_rng(rng);
            (PrivateKey::XWing(sk), PublicKey::XWing(pk))
        }
    };
    Ok((sk, pk))
}

#[allow(clippy::unnecessary_wraps)]
pub fn derive_key_pair(kem: Kem, ikm: &[u8]) -> Res<(PrivateKey, PublicKey)> {
    let (sk, pk) = match kem {
        Kem::X25519Sha256 => {
            let (sk, pk) = X25519HkdfSha256::derive_keypair(ikm);
            (PrivateKey::X25519(sk), PublicKey::X25519(pk))
        }
        Kem::P256Sha256 => {
            let (sk, pk) = DhP256HkdfSha256::derive_keypair(ikm);
            (PrivateKey::P256(sk), PublicKey::P256(pk))
        }
        Kem::XWing => {
            let (sk, pk) = XWing::derive_keypair(ikm);
            (PrivateKey::XWing(sk), PublicKey::XWing(pk))
        }
    };
    Ok((sk, pk))
}

/// Pingers patch: loads a stored private key and computes its public key.
pub fn import_private_key(kem: Kem, sk: &[u8]) -> Res<(PrivateKey, PublicKey)> {
    Ok(match kem {
        Kem::X25519Sha256 => {
            let sk = <X25519HkdfSha256 as KemTrait>::PrivateKey::from_bytes(sk)?;
            let pk = X25519HkdfSha256::sk_to_pk(&sk);
            (PrivateKey::X25519(sk), PublicKey::X25519(pk))
        }
        Kem::P256Sha256 => {
            let sk = <DhP256HkdfSha256 as KemTrait>::PrivateKey::from_bytes(sk)?;
            let pk = DhP256HkdfSha256::sk_to_pk(&sk);
            (PrivateKey::P256(sk), PublicKey::P256(pk))
        }
        Kem::XWing => {
            let sk = <XWing as KemTrait>::PrivateKey::from_bytes(sk)?;
            let pk = XWing::sk_to_pk(&sk);
            (PrivateKey::XWing(sk), PublicKey::XWing(pk))
        }
    })
}
