use super::provider::{PriceProvider, SignedPriceData};
use serde::Deserialize;
use std::error::Error;

#[derive(Deserialize, Debug, Clone)]
pub struct PythResponse {
    /// Binary data containing VAA (Verified Action Approval) signature
    pub binary: BinaryData,
    /// Parsed price feeds
    pub parsed: Vec<ParsedPriceFeed>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BinaryData {
    /// Encoding format (usually "base64")
    pub encoding: String,
    /// Array of VAA signatures (base64 encoded)
    pub data: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ParsedPriceFeed {
    /// Price feed ID (hex string)
    pub id: String,
    /// Current price data
    pub price: PythPrice,
    /// Exponential moving average price
    pub ema_price: PythPrice,
    /// Metadata (optional)
    #[serde(default)]
    pub metadata: Option<PythMetadata>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct PythPrice {
    /// Price value (need to apply expo)
    #[serde(deserialize_with = "deserialize_string_to_i64")]
    pub price: i64,
    /// Confidence interval
    #[serde(deserialize_with = "deserialize_string_to_u64")]
    pub conf: u64,
    /// Exponent (price = price * 10^expo)
    pub expo: i32,
    /// Publish timestamp (Unix time)
    pub publish_time: u64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct PythMetadata {
    pub slot: Option<u64>,
    pub proof_available_time: Option<u64>,
    pub prev_publish_time: Option<u64>,
}

// Helper deserializers for string numbers
fn deserialize_string_to_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = serde::Deserialize::deserialize(deserializer)?;
    s.parse().map_err(serde::de::Error::custom)
}

fn deserialize_string_to_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = serde::Deserialize::deserialize(deserializer)?;
    s.parse().map_err(serde::de::Error::custom)
}

/// Pyth Network oracle provider
pub struct PythProvider {
    base_url: String,
    client: reqwest::blocking::Client,
}

impl PythProvider {
    /// Create new Pyth Network provider
    /// 
    /// Uses Hermes API endpoint: https://hermes.pyth.network
    /// Documentation: https://docs.pyth.network/price-feeds/api-instances-and-providers/hermes
    pub fn new() -> Self {
        Self {
            base_url: "https://hermes.pyth.network".to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }
    
    /// Map symbol to Pyth price feed ID
    /// 
    /// Each symbol maps to a unique feed ID (hex string).
    /// Feed IDs are provided by Pyth Network and identify specific price feeds.
    /// 
    /// Supported symbols:
    /// - "BTC-USD", "BTC", "BITCOIN"
    /// - "ETH-USD", "ETH", "ETHEREUM"
    /// - "SOL-USD", "SOL", "SOLANA"
    /// - "AVAX-USD", "AVAX", "AVALANCHE"
    /// - "MATIC-USD", "MATIC", "POLYGON"
    /// - "USDT-USD", "USDT", "TETHER"
    /// 
    /// Full list of feed IDs: https://pyth.network/developers/price-feed-ids
    pub fn get_feed_id(symbol: &str) -> Option<&'static str> {
        match symbol.to_uppercase().as_str() {
            "BTC-USD" | "BTC" | "BITCOIN" => {
                Some("0xe62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43")
            }
            "ETH-USD" | "ETH" | "ETHEREUM" => {
                Some("0xff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace")
            }
            "SOL-USD" | "SOL" | "SOLANA" => {
                Some("0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d")
            }
            "AVAX-USD" | "AVAX" | "AVALANCHE" => {
                Some("0x93da3352f9f1d105fdfe4971cfa80e9dd777bfc5d0f683ebb6e1294b92137bb7")
            }
            "MATIC-USD" | "MATIC" | "POLYGON" => {
                Some("0x5de33a9112c2b700b8d30b8a3402c103578ccfa2765696471cc672bd5cf6ac52")
            }
            "USDT-USD" | "USDT" | "TETHER" => {
                Some("0x2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b")
            }
            _ => None,
        }
    }
    
    /// Fetch price with signature (VAA) from Pyth Network
    pub fn fetch_price_with_signature(&self, symbol: &str) -> Result<PythResponse, Box<dyn Error>> {
        let feed_id = Self::get_feed_id(symbol)
            .ok_or_else(|| format!("Unknown symbol: {}", symbol))?;
        
        // Use v2/updates/price/latest endpoint to get signed data
        let url = format!(
            "{}/v2/updates/price/latest?ids[]={}",
            self.base_url, feed_id
        );
        
        // Silent fetch - logging done by OracleAgent
        
        let response = self
            .client
            .get(&url)
            .header("User-Agent", "perp-lab-simulator/1.0")
            .send()?;
        
        if !response.status().is_success() {
            return Err(format!("Pyth API error: {} - {}", response.status(), response.text()?).into());
        }
        
        let data: PythResponse = response.json()?;
        
        // Verify we received data
        if data.parsed.is_empty() {
            return Err("No price feed data received".into());
        }
        
        if data.binary.data.is_empty() {
            return Err("No VAA signature received".into());
        }
        
        // VAA signature received: {} bytes - silent
        
        Ok(data)
    }
    
    /// Convert Pyth price to micro-USD
    pub fn price_to_usd_micro(pyth_price: &PythPrice) -> u64 {
        // price = price_value * 10^expo
        let price_f64 = pyth_price.price as f64 * 10_f64.powi(pyth_price.expo);
        // Convert to micro-USD (multiply by 1e6)
        (price_f64 * 1_000_000.0) as u64
    }
    
    /// Fetch multiple prices in a batch
    pub fn fetch_batch_prices(&self, symbols: &[&str]) -> Result<Vec<PythResponse>, Box<dyn Error>> {
        // Build feed IDs
        let feed_ids: Result<Vec<_>, _> = symbols
            .iter()
            .map(|s| Self::get_feed_id(s).ok_or_else(|| format!("Unknown symbol: {}", s)))
            .collect();
        
        let feed_ids = feed_ids?;
        
        // Build URL with multiple IDs
        let mut url = format!("{}/v2/updates/price/latest?", self.base_url);
        for id in &feed_ids {
            url.push_str(&format!("ids[]={}&", id));
        }
        
        // Batch fetch for {} symbols - silent
        
        let response = self
            .client
            .get(&url)
            .header("User-Agent", "perp-lab-simulator/1.0")
            .send()?;
        
        if !response.status().is_success() {
            return Err(format!("Pyth API error: {}", response.status()).into());
        }
        
        let data: PythResponse = response.json()?;
        
        // Split response into individual responses per symbol
        let mut results = Vec::new();
        for (i, feed) in data.parsed.iter().enumerate() {
            results.push(PythResponse {
                binary: BinaryData {
                    encoding: data.binary.encoding.clone(),
                    data: if i < data.binary.data.len() {
                        vec![data.binary.data[i].clone()]
                    } else {
                        vec![]
                    },
                },
                parsed: vec![feed.clone()],
            });
        }
        
        Ok(results)
    }
}

impl Default for PythProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PriceProvider for PythProvider {
    fn fetch_signed_price(&self, symbol: &str) -> Result<SignedPriceData, Box<dyn Error>> {
        let response = self.fetch_price_with_signature(symbol)?;
        let feed = response.parsed.first().ok_or("No price data")?;
        let vaa = response.binary.data.first().ok_or("No VAA signature")?;
        
        Ok(SignedPriceData {
            symbol: symbol.to_string(),
            price_usd_micro: Self::price_to_usd_micro(&feed.price),
            confidence: Some(Self::price_to_usd_micro(&PythPrice {
                price: feed.price.conf as i64,
                conf: 0,
                expo: feed.price.expo,
                publish_time: feed.price.publish_time,
            })),
            ema_price: Some(Self::price_to_usd_micro(&feed.ema_price)),
            publish_time: feed.price.publish_time,
            signature: vaa.as_bytes().to_vec(),
            provider_name: "Pyth Network".to_string(),
        })
    }
    
    fn provider_name(&self) -> &str {
        "Pyth Network"
    }
    
    fn supported_symbols(&self) -> Vec<String> {
        vec![
            "BTC-USD",
            "ETH-USD",
            "SOL-USD",
            "AVAX-USD",
            "MATIC-USD",
            "USDT-USD",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }
    
    fn fetch_batch(&self, symbols: &[&str]) -> Vec<Result<SignedPriceData, Box<dyn Error>>> {
        // Try batch fetch first
        match self.fetch_batch_prices(symbols) {
            Ok(responses) => {
                responses
                    .into_iter()
                    .zip(symbols.iter())
                    .map(|(response, symbol)| {
                        let feed = response.parsed.first().ok_or("No price data")?;
                        let vaa = response.binary.data.first().ok_or("No VAA")?;
                        
                        Ok(SignedPriceData {
                            symbol: symbol.to_string(),
                            price_usd_micro: Self::price_to_usd_micro(&feed.price),
                            confidence: Some(Self::price_to_usd_micro(&PythPrice {
                                price: feed.price.conf as i64,
                                conf: 0,
                                expo: feed.price.expo,
                                publish_time: feed.price.publish_time,
                            })),
                            ema_price: Some(Self::price_to_usd_micro(&feed.ema_price)),
                            publish_time: feed.price.publish_time,
                            signature: vaa.as_bytes().to_vec(),
                            provider_name: "Pyth Network".to_string(),
                        })
                    })
                    .collect()
            }
            Err(e) => {
                // Fallback to individual fetches
                eprintln!("[Pyth] batch fetch failed: {}, falling back to individual", e);
                symbols
                    .iter()
                    .map(|symbol| self.fetch_signed_price(symbol))
                    .collect()
            }
        }
    }
}

