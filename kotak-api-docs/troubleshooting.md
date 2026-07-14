# Troubleshooting — FAQs & Error Codes

---

## 🔑 Tokens & Authentication

**Q1. What is an access token, and where do I get it?**

The **access token** comes from **Neo app/web → Invest → TradeAPI → API Dashboard** (not via `/oauth2/token`). Send it as a **plain string** — no `Bearer` prefix. Resetting it **immediately invalidates** all active sessions.

**Q2. What happens if I reset the token?**

All active sessions break **instantly**. Re-login (TOTP → MPIN Validate) to obtain a new **session token (`Auth`)** and **session sid (`Sid`)**.

**Q3. Explain all the tokens: access token, view token, session token, view sid, session sid, neo-fin-key**

| Token / Key            | Source                                  | Used For                                                                 |
|------------------------|-----------------------------------------|--------------------------------------------------------------------------|
| **Access Token**       | Neo API Dashboard                       | Login APIs + Quotes / Scripmaster                                        |
| **View Token**         | `/login/1.0/tradeApiLogin` (TOTP step)  | Input to the MPIN Validate step (sent as `Auth` header)                  |
| **View SID**           | `/login/1.0/tradeApiLogin` (TOTP step)  | Input to the MPIN Validate step (sent as `sid` header)                   |
| **Session Token**      | `/login/1.0/tradeApiValidate` (MPIN step) | All post-login APIs — sent as `Auth` header                            |
| **Session SID**        | `/login/1.0/tradeApiValidate` (MPIN step) | All post-login APIs — sent as `Sid` header                             |
| **neo-fin-key**        | Static constant                         | Always send `neotradeapi` — **except** for Quotes & Scripmaster          |

---

## ⏱️ Login & baseUrl

**Q4. What are the login endpoints?**

| Step          | Endpoint                                                                 |
|---------------|--------------------------------------------------------------------------|
| TOTP Login    | `POST https://mis.kotaksecurities.com/login/1.0/tradeApiLogin`           |
| MPIN Validate | `POST https://mis.kotaksecurities.com/login/1.0/tradeApiValidate`        |

The MPIN Validate response returns `baseUrl`, `Auth` (session token), and `Sid` (session sid).

**Q5. Is `baseUrl` static or dynamic?**

It's **stable for the day** and rarely changes after that. Always capture it from the MPIN Validate response and use it for the entire session.

**Q6. Which APIs use `baseUrl`?**

All **post-login** APIs: Orders, Reports, Portfolio, Limits, Margins, Quotes, and Scripmaster.  
Only the two login endpoints use fixed URLs.

**Q7. How do I replace `{{baseUrl}}` in practice?**

If the MPIN Validate response returns:
```
"baseUrl": "https://neo-gw.kotaksecurities.com/xyz"
```
And the spec shows:
```
{{baseUrl}}/quick/order/cancel
```
Your final URL is:
```
https://neo-gw.kotaksecurities.com/xyz/quick/order/cancel
```
> Simply substitute the full string — no braces remain in the final URL.

---

## 📋 Headers & Endpoint Usage

**Q8. Which headers do I send for each API category?**

| API Category                                          | Required Headers                                                                 |
|-------------------------------------------------------|----------------------------------------------------------------------------------|
| Login (TOTP + MPIN)                                   | `Authorization: <access_token>` + `neo-fin-key: neotradeapi`                    |
| Orders / Reports / Portfolio / Limits / Margins       | `Auth: <session_token>` + `Sid: <session_sid>` + `neo-fin-key: neotradeapi`     |
| Quotes / Scripmaster                                  | `Authorization: <access_token>` *(no `neo-fin-key`, no `Auth`/`Sid`)*           |

**Q9. Do I use `Bearer` with the Authorization header?**

No. Always pass the token **as a plain string** — no `Bearer` prefix.

---

## 🔐 TOTP Registration & Troubleshooting

**Q10. How do I register for TOTP?**

1. Go to **API Dashboard → TOTP Registration** (top-right menu)
2. Verify your **mobile OTP + client code**
3. **Scan the QR code** with Google Authenticator or Microsoft Authenticator
4. Enter the **6-digit TOTP** shown in the app
5. Confirm the **"TOTP successfully registered"** toast

**Q11. I reinstalled my authenticator app. What now?**

Deregister via the same route, then register again to get a new QR code.

**Q12. I see "Invalid TOTP" or "Service error" — what should I do?**

| Error            | Cause                                      | Fix                                                    |
|------------------|--------------------------------------------|--------------------------------------------------------|
| Invalid TOTP     | Code expired or device time is out of sync | Enable automatic time sync on your device, then retry with the latest code |
| Service error    | Too many reattempts too quickly            | Wait **5 minutes** before re-scanning the QR code      |

