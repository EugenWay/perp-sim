// src/api/cache.rs
// Cached price provider to prevent excessive API calls.

use super::provider::{PriceProvider, SignedPriceData};
use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Wrapper around a price provider that caches results
pub struct CachedPriceProvider<P: PriceProvider> {
    inner: P,
    cache: Arc<Mutex<HashMap<String, (SignedPriceData, u64)>>>,
    cache_duration_secs: u64,
}

impl<P: PriceProvider> CachedPriceProvider<P> {
    /// Create new cached provider
    pub fn new(provider: P, cache_duration_secs: u64) -> Self {
        Self {
            inner: provider,
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_duration_secs,
        }
    }
    
    fn current_time() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

impl<P: PriceProvider> PriceProvider for CachedPriceProvider<P> {
    fn fetch_signed_price(&self, symbol: &str) -> Result<SignedPriceData, Box<dyn Error>> {
        let now = Self::current_time();
        
        // Check cache
        {
            let cache = self.cache.lock().unwrap();
            if let Some((data, cached_at)) = cache.get(symbol) {
                if now - cached_at < self.cache_duration_secs {
                    println!("[Cache] using cached price for {}", symbol);
                    return Ok(data.clone());
                }
            }
        }
        
        // Fetch fresh data
        let data = self.inner.fetch_signed_price(symbol)?;
        
        // Update cache
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(symbol.to_string(), (data.clone(), now));
        }
        
        Ok(data)
    }
    
    fn provider_name(&self) -> &str {
        self.inner.provider_name()
    }
    
    fn supported_symbols(&self) -> Vec<String> {
        self.inner.supported_symbols()
    }
    
    fn fetch_batch(&self, symbols: &[&str]) -> Vec<Result<SignedPriceData, Box<dyn Error>>> {
        let now = Self::current_time();
        let mut results = Vec::new();
        let mut uncached_symbols = Vec::new();
        let mut uncached_indices = Vec::new();
        
        // Check cache for each symbol
        for (i, symbol) in symbols.iter().enumerate() {
            let cached = {
                let cache = self.cache.lock().unwrap();
                cache.get(*symbol).and_then(|(data, cached_at)| {
                    if now - cached_at < self.cache_duration_secs {
                        Some(data.clone())
                    } else {
                        None
                    }
                })
            };
            
            match cached {
                Some(data) => {
                    println!("[Cache] using cached price for {}", symbol);
                    results.push(Ok(data));
                }
                None => {
                    uncached_symbols.push(*symbol);
                    uncached_indices.push(i);
                    results.push(Err("Fetching...".into())); // Placeholder
                }
            }
        }
        
        // Fetch uncached symbols
        if !uncached_symbols.is_empty() {
            let fresh_data = self.inner.fetch_batch(&uncached_symbols);
            
            // Update cache and results
            let mut cache = self.cache.lock().unwrap();
            for (idx, data_result) in uncached_indices.iter().zip(fresh_data.into_iter()) {
                if let Ok(data) = &data_result {
                    cache.insert(data.symbol.clone(), (data.clone(), now));
                }
                results[*idx] = data_result;
            }
        }
        
        results
    }
}

