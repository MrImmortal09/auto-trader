# cURL Examples

---

## 🔑 Login Flow (Fixed Endpoints)

### 1. TOTP Login → returns `viewToken` + `viewSid`

```bash
curl -X POST "https://mis.kotaksecurities.com/login/1.0/tradeApiLogin" \
  -H "Authorization: <access_token>" \
  -H "neo-fin-key: neotradeapi" \
  -H "Content-Type: application/json" \
  -d '{
        "mobileNumber": "<+91XXXXXXXXXX>",
        "ucc": "<client_code>",
        "totp": "<6_digit_totp>"
      }'
```

### 2. MPIN Validate → returns `Auth` (session token) + `Sid` (session sid) + `baseUrl`

> Pass `viewSid` and `viewToken` from Step 1 as headers in this call.

```bash
curl -X POST "https://mis.kotaksecurities.com/login/1.0/tradeApiValidate" \
  -H "Authorization: <access_token>" \
  -H "neo-fin-key: neotradeapi" \
  -H "sid: <viewSid_from_previous_step>" \
  -H "Auth: <viewToken_from_previous_step>" \
  -H "Content-Type: application/json" \
  -d '{
        "mpin": "<mpin>"
      }'
```

**📌 This response gives you:**

- `baseUrl` — use it for **all** post-login APIs
- `token` → use as `Auth` header
- `sid` → use as `Sid` header

---

## 🔁 Using `baseUrl`

If the MPIN Validate response returned:

```
"baseUrl": "https://neo-gw.kotaksecurities.com/xyz"
```

And the API spec shows:

```
{{baseUrl}}/quick/order/cancel
```

Your **final URL** is:

```
https://neo-gw.kotaksecurities.com/xyz/quick/order/cancel
```

> Simply replace `{{baseUrl}}` with the returned string. No braces in the final URL.

---

## 🧾 Orders (`Content-Type: application/x-www-form-urlencoded`, `jData` body)

### Place Order

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

### Modify Order

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

### Cancel Order

```bash
curl -X POST "<baseUrl>/quick/order/cancel" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode 'jData={"on":"<orderNo>","am":"NO"}'
```

### Exit Cover Order

```bash
curl -X POST "<baseUrl>/quick/order/co/exit" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode 'jData={"on":"<orderNo>","am":"NO"}'
```

### Exit Bracket Order

```bash
curl -X POST "<baseUrl>/quick/order/bo/exit" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode 'jData={"on":"<orderNo>","am":"NO"}'
```

---

## 📑 Reports

### Order Book (GET)

```bash
curl -X GET "<baseUrl>/quick/user/orders" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi"
```

### Order History (POST, URL-encoded)

```bash
curl -X POST "<baseUrl>/quick/order/history" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode 'jData={"nOrdNo":"250720000007242"}'
```

### Trade Book (GET)

```bash
curl -X GET "<baseUrl>/quick/user/trades" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi"
```

### Position Book (GET)

```bash
curl -X GET "<baseUrl>/quick/user/positions" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi"
```

### Portfolio Holdings (GET)

```bash
curl -X GET "<baseUrl>/portfolio/v1/holdings" \
  -H "Auth: <session_token>" \
  -H "Sid: <session_sid>" \
  -H "neo-fin-key: neotradeapi"
```

---

## 📈 Quotes (GET)

> ⚠️ **Do NOT send** `neo-fin-key`, `Auth`, or `Sid` for this endpoint — only `Authorization` is required.

```bash
curl -X GET "<baseUrl>/script-details/1.0/quotes/neosymbol/nse_cm|26000/all" \
  -H "Authorization: <access_token>"
```

---

## 📂 Scripmaster Files (GET)

> ⚠️ **Do NOT send** `neo-fin-key`, `Auth`, or `Sid` for this endpoint — only `Authorization` is required.

```bash
curl -X GET "<baseUrl>/script-details/1.0/masterscrip/file-paths" \
  -H "Authorization: <access_token>"
```

---

## Header Reference Summary

| API Group                          | `Authorization` | `Auth` + `Sid` | `neo-fin-key` | `Content-Type`                        |
|------------------------------------|:---------------:|:--------------:|:-------------:|---------------------------------------|
| Login (TOTP & MPIN)                | ✅              | ✅ (MPIN step) | ✅            | `application/json`                    |
| Orders (Place / Modify / Cancel)   | ❌              | ✅             | ✅            | `application/x-www-form-urlencoded`   |
| Reports (Order Book / Trades etc.) | ❌              | ✅             | ✅            | `application/json` (GET) / urlencoded (POST) |
| Quotes                             | ✅              | ❌             | ❌            | —                                     |
| Scripmaster                        | ✅              | ❌             | ❌            | —                                     |
