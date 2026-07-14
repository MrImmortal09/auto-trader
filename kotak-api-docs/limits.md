# Limits API

## 1. Introduction

The **Limits API** allows you to query real-time available limits, margins, collateral, exposure, and cash balances for your trading account, filtered by segment, exchange, and product type.

---

## 2. API Endpoint

```bash
POST <Base URL>/quick/user/limits
```

> Replace `<Base URL>` with the relevant Kotak environment base URL provided in the response from the `/tradeApiValidate` API.

---

## 3. Headers

| Name            | Type   | Description                                  |
|-----------------|--------|----------------------------------------------|
| `accept`        | string | Always `application/json`                    |
| `Sid`           | string | Session SID generated on login               |
| `Auth`          | string | Session token generated on login             |
| `neo-fin-key`   | string | Static value: `neotradeapi`                  |
| `Content-Type`  | string | Always `application/x-www-form-urlencoded`   |

---

## 4. Request

**Example Request:**

```bash
curl --location '<Base URL>/quick/user/limits' \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  --data-urlencode 'jData={"seg":"ALL","exch":"ALL","prod":"ALL"}'
```

### Request Body (`jData`) Fields

| Field  | Type   | Description                        | Allowed Values               | Default |
|--------|--------|------------------------------------|------------------------------|---------|
| `seg`  | string | Segment to fetch limits for        | `ALL`, `CASH`, `CUR`, `FO`   | `ALL`   |
| `exch` | string | Exchange to fetch limits for       | `ALL`, `NSE`, `BSE`          | `ALL`   |
| `prod` | string | Product type to fetch limits for   | `ALL`, `NRML`, `CNC`, `MIS`  | `ALL`   |

> Use `"ALL"` for any parameter to fetch a consolidated report across all segments/exchanges/products.

---

## 5. Response

### Example Success Response

```json
{
  "Category": "CLIENT_MTF",
  "EntityId": "account-******",
  "BoardLotLimit": "5000",
  "CollateralValue": "10197.48",
  "Net": "10157.08",
  "MarginUsed": "40.4",
  "AdhocMargin": "0",
  "SpanMarginPrsnt": "0",
  "ExposureMarginPrsnt": "0",
  "NotionalCash": "0",
  "UnrealizedMtomPrsnt": "0",
  "RealizedMtomPrsnt": "0",
  "SpecialMarginPrsnt": "0",
  "PremiumPrsnt": "0",
  "MarginVarPrsnt": "0",
  "stCode": 200,
  "stat": "Ok"
}
```

### Response Fields (HTTP 200)

| Field                  | Type   | Description                                          |
|------------------------|--------|------------------------------------------------------|
| `Category`             | string | Account/limit category (e.g., `CLIENT_MTF`)          |
| `EntityId`             | string | Account ID                                           |
| `BoardLotLimit`        | string | Board lot limit                                      |
| `CollateralValue`      | string | Value of pledged securities / collateral             |
| `Net`                  | string | Net available margin / cash                          |
| `MarginUsed`           | string | Margin already consumed                              |
| `AdhocMargin`          | string | Extra margin added on an ad-hoc basis                |
| `SpanMarginPrsnt`      | string | SPAN margin requirement                              |
| `ExposureMarginPrsnt`  | string | Exposure margin requirement                          |
| `NotionalCash`         | string | Notional (total) cash                                |
| `UnrealizedMtomPrsnt`  | string | Unrealized Mark-to-Market P&L                        |
| `RealizedMtomPrsnt`    | string | Realized Mark-to-Market P&L                          |
| `SpecialMarginPrsnt`   | string | Special margin imposed                               |
| `PremiumPrsnt`         | string | Premium margin present                               |
| `MarginVarPrsnt`       | string | VAR margin present                                   |
| `stCode`               | int    | Status code — `200` for success                      |
| `stat`                 | string | `"Ok"` for success                                   |
| `...`                  | —      | Additional technical / segment breakdown fields      |

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

- All numerical limits are returned as strings for precision — convert as needed in your application
- The response includes total cash, margin, and product/segment-wise breakdowns
- Use `"ALL"` for any `jData` parameter to fetch a fully consolidated report
- Key fields for UI display: `CollateralValue`, `Net`, `MarginUsed`
- Always use valid session and auth tokens to avoid authentication errors
