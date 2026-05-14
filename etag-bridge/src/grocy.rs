//! Grocy REST API client.
//!
//! Wraps [`reqwest`] to provide typed methods for the Grocy stock-management
//! endpoints used by the bridge.
//!
//! # Grocy REST API reference
//! <https://demo.grocy.info/api/>
//!
//! # Authentication
//!
//! All requests include the `GROCY-API-KEY` header.  Generate an API key at
//! *Grocy → Settings → API Keys*.
//!
//! # Error handling
//!
//! All methods return [`anyhow::Result`].  HTTP errors (4xx / 5xx) are
//! converted into [`anyhow::Error`] with a descriptive message including the
//! URL and response body.  404 responses from stock endpoints are treated as
//! non-fatal warnings (the product may not yet exist in Grocy) and return
//! `Ok(())`.
//!
//! # Example
//!
//! ```rust,no_run
//! # use etag_bridge::grocy::GrocyClient;
//! # #[tokio::main] async fn main() -> anyhow::Result<()> {
//! let client = GrocyClient::new("http://grocy.local", "my_api_key");
//!
//! // Add 2 units of product 42 to Grocy stock.
//! client.add_product(42, 2.0).await?;
//!
//! // Consume 1 unit.
//! client.consume_product(42, 1.0).await?;
//!
//! // Synchronise: computes the delta and calls add or consume automatically.
//! client.sync_stock(42, /*previous=*/ 3, /*new=*/ 2).await?;
//! # Ok(())
//! # }
//! ```

use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tracing::{debug, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// GrocyClient
// ─────────────────────────────────────────────────────────────────────────────

/// HTTP client wrapper for the Grocy REST API.
///
/// Cheap to clone — the inner [`reqwest::Client`] uses a shared connection pool.
#[derive(Debug, Clone)]
pub struct GrocyClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl GrocyClient {
    /// Create a new client.
    ///
    /// `base_url` is the Grocy root URL, e.g. `"http://grocy.local"` or
    /// `"https://grocy.example.com"`.  A trailing slash is stripped automatically.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
        }
    }

    // ── Stock operations ──────────────────────────────────────────────────────

    /// Synchronise the physical tag count to Grocy.
    ///
    /// Computes `delta = new_count - previous` and:
    ///
    /// - `delta > 0` → calls [`add_product`](Self::add_product)
    /// - `delta < 0` → calls [`consume_product`](Self::consume_product)
    /// - `delta == 0` → no-op
    ///
    /// This is the primary method called from the BLE notification handler.
    pub async fn sync_stock(&self, product_id: u32, previous: i32, new_count: i32) -> Result<()> {
        let delta = new_count - previous;
        match delta.cmp(&0) {
            std::cmp::Ordering::Greater => {
                info!(product_id, delta, "Adding stock to Grocy");
                self.add_product(product_id, delta as f64).await
            }
            std::cmp::Ordering::Less => {
                info!(product_id, delta = (-delta), "Consuming stock from Grocy");
                self.consume_product(product_id, (-delta) as f64).await
            }
            std::cmp::Ordering::Equal => {
                debug!(product_id, "Stock unchanged — no Grocy update needed");
                Ok(())
            }
        }
    }

    /// Add `amount` units of `product_id` to Grocy stock.
    ///
    /// Calls `POST /api/stock/products/{id}/add`.
    pub async fn add_product(&self, product_id: u32, amount: f64) -> Result<()> {
        let url = format!("{}/api/stock/products/{product_id}/add", self.base_url);
        let body = serde_json::json!({ "amount": amount });
        let resp = self
            .client
            .post(&url)
            .header("GROCY-API-KEY", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        self.check_response(resp, "add_product", &url).await
    }

    /// Consume `amount` units of `product_id` from Grocy stock.
    ///
    /// Calls `POST /api/stock/products/{id}/consume`.
    pub async fn consume_product(&self, product_id: u32, amount: f64) -> Result<()> {
        let url = format!("{}/api/stock/products/{product_id}/consume", self.base_url);
        let body = serde_json::json!({ "amount": amount, "spoiled": false });
        let resp = self
            .client
            .post(&url)
            .header("GROCY-API-KEY", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        self.check_response(resp, "consume_product", &url).await
    }

    /// Fetch the current stock amount for a product from Grocy.
    ///
    /// Calls `GET /api/stock/products/{id}/overview`.  Returns the `amount`
    /// field as an `f64`.
    pub async fn get_product_stock(&self, product_id: u32) -> Result<f64> {
        let url = format!(
            "{}/api/stock/products/{product_id}/overview",
            self.base_url
        );
        let resp = self
            .client
            .get(&url)
            .header("GROCY-API-KEY", &self.api_key)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;

        if resp.status() == StatusCode::NOT_FOUND {
            warn!(product_id, "Product not found in Grocy");
            return Ok(0.0);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Grocy GET {url} failed ({status}): {body}");
        }

        let summary: ProductStockSummary = resp
            .json()
            .await
            .context("Failed to deserialise Grocy stock summary")?;
        Ok(summary.amount)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Check a Grocy API response, returning `Ok(())` on success and an
    /// informative error on failure.  404 is treated as a non-fatal warning.
    async fn check_response(
        &self,
        resp: reqwest::Response,
        op: &str,
        url: &str,
    ) -> Result<()> {
        let status = resp.status();
        if status.is_success() {
            debug!(op, %status, "Grocy API call succeeded");
            return Ok(());
        }
        if status == StatusCode::NOT_FOUND {
            warn!(op, url, "Product not found in Grocy (skipping sync)");
            return Ok(()); // non-fatal: product may not yet exist
        }
        let body = resp.text().await.unwrap_or_else(|_| "(unreadable)".to_owned());
        bail!("Grocy {op} failed ({status}) at {url}: {body}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Grocy API response types
// ─────────────────────────────────────────────────────────────────────────────

/// Subset of the Grocy stock-overview response used by [`GrocyClient::get_product_stock`].
#[derive(Debug, Deserialize)]
pub struct ProductStockSummary {
    /// Current in-stock amount.
    pub amount: f64,
}
