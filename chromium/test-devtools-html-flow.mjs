// End-to-end test: the JS-identical debugging flow for Mersey.
//
// Breakpoints set in the page's OWN HTML file (todo-native.html, real gutter,
// real document line numbers) and stepping via the STANDARD Sources debugger
// buttons — the same toggle-pause/step-over/step-into/step-out actions and
// F8/F10/F11 shortcuts JS uses. The fork's SourcesPanel delegates route those
// actions to the Mersey engine only while __merseyDebug.lastPause is set
// (V8 is not paused then, so JS debugging loses nothing).
//
// Precondition the test exercises: the page must have been (re)loaded with
// DevTools open, else Chromium cannot serve the file:// document's content
// in Sources ("Resource was not cached") and the gutter has no lines — the
// virtual <page>.mersey file covers that case.
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
const pg=client(await connectWS(targets.find(t=>t.url.includes("todo-native")).webSocketDebuggerUrl));
const ev=async(expr,ms)=>{const r=await dt("Runtime.evaluate",{expression:expr,returnByValue:true,awaitPromise:true},ms);
 return r.exceptionDetails?("EXC: "+(r.exceptionDetails.exception?.description||r.exceptionDetails.text||"").split("\n")[0]):r.result.value;};
await dt("Runtime.evaluate",{expression:`window.__w=function*(r){for(const el of r.querySelectorAll("*")){yield el;if(el.shadowRoot)yield* __w(el.shadowRoot);}};
window.__find=p=>{const o=[];for(const el of __w(document)){try{if(p(el))o.push(el)}catch(e){}}return o;};
window.__click=el=>{for(const t of ["pointerdown","mousedown","pointerup","mouseup","click"])el.dispatchEvent(new (t.startsWith("pointer")?PointerEvent:MouseEvent)(t,{bubbles:true,composed:true,cancelable:true}));};`});

// Reload FIRST so DevTools tracks the document resource and can serve it.
await pg("Page.enable"); await pg("Page.reload");
await new Promise(r=>setTimeout(r,4500));

console.log("1 Sources        =>", await ev(`(()=>{const t=__find(el=>el.classList&&el.classList.contains("tabbed-pane-header-tab")&&el.textContent.trim()==="Sources");
 if(!t.length)return "NO TAB";__click(t[0]);return "ok";})()`));
await new Promise(r=>setTimeout(r,1500));
console.log("2 open html file =>", await ev(`(()=>{const n=__find(el=>el.getAttribute&&el.getAttribute("role")==="treeitem"&&el.textContent.trim()==="todo-native.html");
 if(!n.length)return "NO NODE";__click(n[0]);n[0].dispatchEvent(new MouseEvent("dblclick",{bubbles:true,composed:true}));return "opened";})()`));
await new Promise(r=>setTimeout(r,2000));
console.log("3 html content   =>", await ev(`(()=>{const c=__find(el=>el.classList&&el.classList.contains("cm-content")&&el.textContent.includes("addItem"));
 return c.length?"HTML SOURCE SERVED":"still not cached";})()`));
await ev(`(()=>{for(const sc of __find(el=>el.classList&&el.classList.contains("cm-scroller")))sc.scrollTop=650;return 0;})()`);
await new Promise(r=>setTimeout(r,900));
console.log("4 gutter 45      =>", await ev(`(()=>{const g=__find(el=>el.classList&&el.classList.contains("cm-gutterElement")&&el.textContent.trim()==="45");
 if(!g.length)return "NO GUTTER 45";__click(g[0]);return "clicked";})()`));
await new Promise(r=>setTimeout(r,1500));
console.log("5 pushed         =>", await ev(`JSON.stringify(__merseyDebug.lastLines)`), "(expect [19])");
pg("Runtime.evaluate",{expression:`document.getElementById("new-todo").value="html flow";document.getElementById("add").click();`},60000).catch(()=>{});
await new Promise(r=>setTimeout(r,3000));
console.log("6 paused         =>", await ev(`JSON.stringify({p:__merseyDebug.pauses,paused:!!__merseyDebug.lastPause})`));
console.log("7 revealed in    =>", await ev(`(()=>{for(const el of __w(document)){if(el.classList&&el.classList.contains("cm-activeLine")&&el.textContent.includes("if (text.length"))return "todo-native.html line 45";}return "(active line not detected)";})()`));
console.log("8 STD step-over  =>", await ev(`(()=>{const b=__find(el=>el.getAttribute&&(el.getAttribute("aria-label")||"").startsWith("Step over"));
 if(!b.length)return "NO BUTTON";__click(b[0]);return "clicked standard button";})()`));
await new Promise(r=>setTimeout(r,1800));
console.log("9 stepped        =>", await ev(`JSON.stringify({p:__merseyDebug.pauses,reason:__merseyDebug.lastPause&&__merseyDebug.lastPause.reason})`), "(reason step = engine stepped)");
console.log("A STD resume F8  =>", await ev(`(()=>{const b=__find(el=>el.getAttribute&&/script execution/.test(el.getAttribute("aria-label")||""));
 if(!b.length)return "NO BUTTON";__click(b[0]);return "clicked standard button";})()`));
await new Promise(r=>setTimeout(r,1500));
console.log("B resumed        =>", await ev(`JSON.stringify({paused:!!__merseyDebug.lastPause})`));
process.exit(0);
