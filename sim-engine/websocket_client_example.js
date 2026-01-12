// WebSocket Client Example for Perp DEX Simulator
// This shows how to connect and receive liquidation events

const ws = new WebSocket('ws://localhost:8081');

ws.onopen = () => {
    console.log('✅ Connected to simulator WebSocket');
    
    // Send a command to open a position
    ws.send(JSON.stringify({
        action: 'open',
        symbol: 'ETH-USD',
        side: 'long',
        qty: 5,
        leverage: 20
    }));
};

ws.onmessage = (event) => {
    const data = JSON.parse(event.data);
    
    // Handle different event types
    if (data.type === 'Event') {
        const simEvent = data.payload;
        
        switch (simEvent.event_type) {
            case 'PositionLiquidated':
                console.log('🔥 LIQUIDATION EVENT:', {
                    account: simEvent.account,
                    symbol: simEvent.symbol,
                    side: simEvent.side,
                    size_usd: simEvent.size_usd / 1_000_000,
                    collateral_lost: simEvent.collateral_lost / 1_000_000,
                    pnl: simEvent.pnl / 1_000_000,
                    liquidation_price: simEvent.liquidation_price / 1_000_000,
                });
                break;
                
            case 'OrderExecuted':
                if (simEvent.order_type === 'Liquidation') {
                    console.log('⚠️  Liquidation order executed:', simEvent);
                } else {
                    console.log('✅ Order executed:', simEvent.order_type);
                }
                break;
                
            case 'PositionSnapshot':
                if (simEvent.is_liquidatable) {
                    console.warn('⚠️  Position at risk:', {
                        account: simEvent.account,
                        symbol: simEvent.symbol,
                        liquidation_price: simEvent.liquidation_price / 1_000_000,
                        current_price: simEvent.current_price / 1_000_000,
                    });
                }
                break;
                
            case 'OracleTick':
                // Price updates (can be noisy, comment out if needed)
                // console.log('📊 Price:', simEvent.symbol, (simEvent.price_min + simEvent.price_max) / 2_000_000);
                break;
        }
    } else if (data.type === 'Response') {
        console.log('📨 Response:', data.payload);
    } else if (data.type === 'Error') {
        console.error('❌ Error:', data.payload);
    }
};

ws.onerror = (error) => {
    console.error('WebSocket error:', error);
};

ws.onclose = () => {
    console.log('❌ Disconnected from WebSocket');
};

