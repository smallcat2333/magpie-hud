'use strict';
const fs=require('node:fs'),vm=require('node:vm'),{performance}=require('node:perf_hooks');
const api=require('../ui/details-data.js');
const n=50000, rows=[], costs=[];
for(let i=0;i<n;i++){
  const id=JSON.stringify(['request-v2',i,1793923200000+i,'sample-account',200,'','sample-session','','sample']);
  rows.push({id,timestamp:1793923200000+i,date:'2026-11-06',hour:i%24,session:'sample-session',agent:'sample-cli',callerKeyName:'sample-key',sourceKey:'["sample-cli","key"]',provider:'sample',account:'sample-account',accountKey:'["sample","sample-account"]',model:'Model '+i%10,effort:'high',outcome:'success',input:100,output:50,cacheRead:200,tokens:350,generationMs:1000});
  costs.push({session:'sample-session',rowIds:[id],cost:0.01,priced:1,unpriced:0});
}
const source=fs.readFileSync(require.resolve('../ui/details.js'),'utf8');
const body=source.slice(source.indexOf('  function renderTable()'),source.indexOf('\n  function render()',source.indexOf('  function renderTable()')));
class Element {constructor(){this.children=[];this.disabled=false;this.hidden=false;this.value='model';this.selectedOptions=[{textContent:'模型'}];}append(...v){this.children.push(...v);}replaceChildren(...v){this.children=v;}setAttribute(){}addEventListener(){} }
const elements=new Map();const $=id=>{if(!elements.has(id))elements.set(id,new Element());return elements.get(id);};
const ctx={...api,performance,console, data:{rows},displayRows:rows,viewCosts:costs,drill:null,mode:'requests',view:'groups',page:0,pageSize:30,sort:'value',descending:true,metric:'tokens',metricNames:{tokens:'Token'},units:{tokens:'tok'},$,node:()=>new Element(),document:{querySelectorAll:()=>[]},window:{hudRedact:{text:v=>v}},stamp:v=>new Date(v).toLocaleString('zh-CN',{hour12:false}),rowLabel:(k,v)=>v,valueText:v=>String(v),compactUnits:v=>String(v),sourceLabel:r=>r.agent,makeBins:()=>[]};
vm.createContext(ctx);vm.runInContext(body,ctx);
const samples=[];for(let i=0;i<3;i++){const start=performance.now();ctx.renderTable();samples.push(performance.now()-start);}
samples.sort((a,b)=>a-b);
console.log(JSON.stringify({rows:n,payloadBytes:Buffer.byteLength(JSON.stringify({start:'2026-11-06',end:'2026-11-06',rows,costs,cycles:[],warnings:[]})),samplesMs:samples,medianMs:samples[1]},null,2));
