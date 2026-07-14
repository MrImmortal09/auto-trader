# Margins API

## 1. Introduction

The **Margin API** allows you to check the required margin and available balance for a new order. This API helps you validate whether you have enough margin **before** placing an order.

---

## 2. API Endpoint

```bash
POST <Base URL>/quick/user/check-margin
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
curl --location '<Base URL>/quick/user/check-margin' \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  --data-urlencode 'jData={
    "brkName": "KOTAK",
    "brnchId": "ONLINE",
    "exSeg": "nse_cm",
    "prc": "12500",
    "prcTp": "L",
    "prod": "CNC",
    "qty": "1",
    "tok": "11536",
    "trnsTp": "B"
  }'
```

### Request Body (`jData`) Fields

**Required fields:**

| Field     | Type   | Description                                         | Example    |
|-----------|--------|-----------------------------------------------------|------------|
| `brkName` | string | Broker name — always send `KOTAK`                   | `KOTAK`    |
| `brnchId` | string | Branch ID — always send `ONLINE`                    | `ONLINE`   |
| `exSeg`   | string | Exchange segment                                    | `nse_cm`   |
| `prc`     | string | Order price in Rupees — `"0"` for market orders     | `12500`    |
| `prcTp`   | string | Order type                                          | `L`, `MKT` |
| `prod`    | string | Product code                                        | `CNC`, `NRML`, `MIS` |
| `qty`     | string | Order quantity                                      | `1`        |
| `tok`     | string | Instrument token from scrip master (`pSymbol`)      | `11536`    |
| `trnsTp`  | string | Transaction type                                    | `B` (Buy), `S` (Sell) |

**Optional fields (BO / CO only):**

| Field            | Type   | Description                                              |
|------------------|--------|----------------------------------------------------------|
| `slAbsOrTks`     | string | Stop loss type — `Absolute` or `Ticks` *(BO only)*       |
| `slVal`          | string | Stop loss value *(BO only)*                              |
| `sqrOffAbsOrTks` | string | Square-off type — `Absolute` or `Ticks` *(BO only)*      |
| `sqrOffVal`      | string | Square-off value *(BO only)*                             |
| `trailSL`        | string | Trailing stop loss — `Y` or `N` *(BO only)*              |
| `tSLTks`         | string | Trailing SL ticks value *(BO only)*                      |
| `trgPrc`         | string | Trigger price in Rupees *(CO only)*                      |

---

## 5. Response

### Example Success Response

```json
{
  "avlCash": "10197.480000",
  "totMrgnUsd": "12540.400000",
  "mrgnUsd": "40.400000",
  "ordMrgn": "12500.000000",
  "rmsVldtd": "NOT_OK",
  "reqdMrgn": "12540.400000",
  "avlMrgn": "10197.480000",
  "insufFund": "2342.920000",
  "stat": "Ok",
  "stCode": 200
}
```

### Response Fields (HTTP 200)

| Field        | Type   | Description                                             |
|--------------|--------|---------------------------------------------------------|
| `avlCash`    | string | Total cash available in the account                     |
| `avlMrgn`    | string | Available margin after the order would be placed        |
| `insufFund`  | string | Shortfall — additional funds needed to place the order  |
| `mrgnUsd`    | string | Margin already in use                                   |
| `ordMrgn`    | string | Margin required for this specific order                 |
| `reqdMrgn`   | string | Net total margin required for the order                 |
| `rmsVldtd`   | string | RMS validation result — `OK` or `NOT_OK`                |
| `totMrgnUsd` | string | Total margin consumed across all positions              |
| `stat`       | string | `"Ok"` for success                                      |
| `stCode`     | int    | HTTP status code — `200` for success                    |

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

- Call this API **before** placing any order to verify sufficient funds/margin
- For market orders, send `prc` as `"0"`
- Include the BO/CO optional fields only when checking margin for those specific order types
- If `rmsVldtd` returns `"NOT_OK"`, check `insufFund` for the shortfall amount
- Always use valid session and auth tokens to avoid authentication errors
