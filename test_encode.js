function getScripByteArray(c, a) {
    if (c.charCodeAt[c.length - 1] == "&") {
        c = c.substring(0, c.length - 1)
    }
    let scripArray = c.split("&");
    let scripsCount = scripArray.length;
    let dataLen = 0;
    for (let index = 0; index < scripsCount; index++) {
        scripArray[index] = a + "|" + scripArray[index];
        dataLen += scripArray[index].length + 1
    }
    let bytes = new Uint8Array(dataLen + 2);
    let pos = 0;
    bytes[pos++] = ((scripsCount >> 8) & 255);
    bytes[pos++] = (scripsCount & 255);
    for (let index = 0; index < scripsCount; index++) {
        let currScrip = scripArray[index];
        let scripLen = currScrip.length;
        bytes[pos++] = (scripLen & 255);
        for (let strIndex = 0; strIndex < scripLen; strIndex++) {
            bytes[pos++] = currScrip.charCodeAt(strIndex)
        }
    }
    return bytes
}
let res = getScripByteArray("nse_cm|11536&nse_fo|51386", "sf");
console.log(Array.from(res));
