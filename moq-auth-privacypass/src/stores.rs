// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use async_trait::async_trait;
use blind_rsa_signatures::{Deterministic, Sha384, PSS};
use privacypass::public_tokens::server::OriginKeyStore;
use privacypass::public_tokens::{public_key_to_truncated_token_key_id, PublicKey};
use privacypass::{Nonce, NonceStore, TruncatedTokenKeyId};
use tokio::sync::Mutex;

#[derive(Debug, Default)]
pub struct PublicKeyStore {
    keys: Mutex<HashMap<TruncatedTokenKeyId, Vec<PublicKey>>>,
}

pub fn public_key_from_spki_der(bytes: &[u8]) -> Result<PublicKey, StoreError> {
    blind_rsa_signatures::PublicKey::<Sha384, PSS, Deterministic>::from_spki(bytes)
        .map_err(|_| StoreError::InvalidPublicKey)
}

impl PublicKeyStore {
    pub async fn insert_public_key(&self, public_key: PublicKey) -> Result<(), StoreError> {
        let key_id = public_key_to_truncated_token_key_id(&public_key)
            .map_err(|_| StoreError::InvalidPublicKey)?;
        self.insert(key_id, public_key).await;
        Ok(())
    }
}

#[async_trait]
impl OriginKeyStore for PublicKeyStore {
    async fn insert(&self, truncated_token_key_id: TruncatedTokenKeyId, public_key: PublicKey) {
        let mut keys = self.keys.lock().await;
        keys.entry(truncated_token_key_id)
            .or_default()
            .push(public_key);
    }

    async fn get(&self, truncated_token_key_id: &TruncatedTokenKeyId) -> Vec<PublicKey> {
        self.keys
            .lock()
            .await
            .get(truncated_token_key_id)
            .cloned()
            .unwrap_or_default()
    }

    async fn remove(&self, truncated_token_key_id: &TruncatedTokenKeyId) -> bool {
        self.keys
            .lock()
            .await
            .remove(truncated_token_key_id)
            .is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NonceState {
    Reserved,
    Committed,
}

#[derive(Debug, Default)]
pub struct ReplayCache {
    nonces: Mutex<HashMap<Nonce, NonceState>>,
}

#[async_trait]
impl NonceStore for ReplayCache {
    async fn reserve(&self, nonce: &Nonce) -> bool {
        use std::collections::hash_map::Entry;

        let mut nonces = self.nonces.lock().await;
        match nonces.entry(*nonce) {
            Entry::Vacant(entry) => {
                entry.insert(NonceState::Reserved);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    async fn commit(&self, nonce: &Nonce) {
        let mut nonces = self.nonces.lock().await;
        if let Some(state) = nonces.get_mut(nonce) {
            if *state == NonceState::Reserved {
                *state = NonceState::Committed;
            }
        }
    }

    async fn release(&self, nonce: &Nonce) {
        let mut nonces = self.nonces.lock().await;
        if nonces.get(nonce) == Some(&NonceState::Reserved) {
            nonces.remove(nonce);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid public key")]
    InvalidPublicKey,
}
