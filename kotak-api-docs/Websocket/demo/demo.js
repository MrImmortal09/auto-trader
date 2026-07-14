const isValid = (str)=>{
    const error_data = ['null', null, undefined, 'undefined', '', NaN];
    if(!error_data.includes(str))
        return true;
    else
        return false;
}
const windowIntervals = {
    hsmInterval: null,
    hsiInterval: null,
    hsiRetryTimeout: null,
    hsmRetryTimeout: null
}
const reqGBValues = {
    hsm: "hsm",
    hsi: "hsi",
    hsiFCN: "hb",
    IntrValTime: 30000
}
const retrySocket={
    hsmRetry: false,
    hsiRetry: false,
    subscriptions:[],
    hsiRetryCount: 0,
    retryCount:0,
    initiateRetry:function(socketIdentity){
        switch(socketIdentity){
            case reqGBValues.hsm:
                if(this.retryCount<5) {
                    let tokenValue = (isValid($('#token_id').val())?$('#token_id').val():"0");
                    let sidValue = (isValid($('#sid').val())?$('#sid').val():"0");
                    connectHsm(tokenValue, sidValue).then((msg)=>{
                        console.log("hsm connected",msg, this.subscriptions);
                        this.subscriptions.map((req)=>{
                            userWS.send(req);
                        })
                    })
                }
                else{
                    console.log('%c[Socket]: Retry Exhausted',  'background: red; color: white');
                    this.hsmRetry=false;
                    clearInterval(windowIntervals.hsmInterval);
                    let ref = this;
                    windowIntervals.hsmRetryTimeout=setTimeout(function(){
                        clearInterval(windowIntervals.hsmInterval);
                        ref.retryCount=0;
                        ref.hsmRetry=true;
                        retrySocket.initiateRetry('Hsm');
                    },300000);
                }
                this.retryCount++;
                break;
            case reqGBValues.hsi:
                if(this.hsiRetryCount<5) {
                    let tokenValue = $('#token_id').val();
                    let sidValue = $('#sid').val();
                    let datacenter = $('#datacenter_id').val();
                    if(!isValid(tokenValue)||!isValid(sidValue)||!isValid(datacenter)){
                        console.log('%c[Socket]: Invalid Token !',  'background: red; color: white');
                        return;
                    }
                    connectHsi(tokenValue, sidValue, datacenter).then((msg)=>{
                        console.log("hsi connected",msg);
                    }).catch(err=>console.log('%c[Socket]: Failed to connect ! '+err,  'background: red; color: white'))
                }
                else{
                    console.log('%c[Socket]: Retry Exhausted',  'background: red; color: white');
                    this.hsiRetry=false;
                    clearInterval(windowIntervals.hsiInterval);
                    let ref = this;
                    windowIntervals.hsiRetryTimeout=setTimeout(function(){
                        clearInterval(windowIntervals.hsiInterval);
                        ref.hsiRetryCount=0;
                        ref.hsiRetry=true;
                        retrySocket.initiateRetry(reqGBValues.hsi);
                    },300000);
                }
                this.hsiRetryCount++;
                break;
            default:
                console.log('%c[Socket]: Invalid Request !',  'background: red; color: white');
        }
    }
}
function connectHsm(token,sid)
{
    return new Promise((resolve, reject)=>{
        let url = "wss://mlhsm.kotaksecurities.com"; 
        // <!--wss://qhsm.kotaksecurities.online/is for UAT with VPN,wss://mlhsm.kotaksecurities.com/ for prod   -->
        userWS = new HSWebSocket(url);
        // console.log(document.getElementById('channel_number').value)
        retrySocket.hsmRetry=true;
    
        userWS.onopen = function () {
            consoleLog('[Socket]: Connected to "' + url + '"\n');
            let jObj = {};
            jObj["Authorization"] = token;
            jObj["Sid"] = sid; 
            jObj["type"] = "cn";
            userWS.send(JSON.stringify(jObj));
            clearInterval(windowIntervals.hsmInterval);
            windowIntervals.hsmInterval = setInterval(function(){
                userWS.send(JSON.stringify({type:"ti","scrips":""}))
            },30000);
            resolve("connected");
        }
    
        userWS.onclose = function () {
            consoleLog("[Socket]: Disconnected !\n");
            if(retrySocket.hsmRetry)
                retrySocket.initiateRetry('Hsm')
            reject("closed");
        }
    
        userWS.onerror = function () {
            consoleLog("[Socket]: Error !\n");
            // clearInterval(windowIntervals.hsmInterval)
            reject("error");
        }
    
        userWS.onmessage = function (msg) {
            const result= JSON.parse(msg);
            consoleLog('[Res]: ' + msg + "\n");
        }
    })
}
resumeandpause = function(typeRequest,channel_number) {
    let jObj = {};
    jObj["type"] = typeRequest;
    jObj["channelnums"] = channel_number.split(',').map(function (val) { return parseInt(val, 10); })
    if (userWS != null) {
        let req = JSON.stringify(jObj);
        userWS.send(req);
        if(typeRequest == "cr")
            retrySocket.hsmRetry=true;
    }
    if(typeRequest == "cp"){
        clearTimeout(windowIntervals.hsmRetryTimeout);
        retrySocket.hsmRetry=false;
    }
}


function subscribe_scrip(typeRequest,scrips,channel_number)
{
	//  mws ifs dps	
    let jObj = {"type":typeRequest, "scrips":scrips, "channelnum":channel_number};
    userWS.send(JSON.stringify(jObj));
    retrySocket.subscriptions.push(JSON.stringify(jObj));
}

function connectHsi(token,sid, datacenter)
{
    return new Promise((resolve, reject)=>{
        let url = "wss://mis.kotaksecurities.com/realtime";
        if(datacenter == "adc")
            url = "wss://cis.kotaksecurities.com/realtime";
        else if(datacenter == "e21")
            url = "wss://e21.kotaksecurities.com/realtime";
        else if(datacenter == "e22")
            url = "wss://e22.kotaksecurities.com/realtime";
        else if(datacenter == "e41")
            url = "wss://e41.kotaksecurities.com/realtime";
        else if(datacenter == "e43")
            url = "wss://e43.kotaksecurities.com/realtime";

        hsWs = new HSIWebSocket(url);
        hsWs.onopen = function () {
            consoleLog1('[Socket]: Connected to "' + url + '"\n');
            let hsijObj = {};
            hsijObj["type"] = "cn";
            hsijObj["Authorization"] = token;
            hsijObj["Sid"] = sid;
            hsijObj["source"] = "WEB";
            hsWs.send(JSON.stringify(hsijObj));
            clearInterval(windowIntervals.hsiInterval);
            retrySocket.hsiRetry=true;
            windowIntervals.hsiInterval = setInterval(function(){
                hsiWs.send(JSON.stringify({type: reqGBValues.hsiFCN}))
            },reqGBValues.IntrValTime);
            resolve("connected");
        }
        hsWs.onclose = function () {
            consoleLog1("[Socket]: Disconnected !\n");
            if(retrySocket.hsiRetry)
                retrySocket.initiateRetry(reqGBValues.hsi);
            reject("closed");
        }
        hsWs.onerror = function () {
            consoleLog1("[Socket]: Error !\n");
            reject("error");
        }

        hsWs.onmessage = function (msg) {
            consoleLog1('[Res]: ' + msg + "\n");
        }
    });
}