# Trading APIs — Place, Modify and Cancel Order

---

# Place Order API

## 1. Introduction

The **Place Order API** allows you to place buy or sell orders across all supported exchange segments and order types. It supports product types like NRML, CNC, MIS, CO, and BO.

## 2. API Endpoint

```bash
POST <Base URL>/quick/order/rule/ms/place
```

> Replace `<Base URL>` with the relevant Kotak environment base URL provided in the response from the `/tradeApiValidate` API.

## 3. Headers

| Name            | Type   | Description                                      |
|-----------------|--------|--------------------------------------------------|
| `accept`        | string | Always `application/json`                        |
| `Sid`           | string | Session SID generated on login                   |
| `Auth`          | string | Session token generated on login                 |
| `neo-fin-key`   | string | Static value: `neotradeapi`                      |
| `Content-Type`  | string | Always `application/x-www-form-urlencoded`       |

## 4. Request Body

The request body is sent as a single field named `jData` — a stringified, URL-encoded JSON object.

**Example Request:**

```bash
curl -X POST "<baseUrl>/quick/order/rule/ms/place" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode 'jData={
    "am": "NO",
    "dq": "0",
    "es": "nse_cm",
    "mp": "0",
    "pc": "CNC",
    "pf": "N",
    "pr": "0",
    "pt": "MKT",
    "qt": "1",
    "rt": "DAY",
    "tp": "0",
    "ts": "ITBEES-EQ",
    "tt": "B"
  }'
```

**Example `jData` Object:**

```json
{
  "am": "NO",
  "dq": "0",
  "es": "nse_cm",
  "mp": "0",
  "pc": "MIS",
  "pf": "N",
  "pr": "0",
  "pt": "L",
  "qt": "1",
  "rt": "DAY",
  "tp": "0",
  "ts": "ITBEES-EQ",
  "tt": "B"
}
```

> **⚠️ Note:** Bracket Orders (BO) and Cover Orders (CO) have been **discontinued** on Trade APIs since **Apr 1, 2026**.

### Request Body Fields

| Field | Type   | Description                                              | Allowed / Example Values                                       |
|-------|--------|----------------------------------------------------------|----------------------------------------------------------------|
| `am`  | string | After Market Order flag                                  | `"NO"` (normal), `"YES"` (AMO)                                 |
| `dq`  | string | Disclosed quantity                                       | `"0"` or a partial quantity                                    |
| `es`  | string | Exchange segment code                                    | `"nse_cm"`, `"bse_cm"`, `"nse_fo"`, `"bse_fo"`, `"cde_fo"`, `"mcx_fo"` |
| `mp`  | string | Market protection value                                  | `"0"` or numerical value                                       |
| `pc`  | string | Product code                                             | `"NRML"`, `"CNC"`, `"MIS"`, `"CO"`, `"BO"`, `"MTF"`           |
| `pf`  | string | Portfolio flag                                           | `"N"`                                                          |
| `pr`  | string | Price — `"0"` for market orders                          | `"0"`, `"450.5"`                                               |
| `pt`  | string | Order type                                               | `"L"` (Limit), `"MKT"` (Market), `"SL"` (Stop Loss), `"SL-M"` (SL-Market) |
| `qt`  | string | Order quantity                                           | `"1"`, `"100"`                                                 |
| `rt`  | string | Validity / order duration                                | `"DAY"`, `"IOC"`                                               |
| `tp`  | string | Trigger price — used for SL / SL-M / CO                 | `"0"` or actual trigger price                                  |
| `ts`  | string | Trading symbol (from scrip master)                       | `"ITBEES-EQ"`                                                  |
| `tt`  | string | Transaction type                                         | `"B"` (Buy), `"S"` (Sell)                                      |

## 5. Response

**Example Success Response:**

```json
{
  "nOrdNo": "250720000007242",
  "stat": "Ok",
  "stCode": 200
}
```

| Field     | Type   | Description                                    |
|-----------|--------|------------------------------------------------|
| `nOrdNo`  | string | Unique order number assigned to your request   |
| `stat`    | string | `"Ok"` if successful                           |
| `stCode`  | int    | HTTP status code — `200` for success           |

