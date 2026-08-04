// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;

use crate::{AuthBlob, AuthDecision, AuthHook, RequestContext, SessionContext};

/// Composable observability wrapper that logs every hook invocation
/// while delegating the actual decision to the inner hook.
pub struct LoggingAuthHook<H> {
    inner: H,
}

impl<H> LoggingAuthHook<H> {
    pub fn new(inner: H) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<H: AuthHook> AuthHook for LoggingAuthHook<H> {
    async fn on_setup(
        &self,
        ctx: &SessionContext,
        tokens: &[AuthBlob],
    ) -> anyhow::Result<AuthDecision> {
        let result: anyhow::Result<AuthDecision> = self.inner.on_setup(ctx, tokens).await;
        match &result {
            Ok(decision) => {
                log::debug!(
                    "auth on_setup: session={}, tokens={}, allowed={}, principal={:?}",
                    ctx.session_id,
                    tokens.len(),
                    decision.is_allowed(),
                    decision.principal.as_deref()
                );
            }
            Err(e) => {
                log::error!(
                    "auth on_setup error: session={}, error={}",
                    ctx.session_id,
                    e
                );
            }
        }
        result
    }

    async fn on_request(
        &self,
        ctx: &RequestContext<'_>,
        tokens: &[AuthBlob],
    ) -> anyhow::Result<AuthDecision> {
        let result: anyhow::Result<AuthDecision> = self.inner.on_request(ctx, tokens).await;
        match &result {
            Ok(decision) => {
                log::debug!(
                    "auth on_request: session={}, operation={:?}, allowed={}",
                    ctx.session.session_id,
                    ctx.operation,
                    decision.is_allowed()
                );
            }
            Err(e) => {
                log::error!(
                    "auth on_request error: session={}, operation={:?}, error={}",
                    ctx.session.session_id,
                    ctx.operation,
                    e
                );
            }
        }
        result
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.inner.shutdown().await
    }
}
