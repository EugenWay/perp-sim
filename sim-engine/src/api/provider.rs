use std::error::Error;

#[derive(Debug, Clone)]
pub struct SignedPriceData {
    pub symbol: String,
    pub price_usd_micro: u64, // micro-USD: 1 USD = 1_000_000
    pub confidence: Option<u64>,
    pub ema_price: Option<u64>,
    pub publish_time: u64,
    pub signature: Vec<u8>, // VAA signature for Pyth
    pub provider_name: String,
}

pub trait PriceProvider: Send + Sync {
    fn fetch_signed_price(&self, symbol: &str) -> Result<SignedPriceData, Box<dyn Error>>;
    fn provider_name(&self) -> &str;
    fn supported_symbols(&self) -> Vec<String>;
    
    fn fetch_batch(&self, symbols: &[&str]) -> Vec<Result<SignedPriceData, Box<dyn Error>>> {
        symbols.iter().map(|s| self.fetch_signed_price(s)).collect()
    }
}

