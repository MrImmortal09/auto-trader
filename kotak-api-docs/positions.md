# Positions API

## 1. Introduction

The **Positions API** provides a consolidated view of your open and closed positions for the current trading day across all supported segments. This allows you to track real-time exposures, quantities bought/sold, and settlement data.

---

## 2. API Endpoint

```bash
GET <Base URL>/quick/user/positions
```

> Replace `<Base URL>` with the relevant Kotak environment base URL provided in the response from the `/tradeApiValidate` API.

---

## 3. Headers

| Name          | Type   | Description                                  |
|---------------|--------|----------------------------------------------|
| `accept`      | string | Always `application/json`                    |
| `Sid`         | string | Session SID generated on login               |
| `Auth`        | string | Session token generated on login             |
| `neo-fin-key` | string | Static value: `neotradeapi`                  |
| `Content-Type`| string | Always `application/x-www-form-urlencoded`   |

---

## 4. Request

No request body or query parameters required.

**Example Request:**

```bash
curl -X GET "<baseUrl>/quick/user/positions" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi"
```

---

## 5. Response

### Example Success Response

```json
{
  "stat": "Ok",
  "stCode": 200,
  "data": [
    {
      "actId": "******",
      "prod": "CNC",
      "exSeg": "nse_cm",
      "trdSym": "AXISBANK-EQ",
      "sym": "AXISBANK",
      "qty": "9",
      "buyAmt": "5862.90",
      "sellAmt": "0.00",
      "flBuyQty": "9",
      "flSellQty": "0",
      "brdLtQty": 1,
      "posFlg": "true",
      "sqrFlg": "Y",
      "lotSz": "1",
      "stkPrc": "0.00",
      "hsUpTm": "2022/06/21 15:11:02"
    },
    {
      "actId": "******",
      "prod": "CNC",
      "exSeg": "nse_cm",
      "trdSym": "ITC-EQ",
      "sym": "ITC",
      "qty": "15",
      "buyAmt": "4021.50",
      "sellAmt": "0.00",
      "flBuyQty": "15",
      "flSellQty": "0",
      "brdLtQty": 1,
      "posFlg": "true",
      "sqrFlg": "Y",
      "lotSz": "1",
      "stkPrc": "0.00",
      "hsUpTm": "2022/06/21 15:11:02"
    }
  ]
}
```

### Top-Level Response Fields (HTTP 200)

| Field     | Type   | Description                              |
|-----------|--------|------------------------------------------|
| `stat`    | string | Overall status — `"Ok"` for success      |
| `stCode`  | int    | Status code — `200` for success          |
| `data`    | array  | Array of position objects (see below)    |

### Position Object Fields

| Field       | Type   | Description                                          |
|-------------|--------|------------------------------------------------------|
| `actId`     | string | Account ID                                           |
| `prod`      | string | Product code (e.g., `CNC`, `MIS`, `NRML`)           |
| `exSeg`     | string | Exchange segment (e.g., `nse_cm`, `bse_cm`)          |
| `trdSym`    | string | Trading symbol (e.g., `AXISBANK-EQ`)                 |
| `sym`       | string | Symbol name (e.g., `AXISBANK`)                       |
| `qty`       | string | Net position quantity                                |
| `buyAmt`    | string | Total buy amount                                     |
| `sellAmt`   | string | Total sell amount                                    |
| `flBuyQty`  | string | Filled buy quantity                                  |
| `flSellQty` | string | Filled sell quantity                                 |
| `brdLtQty`  | int    | Board lot quantity                                   |
| `posFlg`    | string | Position flag — `"true"` if position is open         |
| `sqrFlg`    | string | Square-off flag — `"Y"` = allowed                    |
| `lotSz`     | string | Lot size                                             |
| `stkPrc`    | string | Strike price (for derivatives)                       |
| `hsUpTm`    | string | Last updated timestamp                               |

**Additional available fields:**

| Field        | Description                                     |
|--------------|-------------------------------------------------|
| `cfBuyAmt`   | Carry-forward buy amount                        |
| `cfSellAmt`  | Carry-forward sell amount                       |
| `cfBuyQty`   | Carry-forward buy quantity                      |
| `cfSellQty`  | Carry-forward sell quantity                     |
| `multiplier` | Contract multiplier                             |
| `precision`  | Price precision                                 |
| `expDt`      | Expiry date (for F&O)                           |
| `genNum`     | General numerator                               |
| `genDen`     | General denominator                             |
| `prcNum`     | Price numerator                                 |
| `prcDen`     | Price denominator                               |
| `optTp`      | Option type (CE / PE)                           |
| `type`       | Instrument type                                 |

> **Note:** Fields prefixed with `cf` refer to carry-forward values.

### Example Error Response

```json
{
  "stat": "Not_Ok",
  "emsg": "Invalid session",
  "stCode": 1003
}
```

| Field    | Type   | Description                    |
|----------|--------|--------------------------------|
| `stat`   | string | `"Not_Ok"` for errors          |
| `emsg`   | string | Error message in plain English |
| `stCode` | int    | Error code                     |

---

## 6. Notes

- Only positions with actual trades for the current day will be listed
- Always use the latest session and auth tokens
- Quantities and amounts are returned as strings for precision — convert as needed in your application
- Refer to the scrip master for segment, instrument, and symbol details
