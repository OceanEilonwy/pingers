// SPDX-License-Identifier: AGPL-3.0-only
//! Reading an upstream answer's body with a cap, chunk by chunk: an answer longer than the cap
//! is abandoned as soon as it passes it, never buffered whole first.

/// The body of `response`, or `None` once it passes `limit` bytes or fails to arrive.
pub(crate) async fn body(mut response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > limit))
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}
