# Instruments — Scrip Master API

## 1. Introduction

The Scrip Master API provides direct download links to the latest scrip master (instrument master) CSV files for all supported exchange segments. These files include instrument tokens, symbols, and other key data required for trading activities and symbol lookups.

---

## 2. API Endpoint

```bash
GET <Base URL>/script-details/1.0/masterscrip/file-paths
```

> Replace `<Base URL>` with the relevant Kotak environment base URL provided in the response from the `/tradeApiValidate` API.

---

## 3. Headers

| Name            | Type   | Description                                         |
|-----------------|--------|-----------------------------------------------------|
| `Authorization` | string | Token from your NEO API dashboard — use plain token |

---

## 4. Request

No request parameters or request body required.

**Example Request:**

```bash
curl --location '<Base URL>/script-details/1.0/masterscrip/file-paths' \
  --header 'Authorization: xxxxx-your-neo-token-xxxx'
```

---

## 5. Response

**Example Success Response:**

```json
{
  "data": {
    "filesPaths": [
      "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/yyyy-mm-dd/transformed/cde_fo.csv",
      "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/yyyy-mm-dd/transformed/mcx_fo.csv",
      "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/yyyy-mm-dd/transformed/nse_fo.csv",
      "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/yyyy-mm-dd/transformed/bse_fo.csv",
      "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/yyyy-mm-dd/transformed/nse_com.csv",
      "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/yyyy-mm-dd/transformed-v1/bse_cm-v1.csv",
      "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/yyyy-mm-dd/transformed-v1/nse_cm-v1.csv"
    ],
    "baseFolder": "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod"
  }
}
```

### Response Fields (HTTP 200)

| Field         | Type   | Description                                                      |
|---------------|--------|------------------------------------------------------------------|
| `filesPaths`  | array  | Array of URLs — each is a download link to a CSV for one segment |
| `baseFolder`  | string | The root URL for retrieving CSV files (read-only)                |

### Error Codes

| Code  | Description                              |
|-------|------------------------------------------|
| `401` | Unauthorized: Invalid or missing API token |
| `403` | Forbidden: Not enough privileges         |
| `429` | Too Many Requests: API rate limited      |
| `500` | Server Error: Unexpected system error    |

---

## 6. CSV Column Mapping to Orders & WebSocket APIs

| CSV Column    | Maps To                                          | Description                                                                                                                                                                                                                                          |
|---------------|--------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `pSymbol`     | WebSocket and Quotes API                         | Passed along with `pExchSeg` with a `\|` separator. Example: `nse_cm\|11536&nse_cm\|1594`                                                                                                                                                           |
| `pExchSeg`    | Orders API → `es` field; WebSocket & Quotes API URL | Expected values: `nse_cm`, `bse_cm`, `nse_fo`, `bse_fo`, `cde_fo`. Passed as a string in the Orders API.                                                                                                                                           |
| `pTrdSymbol`  | Orders API → `ts` field                          | The trading instrument symbol as interpreted by the Orders API; passed as a string                                                                                                                                                                    |
| `lLotSize`    | Orders API → `qt` field                          | Quantity sent in place order must be a multiple of the lot size                                                                                                                                                                                      |
| `lExpiryDate` | *(reference only)*                               | Expiry date for F&O contracts. **Conversion rules:** <br>• `nse_fo` / `cde_fo`: Add `315511200` to the epoch value, then convert to IST <br>• `mcx_fo` / `bse_fo`: Epoch (`lExpiryDate`) can be directly converted to a human-readable date |

---

## 7. Example Workflow

1. Make a `GET` request with your `Authorization` token to the endpoint above
2. On success, parse `filesPaths` to get the CSV download URLs for each exchange segment (e.g., NSE F&O, BSE CM)
3. Download and use the CSV files to map instrument tokens to trading symbols and other details in your trading application

---

## Notes

- Always use the latest download links — files update frequently (usually daily)
- Each file relates to a specific exchange segment (e.g., `nse_fo`, `bse_cm`)
- Download and cache these CSVs locally for fast symbol lookups
- Always secure your API token and never share confidential links or files publicly
