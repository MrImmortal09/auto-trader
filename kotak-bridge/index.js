const fs = require('fs');
const WebSocket = require('ws');
const pako = require('pako');
const readline = require('readline');

// Mock browser globals required by hslib.js
global.window = global;
global.WebSocket = WebSocket;
global.pako = pako;
global.btoa = (str) => Buffer.from(str, 'binary').toString('base64');
global.atob = (b64) => Buffer.from(b64, 'base64').toString('binary');
global.document = {
    getElementsByTagName: () => [{ appendChild: () => {} }],
    createElement: () => ({})
};

// Disable internal logs of hslib to prevent polluting stdout
global.HSD_Flag = false;
global.HSID_Flag = false;

// Load the library
const hslibCode = fs.readFileSync('../kotak-api-docs/Websocket/hslib.js', 'utf8');
eval(hslibCode);

let wsClient = null;
let heartbeatInterval = null;

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

function handleMessage(msg) {
    if (msg.action === 'connect') {
        const url = "wss://mlhsm.kotaksecurities.com";
        wsClient = new HSWebSocket(url);
        
        wsClient.onopen = function () {
            // Send connection request
            let jObj = {
                "Authorization": msg.auth,
                "Sid": msg.sid,
                "type": "cn"
            };
            wsClient.send(JSON.stringify(jObj));
            
            // Start heartbeat
            if (heartbeatInterval) clearInterval(heartbeatInterval);
            heartbeatInterval = setInterval(() => {
                wsClient.send(JSON.stringify({ type: "ti", scrips: "" }));
            }, 30000);

            // Initially subscribe if scrips are provided
            if (msg.scrips) {
                let subObj = {
                    "type": "mws",
                    "scrips": msg.scrips,
                    "channelnum": 1
                };
                wsClient.send(JSON.stringify(subObj));
            }
        };

        wsClient.onclose = function () {
            console.log(JSON.stringify({ event: "closed" }));
            if (heartbeatInterval) clearInterval(heartbeatInterval);
        };

        wsClient.onerror = function () {
            console.log(JSON.stringify({ event: "error" }));
        };

        wsClient.onmessage = function (data) {
            let parsed;
            if (typeof data === 'string') {
                try {
                    parsed = JSON.parse(data);
                } catch (e) {
                    parsed = data;
                }
            } else {
                parsed = data;
            }
            console.log(JSON.stringify({ event: "data", data: parsed }));
        };
    } else if (msg.action === 'subscribe') {
        if (wsClient) {
            let subObj = {
                "type": "mws",
                "scrips": msg.scrips,
                "channelnum": 1
            };
            try {
                wsClient.send(JSON.stringify(subObj));
            } catch (e) {
                console.log(JSON.stringify({ event: "error", message: `subscribe failed: ${e.message}` }));
            }
        }
    } else if (msg.action === 'close') {
        if (wsClient) {
            wsClient.close();
        }
        process.exit(0);
    }
}
