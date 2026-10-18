// Isolated test responder: simulate a second authority without receiving secrets.
import dgram from 'node:dgram';
import { writeFileSync } from 'node:fs';
const socket=dgram.createSocket('udp4');
socket.on('message',(data,peer)=>{
  if(data.length>1024)return;
  try {
    const query=JSON.parse(data.toString());
    if(query.protocol==='stationdeck-config-v1'&&query.authority==='stationdeck-dev'&&query.port===0) {
      socket.send(Buffer.from(JSON.stringify({...query,port:50070})),peer.port,peer.address);
    }
  } catch {}
});
socket.bind(50071,'0.0.0.0',()=>writeFileSync('/tmp/discovery-ready','ready'));
