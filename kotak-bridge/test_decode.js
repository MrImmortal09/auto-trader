// Run with: npm test
const test = require('node:test');
const assert = require('node:assert');
const { splitBatch, readDividers, decodeTick } = require('./index.js');

// The worked example from Kotak's guide (§10): a 54-byte Mini Touch Line
// packet for nse_cm token 11536, last_traded_price 123456 with divider 100.
const MINI_HEX =
    '3600281c0101000700102d00008026b4' +
    '680000000040e2010032000000000000' +
    '0090dc010077000000b0050000010000' +
    '000201000000';
const mini = () => Buffer.from(MINI_HEX, 'hex');
const DIVIDERS = { 1: 100, 6: 100 };

// A full Market Picture packet (level 4) with only the fields we read set.
function marketPicture({ exchangeId, token, rawLtp, level = 4 }) {
    const p = Buffer.alloc(144);
    p.writeUInt16LE(144, 0);
    p.writeUInt16LE(7208, 2);
    p.writeInt8(exchangeId, 4);
    p.writeUInt8(level, 5);
    p.writeUInt32LE(token, 9);
    p.writeUInt32LE(rawLtp, 69);
    return p;
}

test('decodes the guide example', () => {
    assert.deepStrictEqual(decodeTick(mini(), DIVIDERS), { tk: '11536', e: 'nse_cm', ltp: 1234.56 });
});

test('decodes a full market picture', () => {
    const p = marketPicture({ exchangeId: 6, token: 1116970, rawLtp: 50095 });
    assert.deepStrictEqual(decodeTick(p, DIVIDERS), { tk: '1116970', e: 'bse_fo', ltp: 500.95 });
});

test('falls back to divider 100 when the exchange has none', () => {
    assert.strictEqual(decodeTick(mini(), {}).ltp, 1234.56);
});

test('uses the divider from the auth response', () => {
    const dividers = readDividers({ nse_cm: { value: 1, divider: 10000 }, bse_fo: { divider: 100 } });
    assert.deepStrictEqual(dividers, { 1: 10000, 6: 100 });
    assert.strictEqual(decodeTick(mini(), dividers).ltp, 12.3456);
});

test('matches exchange names whatever their case', () => {
    assert.deepStrictEqual(readDividers({ NSE_FO: { divider: 100 }, Bse_Fo: { divider: 100 } }), { 2: 100, 6: 100 });
});

test('ignores a zero or missing divider', () => {
    assert.deepStrictEqual(readDividers({ nse_cm: { value: 1, divider: 0 }, nse_fo: {} }), {});
});

test('rejects anything that is not a known price packet', () => {
    const index = mini(); index.writeUInt16LE(7207, 2);
    const unknownExchange = mini(); unknownExchange.writeInt8(42, 4);
    const unknownLevel = mini(); unknownLevel.writeUInt8(3, 5);
    const zeroPrice = mini(); zeroPrice.writeUInt32LE(0, 21);
    const shortFull = mini(); shortFull.writeUInt8(4, 5);
    for (const p of [index, unknownExchange, unknownLevel, zeroPrice, shortFull, mini().subarray(0, 40)]) {
        assert.strictEqual(decodeTick(p, DIVIDERS), null);
    }
});

test('splits a batch and drops a truncated tail', () => {
    const full = marketPicture({ exchangeId: 6, token: 7, rawLtp: 100 });
    const frame = Buffer.concat([mini(), full, mini().subarray(0, 30)]);
    const packets = splitBatch(frame);
    assert.deepStrictEqual(packets.map((p) => p.length), [54, 144]);
    assert.strictEqual(decodeTick(packets[1], DIVIDERS).tk, '7');
});

test('stops on a corrupt length instead of looping', () => {
    assert.deepStrictEqual(splitBatch(Buffer.from([0, 0, 1, 2, 3])), []);
});