**Example Error Response:**

```json
{
  "stat": "Not_Ok",
  "emsg": "Insufficient balance.",
  "stCode": 1004
}
```

| Field     | Type   | Description                    |
|-----------|--------|--------------------------------|
| `stat`    | string | `"Not_Ok"` for errors          |
| `emsg`    | string | Error message in plain English |
| `stCode`  | int    | Error code                     |

## Tips & Notes

- Ensure all header tokens and session info are obtained via the authentication flow
- Use the latest scrip master file for correct trading symbols and instrument details
- Handle all non-200 status codes in your integration for robust error management

---

# Modify Order API

## 1. Introduction

The **Modify Order API** allows you to update an already placed order's parameters — such as quantity, price, validity, and product type — before it is executed or fully filled.

## 2. API Endpoint

```bash
POST <Base URL>/quick/order/vr/modify
```

> Replace `<Base URL>` with the relevant Kotak environment base URL provided in the response from the `/tradeApiValidate` API.

## 3. Headers

| Name            | Type   | Description                                      |
|-----------------|--------|--------------------------------------------------|
| `accept`        | string | Always `application/json`                        |
| `Sid`           | string | Session SID generated on login                   |
| `Auth`          | string | Session token generated on login                 |
| `neo-fin-key`   | string | Static value: `neotradeapi`                      |
| `Content-Type`  | string | Always `application/x-www-form-urlencoded`       |

## 4. Request Body

The request body uses a single field named `jData` — a URL-encoded JSON object.

**Example Request:**

```bash
curl -X POST "<baseUrl>/quick/order/vr/modify" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode 'jData={
    "am": "NO",
    "dq": "0",
    "es": "nse_cm",
    "mp": "0",
    "pc": "NRML",
    "pf": "N",
    "pr": "0",
    "pt": "MKT",
    "qt": "1",
    "rt": "DAY",
    "tp": "0",
    "ts": "TATAPOWER-EQ",
    "tt": "B",
    "no": "<orderNo>"
  }'
```

**Example `jData` Object:**

```json
{
  "tk": "11536",
  "mp": "0",
  "pc": "NRML",
  "dd": "NA",
  "dq": "0",
  "vd": "DAY",
  "ts": "TATAPOWER-EQ",
  "tt": "B",
  "pr": "3001",
  "tp": "0",
  "qt": "10",
  "no": "220106000000185",
  "es": "nse_cm",
  "pt": "L"
}
```

### Request Body Fields

| Field | Type   | Description                                                | Allowed / Example Values                                       |
|-------|--------|------------------------------------------------------------|----------------------------------------------------------------|
| `tk`  | string | Instrument token from scrip master (`pSymbol` column)      | `"11536"`                                                      |
| `fq`  | string | Filled quantity *(optional)*                               | `"10"`, `"0"`                                                  |
| `mp`  | string | Market protection value                                    | `"0"`                                                          |
| `pc`  | string | Product code                                               | `"NRML"`, `"CNC"`, `"MIS"`, `"CO"`, `"BO"`                    |
| `dd`  | string | Date/days — trailing validity, if applicable               | `"NA"` or as required                                          |
| `dq`  | string | Disclosed quantity                                         | `"0"` or a partial quantity                                    |
| `vd`  | string | Validity / order duration                                  | `"DAY"`, `"IOC"`                                               |
| `ts`  | string | Trading symbol (from scrip master)                         | `"TCS-EQ"`                                                     |
| `tt`  | string | Transaction type                                           | `"B"` (Buy), `"S"` (Sell)                                      |
| `pr`  | string | Price                                                      | `"3001"`                                                       |
| `tp`  | string | Trigger price — for SL / SL-M orders                       | `"0"` or actual trigger price                                  |
| `qt`  | string | Quantity                                                   | `"10"`                                                         |
| `no`  | string | Nest Order Number — system order ID of the original order  | `"220106000000185"`                                            |
| `es`  | string | Exchange segment                                           | `"nse_cm"`, `"bse_cm"`, `"nse_fo"`, `"bse_fo"`, `"cde_fo"`   |
| `pt`  | string | Order type                                                 | `"L"` (Limit), `"MKT"` (Market), `"SL"`, `"SL-M"`            |

