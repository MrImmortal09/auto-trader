const fs = require('fs');
const session = JSON.parse(fs.readFileSync('../backend/session.json', 'utf8'));
const auth = session.auth_token;
const sid = session.sid;

const { execSync } = require('child_process');
const output = execSync(`echo '{"action":"connect","auth":"${auth}","sid":"${sid}","scrips":"nse_fo|51386"}' | node index.js > /tmp/demo.json & sleep 5 && kill -9 $!`, { shell: '/bin/bash' });
