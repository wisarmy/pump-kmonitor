# pump-kmonitor

A real-time K-line monitoring system for Pump.fun and PumpSwap Amm tokens with automated strategy detection and notification capabilities.

![Web Interface Preview](static/preview.png)

## Features

- 🔍 **Real-time K-line monitoring**: WebSocket connection to Pump.fun for live trading data
- 📈 **Strategy detection**: Automated pattern recognition for consecutive rising candles
- 🌐 **Web interface**: Interactive dashboard for viewing K-line data and statistics
- 🔔 **Notification system**: DingTalk integration with customizable alerts
- 💾 **Redis storage**: Efficient data storage and retrieval with automatic cleanup
- 🎯 **Pattern analysis**: Detects 4 consecutive bullish candles with increasing gains

## Quick Start

### Prerequisites

- **Rust** (latest stable version) - [Install Rust](https://rustup.rs/)
- **Redis** server - [Install Redis](https://redis.io/download)

### Installation

1. **Clone the repository:**
```bash
git clone https://github.com/wisarmy/pump-kmonitor.git
cd pump-kmonitor
```

2. **Install dependencies:**
```bash
cargo build --release
```

### Configuration

1. **Copy environment configuration:**
```bash
cp .env.example .env
```

## Commands

### 1. Monitor Command 📊
Start the monitoring service to collect K-line data from Pump.fun WebSocket:

```bash
# monitor pump
pump-kmonitor monitor
# monitor pumpswap amm
pump-kmonitor monitor-amm
```

### 2. Web Command 🌐
Start the web service to view K-line data through an interactive dashboard:

```bash
# Start with default port 8080
pump-kmonitor web

# Start with custom port
pump-kmonitor web --port 3000
```

### 3. Strategy Command 🎯
Run automated strategy detection to identify trading patterns:

```bash
# Run once and exit
pump-kmonitor strategy --once

# Run continuously with default 10-second interval
pump-kmonitor strategy

# Run continuously with custom interval (in seconds)
pump-kmonitor strategy --interval 60
```