## 5. Response

**Example Success Response:**

```json
{
  "nOrdNo": "250720000007242",
  "stat": "Ok",
  "stCode": 200
}
```

| Field     | Type   | Description                              |
|-----------|--------|------------------------------------------|
| `nOrdNo`  | string | New or modified order number             |
| `stat`    | string | `"Ok"` if modification was successful    |
| `stCode`  | int    | HTTP status code — `200` for success     |

**Example Error Response:**

```json
{
  "stat": "Not_Ok",
  "emsg": "Order cannot be modified as it is already executed.",
  "stCode": 1006
}
```

| Field     | Type   | Description                    |
|-----------|--------|--------------------------------|
| `stat`    | string | `"Not_Ok"` for errors          |
| `emsg`    | string | Error message in plain English |
| `stCode`  | int    | Error code                     |

## Notes

- Only orders that are **not yet executed or completed** can be modified
- Always use valid instrument tokens, symbols, and original order numbers
- Use the latest scrip master data for token and symbol lookups
- Handle all non-200 and failure responses appropriately

---

# Cancel Order API

## 1. Introduction

The **Cancel Order API** allows cancellation of open orders that have not yet been executed.

## 2. API Endpoint

```bash
POST <Base URL>/quick/order/cancel
```

> Replace `<Base URL>` with the relevant Kotak environment base URL provided in the response from the `/tradeApiValidate` API.

## 3. Headers

| Name            | Type   | Description                                      |
|-----------------|--------|--------------------------------------------------|
| `accept`        | string | Always `application/json`                        |
| `Sid`           | string | Session SID generated on login                   |
| `Auth`          | string | Session token generated on login                 |
| `neo-fin-key`   | string | Static value: `neotradeapi`                      |
| `Content-Type`  | string | Always `application/x-www-form-urlencoded`       |

## 4. Request Body

The request body is a single URL-encoded field named `jData`, containing a JSON object.

**Example Request:**

```bash
curl -X POST "<baseUrl>/quick/order/cancel" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode 'jData={"on":"<orderNo>","am":"NO"}'
```

**Example `jData` Object:**

```json
{
  "am": "NO",
  "on": "2105199703091997",
  "ts": ""
}
```

> `ts` (trading symbol) is optional for regular orders but **mandatory for AMO cancellations**.

### Request Body Fields

| Field | Type   | Description                                              | Required              | Example              |
|-------|--------|----------------------------------------------------------|-----------------------|----------------------|
| `am`  | string | AMO flag — `"YES"` for AMO orders, `"NO"` for others    | Optional              | `"YES"`, `"NO"`      |
| `on`  | string | Nest order number (unique order ID)                      | **Required**          | `"2105199703091997"` |
| `ts`  | string | Trading symbol — mandatory for AMO cancellations         | Optional (AMO: Required) | `"TCS-EQ"`        |

## 5. Response

**Example Success Response:**

```json
{
  "nOrdNo": "2105199703091997",
  "stat": "Ok",
  "stCode": 200
}
```

| Field     | Type   | Description                                    |
|-----------|--------|------------------------------------------------|
| `nOrdNo`  | string | Nest order number of the cancelled order       |
| `stat`    | string | `"Ok"` if cancellation was successful          |
| `stCode`  | int    | HTTP status code — `200` for success           |

**Example Error Response:**

```json
{
  "stat": "Not_Ok",
  "emsg": "Order already cancelled or not found.",
  "stCode": 1006
}
```

| Field     | Type   | Description                    |
|-----------|--------|--------------------------------|
| `stat`    | string | `"Not_Ok"` for errors          |
| `emsg`    | string | Error message in plain English |
| `stCode`  | int    | Error code                     |

## 6. Usage Notes

- For **AMO cancellations**, `"am": "YES"` and `"ts"` (trading symbol) are **mandatory**
- Orders already fully executed or cancelled **cannot** be cancelled again
- Use the exact `on` (Nest order number) as returned from order placement or status queries
- Always verify `"stat": "Ok"` in the response to confirm successful cancellation
