const fs = require('fs');
const WebSocket = require('ws');
const pako = require('pako');

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

// Load the library
const hslibCode = fs.readFileSync('../kotak-api-docs/Websocket/hslib.js', 'utf8');
eval(hslibCode);

console.log("HSWebSocket defined?", typeof HSWebSocket);
