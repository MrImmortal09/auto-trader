function fetchLogin(){
	
	var userId = $('#userId').val();
	var userPassword = $('#userPassword').val();

	var settings = {
  "url": "https://qapi.kotaksecurities.com/login/validate",
  "method": "POST",
  "timeout": 0,
  "headers": {
    "accept": "application/json",
    "Content-Type": "application/json"
  },
	  "data": JSON.stringify({"userId":userId,"password":userPassword}),
	};
	$.ajax(settings).done(function (response) {
	  console.log(response);
	});
	/*
	data = response.json();
	
	$('#token_id').val(data.data.token);
	$('#sid').val(data.data.sid);
	*/
	
}

function wconnect(typeFunction){
	retrySocket.retryCount=0;
	retrySocket.subscriptions=[];
	var token = $('#token_id').val();
	var sid = $('#sid').val();
	// var handshakeServerId = $('#serverid').val();
	var datacenter = $('#datacenter_id').val();
	
	if(typeFunction=='Hsi'){
		connectHsi(token,sid, datacenter);	
		
	}else if(typeFunction=='Hsm'){
		connectHsm(token,sid);	
	}
		
	return;

}

function awaited(testDisplay){
	alert(testDisplay);	
}

function wsub(typeRequest,scrips,channel_number){
	channel_number = $('#channel_number').val();
	scrips = $('#'+scrips).val();
	subscribe_scrip(typeRequest,scrips,channel_number);	
}

function consoleLog(printLogs){
        const d = new Date();
        $('#stream_scrips').append(d + "\n");
        $('#stream_scrips').append(printLogs);
        $('#stream_scrips').append("\n" +"\n"); 
	var psconsole = $('#stream_scrips');
    if(psconsole.length)
       psconsole.scrollTop(psconsole[0].scrollHeight - psconsole.height());
   
}

function consoleLog1(printLogs){
	  const d = new Date();
        $('#stream_scrips1').append(d + "\n");
        $('#stream_scrips1').append(printLogs);
        $('#stream_scrips1').append("\n" +"\n"); 
	var psconsole = $('#stream_scrips');
	var psconsole = $('#stream_scrips1');
    if(psconsole.length)
       psconsole.scrollTop(psconsole[0].scrollHeight - psconsole.height());
   
}