// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! A network client that refuses every request.
//!
//! GPUI depends on an HTTP client crate unconditionally and hands one to the
//! application context. Nothing in this product calls it, but "nothing calls
//! it" is a claim about today's code, not a guarantee. Installing a client that
//! cannot succeed turns "no network" into a property of the binary: a request
//! that is ever attempted fails loudly instead of reaching the network.

use futures::future::BoxFuture;
use gpui::http_client::{AsyncBody, HttpClient, Response, Url, http};

/// The refusal message, shared with the test that pins this behavior.
pub const REFUSAL: &str = "this application performs no network requests";

/// An [`HttpClient`] that fails every request without opening a socket.
#[derive(Debug, Default)]
pub struct NoNetwork;

impl HttpClient for NoNetwork {
    fn type_name(&self) -> &'static str {
        "NoNetwork"
    }

    fn user_agent(&self) -> Option<&http::HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: http::Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let uri = request.uri().to_string();
        Box::pin(async move { Err(anyhow::anyhow!("{REFUSAL} (refused {uri})")) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::http_client::AsyncBody;

    #[test]
    fn every_request_is_refused_with_the_target_named() {
        let client = NoNetwork;
        let request = http::Request::builder()
            .uri("https://example.invalid/telemetry")
            .body(AsyncBody::empty())
            .unwrap();

        let message = match futures::executor::block_on(client.send(request)) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("the offline client must never succeed"),
        };
        assert!(message.contains(REFUSAL), "{message}");
        assert!(message.contains("example.invalid"), "{message}");
    }

    #[test]
    fn the_client_advertises_neither_a_user_agent_nor_a_proxy() {
        assert!(NoNetwork.user_agent().is_none());
        assert!(NoNetwork.proxy().is_none());
        assert_eq!(NoNetwork.type_name(), "NoNetwork");
    }
}
