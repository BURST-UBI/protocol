// Isolated Linux IGD emulator: SOAP + actual DNAT/forwarding + finite leases.
// Never run on a host router. All changes are inside the lab router namespace.
import dgram from 'node:dgram';
import http from 'node:http';
import {execFileSync} from 'node:child_process';
import {existsSync} from 'node:fs';
if (!existsSync('/.dockerenv') || process.env.BURST_ISOLATED_NAT_LAB !== 'yes') throw Error('isolated lab only');
const lan = '10.99.0.1';
const service = 'urn:schemas-upnp-org:service:WANIPConnection:1';
const actions = {
  AddPortMapping: ['NewRemoteHost','NewExternalPort','NewProtocol','NewInternalPort','NewInternalClient','NewEnabled','NewPortMappingDescription','NewLeaseDuration'],
  DeletePortMapping: ['NewRemoteHost','NewExternalPort','NewProtocol'],
};
const mappings = new Map();
let externalIp = '8.8.4.4', reject = false, conflict = false;
const soap = body => `<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>${body}</s:Body></s:Envelope>`;
const rule = (op, port, m) => {
  execFileSync('iptables', ['-t','nat',op,'PREROUTING','-i','outside','-p','tcp','--dport',String(port),'-j','DNAT','--to-destination',`${m.client}:${m.internal}`]);
  execFileSync('iptables', [op,'FORWARD','-i','outside','-o','lan','-p','tcp','-d',m.client,'--dport',String(m.internal),'-j','ACCEPT']);
};
const remove = port => {
  const m=mappings.get(port); if(!m)return;
  clearTimeout(m.timer); rule('-D',port,m); mappings.delete(port);
};
http.createServer((req,res)=>{
  res.setHeader('Content-Type','text/xml');
  if(req.url==='/test/cgnat') { externalIp='100.64.0.1'; return res.end('CGNAT'); }
  if(req.url==='/test/public') { externalIp='8.8.4.4'; reject=false; conflict=false; return res.end('public'); }
  if(req.url==='/test/reject') { reject=true; return res.end('reject'); }
  if(req.url==='/test/conflict') { conflict=true; return res.end('7076 occupied'); }
  if(req.url==='/test/steal') {
    remove(7076);
    const m={client:'10.99.0.3',internal:7076}; rule('-A',7076,m);
    m.timer=setTimeout(()=>remove(7076),30000); mappings.set(7076,m);
    return res.end('simulated lease reassigned after router restart');
  }
  if(req.url==='/test/release-other') { remove(7076); return res.end('released test mapping'); }
  if(req.url==='/test/state') { res.setHeader('Content-Type','application/json'); return res.end(JSON.stringify([...mappings].map(([port,m])=>({port,client:m.client,internal:m.internal})))); }
  if(req.url==='/root.xml') return res.end(`<root><device><serviceList><service><serviceType>${service}</serviceType><SCPDURL>/schema.xml</SCPDURL><controlURL>/control</controlURL></service></serviceList></device></root>`);
  if(req.url==='/schema.xml') return res.end(`<scpd><actionList>${Object.entries(actions).map(([name,args])=>`<action><name>${name}</name><argumentList>${args.map(name=>`<argument><name>${name}</name><direction>in</direction><relatedStateVariable>test</relatedStateVariable></argument>`).join('')}</argumentList></action>`).join('')}</actionList></scpd>`);
  let body=''; req.on('data',chunk=>body+=chunk); req.on('end',()=>{
    const action=String(req.headers.soapaction ?? '').split('#')[1]?.replaceAll('"','');
    const field=name=>body.match(new RegExp(`<${name}>(.*?)</${name}>`))?.[1];
    const fault=(code,description)=>{res.statusCode=500; res.end(soap(`<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>${code}</errorCode><errorDescription>${description}</errorDescription></UPnPError></detail></s:Fault>`));};
    if(action==='GetExternalIPAddress') return res.end(soap(`<u:GetExternalIPAddressResponse xmlns:u="${service}"><NewExternalIPAddress>${externalIp}</NewExternalIPAddress></u:GetExternalIPAddressResponse>`));
    const port=Number(field('NewExternalPort'));
    if(action==='AddPortMapping') {
      const client=field('NewInternalClient'), internal=Number(field('NewInternalPort')), lease=Number(field('NewLeaseDuration'));
      console.log(JSON.stringify({action,client,port,internal,lease}));
      if(!['10.99.0.2','10.99.0.3'].includes(client) || internal!==7076 || !Number.isInteger(port) || port<1 || port>65535 || !Number.isInteger(lease) || lease<1 || lease>3600 || field('NewProtocol')!=='TCP') return fault(402,'Invalid target');
      if(reject) return fault(501,'Test mapping failure');
      if((conflict && port===7076) || (mappings.has(port) && mappings.get(port).client!==client)) return fault(718,'ConflictInMappingEntry');
      remove(port);
      const m={client,internal}; rule('-A',port,m);
      m.timer=setTimeout(()=>{remove(port);console.log(JSON.stringify({action:'LeaseExpired',port}));},lease*1000);
      mappings.set(port,m);
      return res.end(soap(`<u:AddPortMappingResponse xmlns:u="${service}"/>`));
    }
    if(action==='DeletePortMapping') {
      console.log(JSON.stringify({action,previousClient:mappings.get(port)?.client,port})); remove(port);
      return res.end(soap(`<u:DeletePortMappingResponse xmlns:u="${service}"/>`));
    }
    res.statusCode=400; res.end('Unsupported action');
  });
}).listen(1901,lan);
const ssdp=dgram.createSocket({type:'udp4',reuseAddr:true});
ssdp.on('message',(msg,remote)=>{
  if(!msg.toString().includes('M-SEARCH'))return;
  ssdp.send(`HTTP/1.1 200 OK\r\nLOCATION: http://${lan}:1901/root.xml\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n`,remote.port,remote.address);
});
ssdp.bind(1900,'0.0.0.0',()=>{ssdp.addMembership('239.255.255.250',lan);console.log('IGD emulator ready');});
process.on('SIGTERM',()=>{for(const port of mappings.keys())remove(port);process.exit(0);});
