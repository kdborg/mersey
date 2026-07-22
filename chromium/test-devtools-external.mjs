// End-to-end test: debugging an EXTERNAL Mersey module in the Sources panel.
//
// todo.html loads demo/todo.mersey via <script type="text/mersey" src=...>;
// the fork's native module-graph loader fetches and runs it. The contract:
// every fetched module is recorded (url + engine spec + source) and appears
// in the Sources tree under its REAL URL with editor lines = engine lines
// (startLine 0, no offset), takes gutter breakpoints, pauses on a real page
// click, steps with the STANDARD debugger buttons, and the breakpoint
// survives reload (bridge refresh retries cover the async graph load).
//
// Serve web/ first:  python3 -m http.server 8123  (file:// works too, but
// http exercises the real fetch path).
import net from "node:net"; import crypto from "node:crypto";
process.on("unhandledRejection", e => { console.error("FATAL:", e); process.exit(1); });
function connectWS(u0){const u=new URL(u0);return new Promise((res,rej)=>{const key=crypto.randomBytes(16).toString("base64");
const sock=net.connect(Number(u.port),u.hostname,()=>{sock.write(`GET ${u.pathname}${u.search} HTTP/1.1\r\nHost: ${u.host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`);});
let buf=Buffer.alloc(0),open=false;const hs=[];
sock.on("data",d=>{buf=Buffer.concat([buf,d]);
if(!open){const e=buf.indexOf("\r\n\r\n");if(e<0)return;buf=buf.subarray(e+4);open=true;res({send,onMessage:h=>hs.push(h)});}
for(;;){if(buf.length<2)return;const l0=buf[1]&0x7f;let off=2,len=l0;
if(l0===126){if(buf.length<4)return;len=buf.readUInt16BE(2);off=4;}else if(l0===127){if(buf.length<10)return;len=Number(buf.readBigUInt64BE(2));off=10;}
if(buf.length<off+len)return;const p=buf.subarray(off,off+len).toString("utf8");buf=buf.subarray(off+len);for(const h of hs)h(p);}});
sock.on("error",rej);
function send(t){const b=Buffer.from(t,"utf8"),m=crypto.randomBytes(4),h=[0x81];
if(b.length<126)h.push(0x80|b.length);else h.push(0x80|126,b.length>>8,b.length&0xff);
const k=Buffer.from(b);for(let i=0;i<k.length;i++)k[i]^=m[i&3];sock.write(Buffer.concat([Buffer.from(h),m,k]));}});}
function client(ws){let id=1;const pend=new Map();
ws.onMessage(t=>{const m=JSON.parse(t);if(m.id&&pend.has(m.id)){pend.get(m.id)(m);pend.delete(m.id);}});
return (method,params={},ms=25000)=>new Promise((res,rej)=>{const i=id++;
 pend.set(i,m=>m.error?rej(new Error(method+": "+m.error.message)):res(m.result));
 ws.send(JSON.stringify({id:i,method,params}));setTimeout(()=>{if(pend.has(i)){pend.delete(i);rej(new Error("timeout "+method));}},ms);});}
const targets=await(await fetch("http://127.0.0.1:9222/json/list")).json();
const dt=client(await connectWS(targets.find(t=>t.url.startsWith("devtools://")).webSocketDebuggerUrl));
const pg=client(await connectWS(targets.find(t=>t.url.includes("todo.html")).webSocketDebuggerUrl));
const ev=async(expr,ms)=>{const r=await dt("Runtime.evaluate",{expression:expr,returnByValue:true,awaitPromise:true},ms);
 return r.exceptionDetails?("EXC: "+(r.exceptionDetails.exception?.description||r.exceptionDetails.text||"").split("\n")[0]):r.result.value;};
await dt("Runtime.evaluate",{expression:`window.__w=function*(r){for(const el of r.querySelectorAll("*")){yield el;if(el.shadowRoot)yield* __w(el.shadowRoot);}};
window.__find=p=>{const o=[];for(const el of __w(document)){try{if(p(el))o.push(el)}catch(e){}}return o;};
window.__click=el=>{for(const t of ["pointerdown","mousedown","pointerup","mouseup","click"])el.dispatchEvent(new (t.startsWith("pointer")?PointerEvent:MouseEvent)(t,{bubbles:true,composed:true,cancelable:true}));};`});

