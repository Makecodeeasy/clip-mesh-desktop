//! # 端到端加密模块 — AES-256-GCM
//!
//! 本模块封装 `aes-gcm` crate，提供与 clip-mesh-core Go 后端兼容的
//! AES-256-GCM 加密/解密函数。
//!
//! ## 密文格式（与 Go 端一致）
//!
//! ```text
//! [nonce: 12 bytes][ciphertext + tag: variable length]
//! ```
//!
//! - Nonce (IV): 12 字节随机数，每次加密唯一生成
//! - Tag: GCM 产生的 16 字节认证标签，附加在密文末尾
//!
//! ## 兼容性
//!
//! Go 端 `crypto.Cipher` 使用 `cipher.NewGCM` + `Seal/Open`，
//! Rust 端 `aes_gcm::Aes256Gcm` 使用相同的 NIST SP 800-38D 标准，
//! 两端产生的密文可互相解密。

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use rand::RngCore;

/// 密钥长度（字节）— AES-256
pub const KEY_SIZE: usize = 32;

/// Nonce 长度（字节）— GCM 标准推荐值
pub const NONCE_SIZE: usize = 12;

/// 认证标签长度（字节）
pub const TAG_SIZE: usize = 16;

/// AES-256-GCM 加密器，持有预编译的密钥扩展结果。
pub struct MeshCipher {
    cipher: Aes256Gcm,
}

impl MeshCipher {
    /// 使用 32 字节密钥创建加密器实例。
    ///
    /// # 错误
    /// 若密钥长度不为 32 字节，返回错误信息。
    pub fn new(key: &[u8]) -> Result<Self, String> {
        if key.len() != KEY_SIZE {
            return Err(format!(
                "Invalid key size: expected {} bytes, got {}",
                KEY_SIZE,
                key.len()
            ));
        }

        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| format!("Failed to initialize AES-256-GCM: {}", e))?;

        Ok(Self { cipher })
    }

    /// 使用十六进制编码的密钥字符串创建加密器。
    ///
    /// # 示例
    /// ```ignore
    /// let cipher = MeshCipher::from_hex_key("a1b2c3d4...")?;
    /// ```
    pub fn from_hex_key(hex_key: &str) -> Result<Self, String> {
        let key_bytes = hex::decode(hex_key)
            .map_err(|e| format!("Invalid hex key: {}", e))?;
        Self::new(&key_bytes)
    }

    /// 加密明文数据。
    ///
    /// # 参数
    /// - `plaintext`: 待加密的原始数据
    /// - `aad`: 附加认证数据（Additional Authenticated Data），可为空切片
    ///
    /// # 返回
    /// 密文字节数组，格式为 `[nonce(12B)][ciphertext+tag]`
    pub fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        // 生成 12 字节随机 Nonce
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // 构造带 AAD 的 Payload
        let payload = aes_gcm::aead::Payload {
            msg: plaintext,
            aad,
        };

        // 执行 GCM 加密
        let ciphertext = self.cipher
            .encrypt(nonce, payload)
            .map_err(|e| format!("Encryption failed: {}", e))?;

        // 拼接输出：nonce + ciphertext（含 tag）
        let mut output = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ciphertext);

        Ok(output)
    }

    /// 加密文本字符串（便捷封装）。
    ///
    /// 返回 Base64 编码的密文字符串，便于通过 JSON/WebSocket 传输。
    pub fn encrypt_text(&self, text: &str, aad: &[u8]) -> Result<String, String> {
        let encrypted = self.encrypt(text.as_bytes(), aad)?;
        use base64::Engine;
        Ok(base64::engine::general_purpose::STANDARD.encode(&encrypted))
    }

    /// 解密密文数据。
    ///
    /// # 参数
    /// - `ciphertext`: 由 `encrypt` 产生的密文，格式为 `[nonce(12B)][ciphertext+tag]`
    /// - `aad`: 附加认证数据，必须与加密时使用的一致
    ///
    /// # 返回
    /// 解密后的明文字节数组
    pub fn decrypt(&self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        // 校验最小长度：Nonce (12B) + Tag (16B) = 28B
        if ciphertext.len() < NONCE_SIZE + TAG_SIZE {
            return Err(format!(
                "Ciphertext too short: {} bytes (minimum {})",
                ciphertext.len(),
                NONCE_SIZE + TAG_SIZE
            ));
        }

        // 拆分 Nonce 与实际密文
        let nonce = Nonce::from_slice(&ciphertext[..NONCE_SIZE]);
        let encrypted_data = &ciphertext[NONCE_SIZE..];

        // 构造带 AAD 的 Payload
        let payload = aes_gcm::aead::Payload {
            msg: encrypted_data,
            aad,
        };

        // 执行 GCM 解密（同时验证认证标签）
        let plaintext = self.cipher
            .decrypt(nonce, payload)
            .map_err(|_| "Decryption failed: ciphertext is invalid or tampered".to_string())?;

        Ok(plaintext)
    }

    /// 解密 Base64 编码的密文字符串，返回明文 String。
    pub fn decrypt_text(&self, b64_ciphertext: &str, aad: &[u8]) -> Result<String, String> {
        use base64::Engine;
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(b64_ciphertext)
            .map_err(|e| format!("Invalid base64: {}", e))?;

        let plaintext = self.decrypt(&ciphertext, aad)?;

        String::from_utf8(plaintext)
            .map_err(|e| format!("Decrypted data is not valid UTF-8: {}", e))
    }
}

/// 生成一个密码学安全的随机 256 位密钥。
pub fn generate_key() -> [u8; KEY_SIZE] {
    let mut key = [0u8; KEY_SIZE];
    OsRng.fill_bytes(&mut key);
    key
}
