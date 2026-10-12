const WebSocket = require('ws');
const readline = require('readline');

// Bridge to Kotak Neo's market-data WebSocket ("SFeed", native_batch protocol).
// Control messages are JSON text frames; ticks arrive as binary frames holding
// a batch of little-endian packed packets. Protocol reference:
// https://www.kotakneo.com/uploads/kotak_neo_market_data_websocket_guide_38e4831feb.pdf

const HEADER_SIZE = 9;
const MSG_MARKET_PICTURE = 7208;
const MSG_SUBSCRIBE_ACK = 1109;
const MSG_AUTH_OK = 1117;
const MSG_CONNECTED = 1119;
const MSG_AUTH_FAILED = 1120;
const LEVEL_MINI_TOUCH_LINE = 1;
const LEVELS_MARKET_PICTURE = [2, 4, 8];
const MINI_TOUCH_LINE_SIZE = 54;
const MARKET_PICTURE_FIXED_SIZE = 144;
const DEFAULT_DIVIDER = 100;

// Liveness: a quiet feed is asked for a snapshot; one left unanswered is dead.
const QUIET_PROBE_MS = 15000;
const WATCHDOG_TIMEOUT_MS = 30000;

// exchange_id byte of the binary header -> segment string used in scrip keys.
const EXCHANGE_BY_ID = {
    1: 'nse_cm', 2: 'nse_fo', 3: 'cde_fo', 4: 'nse_com', 5: 'bse_cm',
    6: 'bse_fo', 7: 'bse_cd', 8: 'bse_co', 9: 'mcx_fo', 10: 'ncd_co'
};
const EXCHANGE_ID_BY_NAME = Object.fromEntries(
    Object.entries(EXCHANGE_BY_ID).map(([id, name]) => [name, Number(id)])
);

// One binary frame is several packets laid end to end; each starts with its
// own uint16 total length. A truncated tail is dropped.
function splitBatch(frame) {
    const packets = [];
    let offset = 0;
    while (offset + 2 <= frame.length) {
        const size = frame.readUInt16LE(offset);
        if (size < HEADER_SIZE || offset + size > frame.length) break;
        packets.push(frame.subarray(offset, offset + size));
        offset += size;
    }
    return packets;
}

// Per-exchange price dividers from the auth response, keyed by exchange_id.
function readDividers(exchanges) {
    const dividers = {};
    for (const [name, info] of Object.entries(exchanges || {})) {
        if (!info || typeof info !== 'object') continue;
        const id = info.value !== undefined ? info.value : EXCHANGE_ID_BY_NAME[name.toLowerCase()];
        if (id !== undefined && info.divider > 0) dividers[id] = info.divider;
    }
    return dividers;
}

// Decode one packet into { tk, e, ltp }, or null for anything that is not a
// price packet we fully understand — an unreadable packet must never turn
// into a price.
function decodeTick(packet, dividers) {
    if (packet.length < HEADER_SIZE) return null;
    if (packet.readUInt16LE(2) !== MSG_MARKET_PICTURE) return null;

    const exchangeId = packet.readInt8(4);
    const exchange = EXCHANGE_BY_ID[exchangeId];
    if (!exchange) return null;

    const level = packet.readUInt8(5);
    let ltpOffset;
    if (level === LEVEL_MINI_TOUCH_LINE) {
        if (packet.length < MINI_TOUCH_LINE_SIZE) return null;
        ltpOffset = 21;
    } else if (LEVELS_MARKET_PICTURE.includes(level)) {
        if (packet.length < MARKET_PICTURE_FIXED_SIZE) return null;
        ltpOffset = 69;
    } else {
        return null;
    }

    const raw = packet.readUInt32LE(ltpOffset);
    if (raw === 0) return null;

    const divider = dividers[exchangeId] || DEFAULT_DIVIDER;
    return { tk: String(packet.readUInt32LE(9)), e: exchange, ltp: raw / divider };
}

function describeError(err) {
    if (!err) return "unknown error";
    const inner = Array.isArray(err.errors)
        ? err.errors.map((e) => `${e.code || e.message}${e.address ? '@' + e.address : ''}`).join(', ')
        : '';
    return [err.code, err.message, inner].filter(Boolean).join(' — ') || String(err);
}

module.exports = { splitBatch, readDividers, decodeTick };

if (require.main === module) {
    main();
}

