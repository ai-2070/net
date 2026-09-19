// Execute the exact page's connect verdict, with only browser I/O mocked.
// This is a JS oracle unit probe, NOT a Chromium/Noise execution.
const fs=require('node:fs');const vm=require('node:vm');const assert=require('node:assert/strict');
const root='C:/Users/chief/orca/workspaces/net/kyra-webrtc-4b-review-871e0138d';
const source=fs.readFileSync(root+'/net/crates/net/tests/rtc_browser/page/app.js','utf8');
assert(source.includes("import init, { LeafEndpoint } from './leaf.js';"));
const executable=source.replace("import init, { LeafEndpoint } from './leaf.js';",'').split('\nmain().catch')[0]+'\nglobalThis.reviewExecute=execute;';
let noiseConstructed=0,fetches=0;
class PeerConnection {
 createDataChannel(){return {readyState:'connecting'};}
 async createOffer(){return {type:'offer',sdp:'v=0'};}
 async setLocalDescription(value){this.localDescription=value;}
}
const context={document:{getElementById:()=>({textContent:''})},console,performance,setTimeout,clearTimeout,RTCPeerConnection:PeerConnection,
 LeafEndpoint:class{constructor(){noiseConstructed++;throw Error('Noise should never be reached');}},
 fetch:async()=>{fetches++;throw Error('reviewer-injected HTTP failure before offer response');}};
vm.createContext(context);vm.runInContext(executable,context,{filename:'exact-head-app.js'});
(async()=>{
 const result=await context.reviewExecute({kind:'connect',expect_failure:true,base:'https://example.invalid',session:'mitm',node_id:'7',credential:'synthetic-placeholder'});
 const record={kind:'exact-page-oracle-unit-probe',result,fetches,noiseConstructed,falseGreen:result.ok===true&&noiseConstructed===0};
 console.log(JSON.stringify(record,null,2));
 assert.equal(fetches,1);assert.equal(noiseConstructed,0);
 assert.equal(result.ok,false,'MITM verdict must not pass on an HTTP failure before Noise');
})().catch(e=>{console.error(e.message);process.exitCode=1;});
