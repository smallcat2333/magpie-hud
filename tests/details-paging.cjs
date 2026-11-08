'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const flush=async()=>{for(let i=0;i<5;i++)await new Promise(resolve=>setImmediate(resolve));};
function panel(){
  class Element{
    constructor(tag='div'){this.tagName=tag.toLowerCase();this.children=[];this.handlers={};this.attrs={};this.dataset={};this.style={};this.value='';this.hidden=false;this.disabled=false;this.className='';this.clientWidth=900;this.clientHeight=250;this._text='';this.classList={add:(...v)=>{this.className+=' '+v.join(' ');},toggle:()=>{}};}
    append(...items){for(let child of items){if(typeof child==='string'){const text=new Element('#text');text.textContent=child;child=text;}child.parent=this;this.children.push(child);if(this.tagName==='select'&&!this.value)this.value=child.value;}}
    prepend(...items){this.children.unshift(...items);}
    replaceChildren(...items){this.children=[];this._text='';this.append(...items);}
    set textContent(value){this._text=String(value);this.children=[];}
    get textContent(){return this._text+this.children.map(c=>c.textContent).join('');}
    get childElementCount(){return this.children.length;}
    get options(){return this.children;}
    get selectedOptions(){return [this.children.find(c=>c.value===this.value)||{textContent:'模型'}];}
    setAttribute(key,value){this.attrs[key]=value;}
    addEventListener(key,fn){this.handlers[key]=fn;}
    matches(selector){if(selector==='button')return this.tagName==='button';if(selector.startsWith('.'))return this.className.split(' ').includes(selector.slice(1));const m=selector.match(/^\[data-([\w-]+)\]$/);if(m)return this.dataset[m[1].replace(/-([a-z])/g,(_,c)=>c.toUpperCase())]!==undefined;return false;}
    querySelectorAll(selector){return this.children.flatMap(c=>[...(c.matches(selector)?[c]:[]),...c.querySelectorAll(selector)]);}
    querySelector(selector){return this.querySelectorAll(selector)[0]||null;}
    contains(other){return other===this||this.children.some(c=>c.contains(other));}
    getBoundingClientRect(){return {x:0,y:0,width:900,height:250};}
  }
  const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,new Element());return elements.get(id);};
  get('group').value='model';get('grain').value='hour';get('cycle-window').tagName='select';
  const modes=['groups','chart','requests'].map(mode=>{const e=new Element('button');e.dataset.mode=mode;return e;});
  const views=['trend','groups','cycles'].map(view=>{const e=new Element('button');e.dataset.view=view;return e;});
  const statics=[...modes,...views];
  const document={getElementById:get,createElement:t=>new Element(t),createElementNS:(_,t)=>new Element(t),body:new Element('body'),addEventListener(){},querySelector:get,
    querySelectorAll(selector){return [...statics,...elements.values()].flatMap(e=>[...(e.matches(selector)?[e]:[]),...e.querySelectorAll(selector)]);}};
  const calls=[],events={};
  const window={hudRedact:{text:v=>v,toggle(){}},addEventListener:(name,fn)=>{events[name]=fn;},HudData:{shell:true,preview:false,noteToday(){},today:()=>'2026-11-06',timeZone:()=>'',now:()=>new Date('2026-11-06T12:00:00').getTime(),serverToday:async()=>'2026-11-06',listen:async(event,fn)=>{events[event]=fn;},invoke(command,args){if(command==='usage_details_context')return Promise.resolve({date:'2026-11-06'});return new Promise((resolve,reject)=>calls.push({command,args,resolve,reject}));}}};
  class Clock extends Date {constructor(...args){super(...(args.length?args:['2026-11-06T12:00:00']));}static now(){return new Date('2026-11-06T12:00:00').getTime();}}
  const context=vm.createContext({window,document,Date:Clock,console,innerWidth:1100,performance:{now:()=>1000},ResizeObserver:class{observe(){}},requestAnimationFrame:()=>1,cancelAnimationFrame(){},Event:class{},URLSearchParams,location:{search:''}});
  vm.runInContext(fs.readFileSync(require.resolve('../ui/details-data.js'),'utf8'),context);
  vm.runInContext(fs.readFileSync(require.resolve('../ui/details.js'),'utf8'),context);
  return {calls,get,modes,views,events,metric:key=>get('metrics').children.find(e=>e.dataset.metric===key)};
}
const stats={calls:50000,tokens:17500000,input:5000000,output:2500000,cacheRead:10000000,generation:50000000,speedOutput:2500000,samples:50000,priced:50000,cost:500,success:50000,failed:0,cancelled:0,unknown:0,speed:50,cache:200/3,average:.01};
const overview=(id=1)=>({datasetId:id,start:'2026-11-06',end:'2026-11-06',total:50000,cycles:[],warnings:[],facets:{sources:[{value:'source',agent:'Sample',callerKeyName:'本机'}],models:['Model A','Model B'],providers:['sample'],accounts:[{value:'["sample","account"]',provider:'sample',account:'account'}],efforts:['high']}});
const drill={field:'model',key:'Model A',from:'',to:'',grain:''};
function result(query){const requests=query.mode==='requests';return {stats,chart:[{key:'Model A',label:'Model A',stats,drill}],rows:requests?Array.from({length:30},(_,i)=>({key:String(i),label:'',stats:{...stats,calls:1,tokens:350},row:{id:'row-'+(query.page*30+i),timestamp:1793923200000+query.page*30000+i,model:'Model A',effort:'high',provider:'sample',account:'account',agent:'Sample',callerKeyName:'本机',outcome:'success',input:100,output:50,cacheRead:200},drill:null})):[{key:'Model A',label:'Model A',stats,row:null,drill}],page:query.page,pageSize:30,totalEntries:requests?50000:10,tableRequests:50000};}
(async()=>{
  const app=panel();await flush();
  assert.equal(app.calls[0].command,'query_usage_details');app.calls[0].resolve(overview());await flush();
  let call=app.calls.at(-1);assert.equal(call.command,'query_usage_details_view');assert.equal(call.args.query.datasetId,1);call.resolve(result(call.args.query));await flush();
  assert.equal(app.get('table').children[0].children[1].children.length,1);
  app.modes[2].handlers.click();await flush();call=app.calls.at(-1);assert.equal(call.args.query.mode,'requests');call.resolve(result(call.args.query));await flush();
  assert.equal(app.get('table').children[0].children[1].children.length,30,'only current thirty requests enter DOM');
  assert.match(app.get('page-label').textContent,/1–30 \/ 50000/);
  app.get('page-next').handlers.click();await flush();const slow=app.calls.at(-1);assert.equal(slow.args.query.page,1);
  app.get('page-next').handlers.click();await flush();const fast=app.calls.at(-1);assert.equal(fast.args.query.page,2);fast.resolve(result(fast.args.query));await flush();
  slow.resolve(result(slow.args.query));await flush();assert.match(app.get('page-label').textContent,/61–90 \/ 50000/,'late page must not overwrite current page');
  app.get('page-prev').handlers.click();await flush();assert.match(app.get('page-label').textContent,/31–60 \/ 50000/,'completed page response reused');
  const before=app.calls.length;app.get('page-next').handlers.click();await flush();assert.equal(app.calls.length,before,'cached immutable page does not query again');
  app.metric('cost').handlers.click();await flush();call=app.calls.at(-1);assert.equal(call.args.query.metric,'cost');call.reject(new Error('数据集已过期，请刷新'));await flush();assert.match(app.get('load-status').textContent,/过期.*刷新/);
  app.get('refresh').handlers.click();await flush();call=app.calls.at(-1);assert.equal(call.command,'query_usage_details');call.resolve(overview(2));await flush();call=app.calls.at(-1);assert.equal(call.args.query.datasetId,2);call.resolve(result(call.args.query));await flush();assert.equal(app.get('load-status').hidden,true);
  app.views[1].handlers.click();await flush();call=app.calls.at(-1);call.resolve(result(call.args.query));await flush();app.modes[0].handlers.click();await flush();call=app.calls.at(-1);call.resolve(result(call.args.query));await flush();
  const labelHeader=app.get('table').children[0].children[0].children[0].children[0].children[0];labelHeader.handlers.click();await flush();call=app.calls.at(-1);
  assert.equal(call.args.query.sort,'label');assert.deepEqual(JSON.parse(JSON.stringify(call.args.query.labelOrder)),[['Model A'],['Model B']]);call.resolve(result(call.args.query));await flush();
  const groupButton=app.get('table').children[0].children[1].children[0].children[0].children[0];groupButton.handlers.click();await flush();call=app.calls.at(-1);assert.equal(call.args.query.drill.field,'model');assert.equal(call.args.query.drill.key,'Model A');
  const cycles=panel();await flush();
  const midnight=new Date('2026-11-06T00:00:00').getTime();
  const history=overview(10);history.cycles=[{provider:'sample',account:'account',window:'Window',start:midnight,end:midnight+86400000,observedAt:midnight,remaining:50},{provider:'sample',account:'account',window:'Window',start:midnight-86400000,end:midnight,observedAt:midnight,remaining:20}];
history.cycles.push({...history.cycles[1],window:'OtherWindow'});
  const finished=new Set([cycles.calls[0]]);cycles.calls[0].resolve(history);await flush();
  let pending=cycles.calls.at(-1);finished.add(pending);pending.resolve(result(pending.args.query));await flush();cycles.views[2].handlers.click();await flush();
  for(let round=0;round<8;round++){
    for(const pending of cycles.calls.filter(c=>!finished.has(c))){finished.add(pending);if(pending.command==='query_usage_details'){const older=overview(11);older.start=pending.args.start;older.end=pending.args.end;pending.resolve(older);}else pending.resolve(result(pending.args.query));}
    await flush();
  }
  assert.equal(cycles.calls.filter(c=>c.command==='query_usage_details').length,2,'covered cycle reuses current dataset; only outside cycle loads another range');
  assert.ok(cycles.calls.some(c=>c.args?.query?.datasetId===11&&c.args.query.cycle.start===midnight-86400000));
  assert.ok(cycles.calls.length<15,'resolved cycle summaries must not cause a request loop');
  const olderButton=cycles.get('cycle-list').children[0].children[1].children[1].children[0].children[0];olderButton.handlers.click();await flush();
  pending=cycles.calls.at(-1);assert.equal(pending.args.query.datasetId,11);assert.equal(pending.args.query.cycle.end,midnight);pending.resolve(result(pending.args.query));await flush();
  cycles.get('cycle-window').value='["sample","account","OtherWindow"]';cycles.get('cycle-window').handlers.change();await flush();
  for(let round=0;round<5;round++){for(const pending of cycles.calls.filter(c=>!finished.has(c))){finished.add(pending);assert.equal(pending.command,'query_usage_details_view','same date/account reuses dataset even across quota windows');pending.resolve(result(pending.args.query));}await flush();}
  assert.equal(cycles.calls.filter(c=>c.command==='query_usage_details').length,2);
  console.log('Detail paging UI: bounded rows, page cache, stale-response isolation, errors, refresh, drill and cycle ranges passed.');
})().catch(error=>{console.error(error);process.exitCode=1;});
