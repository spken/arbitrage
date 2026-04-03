#[derive(Debug, Clone)]
pub struct TradeSignal {
    pub market_id: String,
    pub condition_id: String,
    pub token_id: String,
    pub direction: String, // "yes" or "no"
    pub price: f64,
    pub size_usdc: f64,
    pub edge: f64,
    pub p_model: f64,
    pub p_market: f64,
    pub symbol: String,
    pub momentum: f64,
}