console.log("1 scripts        =>", await ev(`JSON.stringify((window.__merseyDebug&&__merseyDebug.scripts)||0)`), "(external module recorded?)");
console.log("2 Sources        =>", await ev(`(()=>{const t=__find(el=>el.classList&&el.classList.contains("tabbed-pane-header-tab")&&el.textContent.trim()==="Sources");
 if(!t.length)return "NO TAB";__click(t[0]);return "ok";})()`));
await new Promise(r=>setTimeout(r,1500));
console.log("3a expand demo   =>", await ev(`(()=>{const f=__find(el=>el.getAttribute&&el.getAttribute("role")==="treeitem"&&el.textContent.trim()==="demo");
 if(!f.length)return "NO FOLDER";__click(f[0]);f[0].dispatchEvent(new MouseEvent("dblclick",{bubbles:true,composed:true}));return "expanded";})()`));
await new Promise(r=>setTimeout(r,1200));
console.log("3b tree item     =>", await ev(`(()=>{const n=__find(el=>el.getAttribute&&el.getAttribute("role")==="treeitem"&&el.textContent.trim()==="todo.mersey");
 if(!n.length)return "NO NODE";__click(n[0]);n[0].dispatchEvent(new MouseEvent("dblclick",{bubbles:true,composed:true}));return "opened todo.mersey";})()`));
await new Promise(r=>setTimeout(r,2000));
console.log("4 content        =>", await ev(`(()=>{const c=__find(el=>el.classList&&el.classList.contains("cm-content")&&el.textContent.includes("addItem"));
 return c.length?"module source in editor":"NO CONTENT";})()`));
console.log("5 gutter 19      =>", await ev(`(()=>{const g=__find(el=>el.classList&&el.classList.contains("cm-gutterElement")&&el.textContent.trim()==="19");
 if(!g.length)return "NO GUTTER";__click(g[0]);return "clicked";})()`));
await new Promise(r=>setTimeout(r,1500));
console.log("6 pushed         =>", await ev(`JSON.stringify(__merseyDebug.lastLines)`), "(expect [18])");
pg("Runtime.evaluate",{expression:`document.getElementById("new-todo").value="external";document.getElementById("add").click();`},60000).catch(()=>{});
await new Promise(r=>setTimeout(r,3000));
console.log("7 paused         =>", await ev(`JSON.stringify({p:__merseyDebug.pauses,frameUrl:__merseyDebug.lastPause&&__merseyDebug.lastPause.callFrames[0].url})`));
console.log("8 STD step-over  =>", await ev(`(()=>{const b=__find(el=>el.getAttribute&&(el.getAttribute("aria-label")||"").startsWith("Step over"));
 if(!b.length)return "NO BUTTON";__click(b[0]);return "clicked";})()`));
await new Promise(r=>setTimeout(r,1800));
console.log("9 stepped        =>", await ev(`JSON.stringify({p:__merseyDebug.pauses,reason:__merseyDebug.lastPause&&__merseyDebug.lastPause.reason})`));
console.log("A STD resume     =>", await ev(`(()=>{const b=__find(el=>el.getAttribute&&/script execution/.test(el.getAttribute("aria-label")||""));
 if(!b.length)return "NO BUTTON";__click(b[0]);return "clicked";})()`));
await new Promise(r=>setTimeout(r,1500));
await pg("Page.enable");await pg("Page.reload");
await new Promise(r=>setTimeout(r,7000));
pg("Runtime.evaluate",{expression:`document.getElementById("new-todo").value="again";document.getElementById("add").click();`},60000).catch(()=>{});
await new Promise(r=>setTimeout(r,2500));
console.log("B after reload   =>", await ev(`JSON.stringify({p:__merseyDebug.pauses})`), "(3 = persisted across reload)");
console.log("C resume         =>", await ev(`__merseyDebug.resume().then(r=>r.getError()||"ok")`));
process.exit(0);
