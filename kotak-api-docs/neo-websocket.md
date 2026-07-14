# NEO WebSocket

---

## Overview

The Kotak Neo WebSocket package (`Websocket.zip`) contains 4 files:

| File        | Purpose                              |
|-------------|--------------------------------------|
| `HSLib`     | Core WebSocket library               |
| `Demo.html` | Browser-based demo UI                |
| `Demo.js`   | Demo page logic                      |
| `Neo.js`    | Neo WebSocket client implementation  |

---

## Setup — File Structure

The files must be arranged in a **specific directory structure** as shown below:

```
Main Folder/
├── hslib.js                  ← stays in the Main Folder
└── New Folder/
    ├── demo.html
    ├── demo.js
    └── neo.js
```

> **Important:** `hslib.js` must remain in the **parent (Main) folder**, while `demo.html`, `demo.js`, and `neo.js` go inside a **subfolder**. Misaligning this structure will prevent the WebSocket from initialising.

Open `demo.html` in your browser to launch the demo UI.

---

## Connecting to the WebSocket

The demo UI is titled **"Subscribing to HSM demo"** and contains the following input fields at the top:

| Field               | Description                                                              | Example             |
|---------------------|--------------------------------------------------------------------------|---------------------|
| `Token`             | Session token received after MPIN Validate                               | `eyJhbGciOiJ...`    |
| `Sid`               | Session SID received after MPIN Validate                                 | `xxxx-xxxx-xxxx`    |
| `HandshakeServers`  | Data center code returned in the login response                          | `E43`, `E41`, `123` |
| `server ID`         | Internal server identifier (usually auto-filled)                         | —                   |

> **Note:** All required values (`Token`, `Sid`, `HandshakeServers`) are returned by `POST https://mis.kotaksecurities.com/login/1.0/tradeApiValidate`.

After filling in the fields:
- Click **"Connect HSM"** to connect to the **market data** stream
- Click **"Connect HSI"** to connect to the **order updates** stream

The UI also provides **Pause** and **Resume** buttons to control active channel streams.

---

## HSM — Market Data Feed

**HSM** is the stream that delivers live market data (prices, depth, OHLC, etc.).

### Subscribe Scrip

Subscribe to individual stock/ETF data feeds using the format:

```
<exchange_segment>|<scrip_identifier>
```

**Examples:**

```
nse_cm|11536
nse_cm|11536&nse_cm|1594
```

- Use `pSymbol` from the scrip master as the scrip identifier
- Separate multiple scrips with `&` (ampersand) in a single input

### Subscribe Index

Subscribe to index data feeds using the format:

```
<exchange_segment>|<index_name>
```

**Examples:**

```
nse_cm|Nifty 50
nse_cm|Nifty 50&nse_cm|Nifty Bank
```

- Use the exact case-sensitive index name (see [Quotes glossary](./quotes.md#7-glossary-index-search-values))
- Separate multiple indices with `&` in a single input

### Subscribe Depth (Market Depth)

Subscribe to order book depth feeds using the same format as scrips:

```
<exchange_segment>|<scrip_identifier>
```

**Example:**

```
nse_cm|11536&nse_cm|1594
```

- Separate multiple depth subscriptions with `&`

---

## HSI — Order Feed

**HSI** is the stream that delivers real-time order updates for orders you have placed.

Connect to HSI to receive live order status updates. Updates will appear in the **Streaming Orders** column of the demo UI.

---

## Limits

| Limit                         | Value |
|-------------------------------|-------|
| Max channels per session       | 16    |
| Max scrips subscribed at once  | 200   |

---

## Integration

To integrate WebSocket feeds into your own code:
1. Open `demo.html` in a browser
2. Right-click → **Inspect** (or press `F12`)
3. Go to the **Network → WS** tab
4. Observe the WebSocket connection strings and messages

The demo UI streams data into two areas:
- **Streaming Scrips** — live price/market feed for subscribed instruments
- **Streaming Orders** — live order status updates

---

## WebSocket Response Field Mapping

### Price Feed Fields

| Field  | Meaning                  |
|--------|--------------------------|
| `tk`   | Exchange Token           |
| `ts`   | Trading Symbol           |
| `e`    | Exchange                 |
| `ltp`  | Last Traded Price        |
| `ltq`  | Last Traded Quantity     |
| `tbq`  | Total Buy Quantity       |
| `tsq`  | Total Sell Quantity      |
| `bp`   | Best Bid Price           |
| `bq`   | Best Bid Quantity        |
| `sp`   | Best Ask Price           |
| `bs`   | Best Ask Quantity        |
| `op`   | Open                     |
| `h`    | High                     |
| `lo`   | Low                      |
| `c`    | Previous Close           |
| `cng`  | Change (absolute)        |
| `nc`   | % Change                 |
| `ap`   | Average Traded Price     |
| `to`   | Turnover                 |
| `oi`   | Open Interest            |
| `ltt`  | Last Trade Time          |
| `fdtm` | Feed Time                |
| `prec` | Price Precision          |

### Additional Fields

| Field  | Meaning                              |
|--------|--------------------------------------|
| `lcl`  | Lower Circuit Limit                  |
| `ucl`  | Upper Circuit Limit                  |
| `yh`   | 52-Week High                         |
| `yl`   | 52-Week Low                          |
| `mul`  | Price / Contract Multiplier          |
| `name` | Feed Type (value: `sf`)              |