function main() {
    // Handle EPIPE gracefully when the Rust parent drops the stdout pipe
    process.stdout.on('error', function(err) {
        if (err.code === 'EPIPE') {
            process.exit(0);
        }
    });

    let wsClient = null;
    let heartbeatInterval = null;
    let watchdogInterval = null;
    let lastMessageTime = Date.now();
    let dividers = {};

    // Subscriptions are held back until the server's first auth response
    // (1117 / 1119) has been seen, then drained in order.
    let feedReady = false;
    let pendingSubscriptions = [];
    let subscribedTokens = new Set();

    const rl = readline.createInterface({
        input: process.stdin,
        output: process.stdout,
        terminal: false
    });

    rl.on('line', (line) => {
        if (!line.trim()) return;
        try {
            const msg = JSON.parse(line);
            handleMessage(msg);
        } catch (e) {
            console.error("Failed to parse JSON line:", e.message);
        }
    });

    function fail(message) {
        feedReady = false;
        pendingSubscriptions = [];
        console.log(JSON.stringify({ event: "error", message }));
        if (heartbeatInterval) clearInterval(heartbeatInterval);
        if (watchdogInterval) clearInterval(watchdogInterval);
        process.exit(1);
    }

    function sendSubscribe(scrips) {
        const tokens = String(scrips).split(/[,&]/).map((s) => s.trim()).filter(Boolean);
        if (tokens.length === 0) return;
        try {
            wsClient.send(JSON.stringify({
                event: "subscribeScripsLite",
                inputtoken: tokens.join(','),
                ack_symbol: true
            }));
            for (const token of tokens) subscribedTokens.add(token);
        } catch (e) {
            console.log(JSON.stringify({ event: "error", message: `subscribe failed: ${e.message}` }));
        }
    }

    function handleControlFrame(text) {
        let msg;
        try {
            msg = JSON.parse(text);
        } catch (e) {
            return;
        }

        const code = Number(msg.message_code);
        if (code === MSG_AUTH_FAILED) {
            fail(`feed auth failed: ${msg.message || text}`);
        } else if (code === MSG_AUTH_OK || code === MSG_CONNECTED) {
            if (msg.format === "native_fallback") {
                fail("feed downgraded to native_fallback, which this bridge cannot decode");
            }
            if (msg.exchanges) dividers = readDividers(msg.exchanges);
            if (!feedReady) {
                feedReady = true;
                console.log(JSON.stringify({ event: "connected", message_code: code }));
                for (const scrips of pendingSubscriptions) sendSubscribe(scrips);
                pendingSubscriptions = [];
            }
        } else if (code === MSG_SUBSCRIBE_ACK) {
            console.log(JSON.stringify({
                event: "subscribed",
                error_code: msg.error_code,
                description: msg.description,
                scrips: Object.keys(msg.data || {})
            }));
        }
    }

    function handleMessage(msg) {
        if (msg.action === 'connect') {
            // Reset state for this new connection
            feedReady = false;
            pendingSubscriptions = msg.scrips ? [msg.scrips] : [];
            subscribedTokens = new Set();
            dividers = {};
            lastMessageTime = Date.now();

            // The login API hands the feed endpoint back with an https scheme.
            const url = String(msg.url || '').trim().replace(/^http(s?):\/\//i, 'ws$1://');
            if (!url || !msg.user || !msg.auth) {
                fail("connect needs url, user and auth");
            }

            wsClient = new WebSocket(url);

            wsClient.on('open', function () {
                lastMessageTime = Date.now();

                // Authentication is the first application message.
                wsClient.send(JSON.stringify({
                    user: msg.user,
                    auth: msg.auth,
                    format: "native_batch",
                    source: "NEOTRADEAPI",
                    sdk_version: 1.1,
                    sdk_date: "2026-10-12"
                }));

                // The feed has no application-level heartbeat; keep-alive is
                // the WebSocket layer's own ping/pong.
                if (heartbeatInterval) clearInterval(heartbeatInterval);
                heartbeatInterval = setInterval(() => {
                    try { wsClient.ping(); } catch (e) { /* closing — onclose handles it */ }
                }, 20000);

                // Watchdog. Only real feed messages count as life: a pong proves
                // the socket is up, not that prices are flowing, and a stalled
                // feed would leave open positions unmonitored. So when the feed
                // goes quiet (illiquid contracts, after 15:30, a holiday) ask
                // for a snapshot — the server answers that even for a contract
                // that never traded — and restart only if that goes unanswered.
                if (watchdogInterval) clearInterval(watchdogInterval);
                watchdogInterval = setInterval(() => {
                    const quietFor = Date.now() - lastMessageTime;
                    if (quietFor > WATCHDOG_TIMEOUT_MS) {
                        console.error("Watchdog timeout: No data received from Kotak WebSocket for 30s. Exiting bridge to force restart...");
                        fail("Watchdog timeout: no data for 30s");
                    } else if (quietFor > QUIET_PROBE_MS && feedReady && subscribedTokens.size > 0) {
                        try {
                            wsClient.send(JSON.stringify({
                                event: "snapshotScripsLite",
                                inputtoken: [...subscribedTokens].join(',')
                            }));
                        } catch (e) { /* closing — onclose handles it */ }
                    }
                }, 5000);
            });

            wsClient.on('close', function (code, reason) {
                feedReady = false;
                pendingSubscriptions = [];
                console.log(JSON.stringify({ event: "closed", code: code, reason: reason ? reason.toString() : null }));
                if (heartbeatInterval) clearInterval(heartbeatInterval);
                if (watchdogInterval) clearInterval(watchdogInterval);
                process.exit(1);
            });

            wsClient.on('error', function (err) {
                fail(describeError(err));
            });

            wsClient.on('message', function (data, isBinary) {
                lastMessageTime = Date.now();
                if (!isBinary) {
                    handleControlFrame(data.toString());
                    return;
                }
                const ticks = [];
                for (const packet of splitBatch(data)) {
                    const tick = decodeTick(packet, dividers);
                    if (tick) ticks.push(tick);
                }
                if (ticks.length > 0) {
                    console.log(JSON.stringify({ event: "data", data: ticks }));
                }
            });
        } else if (msg.action === 'subscribe') {
            if (wsClient && msg.scrips) {
                if (feedReady) {
                    // Connection is live — send immediately
                    sendSubscribe(msg.scrips);
                } else {
                    // Not authenticated yet — queue for drain on the auth response
                    pendingSubscriptions.push(msg.scrips);
                }
            }
        } else if (msg.action === 'close') {
            if (wsClient) {
                wsClient.close();
            }
            process.exit(0);
        }
    }
}