---

## 🌐 Static IP & Family Account Mapping

**Q13. How do I find my current network IP?**

- **Windows:** Open Command Prompt → type `ipconfig` → note the **IPv4 Address**
- **Mac:** Open Terminal → type `ipconfig getifaddr en0` (Wi-Fi) or `ipconfig getifaddr en1` (Ethernet)

**Q14. How do I get a static IP?**

Request one from your **ISP** (Airtel, Jio, ACT, etc.) — typically requires Aadhaar/KYC verification. Alternatively, use an **IP-over-VPN service** that provides a fixed address.

> Kotak Securities is not associated with any provider. Please do your own due diligence before choosing one.

**Q15. Why is a static IP required?**

For **SEBI compliance** and security — it ensures only trusted infrastructure can call your APIs. Currently **optional**, but will become **mandatory** once the relevant SEBI circular takes effect.

**Q16. How many IPs can I whitelist, and how often can I change them?**

- You can set **one primary** and **one secondary** (backup) IP
- Each can be changed **once per week**

**Q17. Can I reuse the same IP for family accounts?**

Yes — the self-serve UI lets you link up to **10 family members** to the same whitelisted IP.

**Q18. I don't have a static IP. What are my options?**

Consider using a **registered third-party platform** (e.g., **smallcase**) that manages IP whitelisting with Kotak on your behalf.

---

## 📊 API Changes

**Q19. What changed in the Portfolio Holdings response?**

The new v1 response adds richer fields including: `commonScripCode`, `logoUrl`, `cmotCode`, `unrealisedGainLoss`, `sqGainLoss`, `delGainLoss`, `marketLot`, `securitySubType`, and more.

See the [Holdings migration table](./holdings.md#7-migration-guide-old-api--v1-api-field-changes) for a full field-by-field mapping.

**Q20. What are the new endpoints for Quotes and Scripmaster?**

| API          | Endpoint                                                              | Required Header                  |
|--------------|-----------------------------------------------------------------------|----------------------------------|
| Quotes       | `{{baseUrl}}/script-details/1.0/quotes/`                              | `Authorization: <access_token>`  |
| Scripmaster  | `{{baseUrl}}/script-details/1.0/masterscrip/file-paths`               | `Authorization: <access_token>`  |

---

## 🛠️ Third-Party Platforms & SDKs

**Q21. I'm on a third-party platform. Do I need to do anything?**

Check with your provider if they've **migrated to v2**. If they have, **no action is needed** on your end.

**Q22. I use the Python SDK. Any changes required?**

**No changes** needed currently — old endpoints remain valid. For direct REST calls, follow the migration guide.

**Q23. Do you provide SDKs in other languages?**

Not at this time. Use **cURL + Postman codegen** to generate language stubs for your preferred language.

---

## 📈 Rate Limits & Versioning

**Q24. What are the rate limits?**

**10 requests per second** across all APIs. Exceeding this limit returns a rate limit error.

**Q25. What happens when v1 retires?**

Calls to old/deprecated endpoints will **fail**. Migrate to v2 endpoints before the cutover date.

---

## ✅ Error Handling Reference

### Login API

| Error                            | Cause                                            | Fix                                                      |
|----------------------------------|--------------------------------------------------|----------------------------------------------------------|
| Invalid TOTP                     | Code expired or wrong                            | Sync device time and retry with a fresh 6-digit code     |
| Invalid MPIN                     | Incorrect MPIN entered                           | Verify or reset your MPIN, then retry                    |
| Session expired (after reset)    | Access token was reset, ending all sessions      | Re-login (TOTP → MPIN Validate) to get new tokens        |
| Dependency error (`424`)         | Temporary backend issue                          | Retry after a few seconds                                |

### Orders API

| Error Code | Description          | Fix                                                                    |
|------------|----------------------|------------------------------------------------------------------------|
| `1005`     | Internal Error       | Transient issue — retry; if persistent, raise a support ticket         |
| `1006`     | Invalid Exchange     | Wrong exchange segment — use the correct `nse_cm` / `bse_cm` value     |
| `1007`     | Invalid Symbol       | Scrip not found or unsupported — validate against the scrip master      |
| `1009`     | Invalid Quantity     | Below minimum or wrong lot step — match the instrument's lot size rules |

### Reports API

| Error             | Cause                                 | Fix                                                         |
|-------------------|---------------------------------------|-------------------------------------------------------------|
| `400`             | Request format error                  | Check payload schema, field types, and required fields      |
| `401` / Expired   | Session token/sid expired or missing  | Re-login and pass valid `Auth` + `Sid` headers              |
| `424`             | Upstream dependency failure           | Retry after a short delay                                   |
