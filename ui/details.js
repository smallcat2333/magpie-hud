'use strict';
(() => {
  const $ = id => document.getElementById(id);
  const { aggregate, group, filter } = window.usageDetailsData;
  const metricNames = { tokens: 'Token', speed: 'TPS', cost: '费用估值', cache: '缓存命中', calls: '调用次数', average: '均次费用' };
  const units = { tokens: 'tok', speed: 'tok/s', cost: '$', cache: '%', calls: '次', average: '$/次' };
  const filterNames = { sourceKey: 'Agent / 来源', model: '模型', provider: '供应商', accountKey: '账号', effort: '强度' };
  const filters = Object.fromEntries(Object.keys(filterNames).map(k => [k, new Set()]));
  let data = null, metric = 'tokens', view = 'trend', mode = 'groups', session = '', cycle = null, cycleUsage = null;
  let page = 0, request = 0, drill = null, sort = 'value', descending = true, loading = false;
  let cycleRows = [], displayRows = [], groupRows = [], viewCosts = [];
  let labels = new Map();
  const pageSize = 30;
  const ns = 'http://www.w3.org/2000/svg';
  const number = new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 2 });
  const compact = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 });
  const node = (tag, text, className) => { const el = document.createElement(tag); if (text !== undefined) el.textContent = text; if (className) el.className = className; return el; };
  function localDate(date = new Date()) { return `${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}`; }
  function offset(date, days) { const d = new Date(`${date}T12:00:00`); d.setDate(d.getDate()+days); return localDate(d); }
  function dayStart(date) { return new Date(`${date}T00:00:00`).getTime(); }
  function stamp(value) { return new Date(value).toLocaleString('zh-CN', { hour12:false }); }
  function sourceLabel(r) { return `${r.agent} · ${r.callerKeyName || '本机'}`; }
  function rowLabel(key, value) {
    if (key === 'sourceKey') return labels.get(value) || '所选来源（此范围暂无记录）';
    if (key === 'accountKey') { try { const [p,a] = JSON.parse(value); return `${p || '供应商未记录'} · ${a || '账号未记录'}`; } catch { return value; } }
    return value || '未记录';
  }
  function valueText(value, key = metric, short = false) {
    if (value === null || value === undefined || !Number.isFinite(value)) return '—';
    if (key === 'cost' || key === 'average') return value === 0 ? '0.00' : value.toFixed(value < .01 ? 4 : 2);
    if (key === 'cache' || key === 'speed') return value.toFixed(1);
    return short ? compact.format(value) : number.format(value);
  }
  function metricValue(stats) { return stats[metric]; }
  function clearDrill() { drill = null; page = 0; }
  function resetFilters() { Object.values(filters).forEach(s => s.clear()); session = ''; cycle = null; clearDrill(); }

  function bindWheel(control, valuesFor, selectedIndex, choose, enabled = () => true) {
    let sum=0, last=0, switched=0;
    control.addEventListener('wheel',e=>{
      if(!enabled() || e.ctrlKey || e.metaKey || e.altKey || e.shiftKey || !e.deltaY || Math.abs(e.deltaX)>Math.abs(e.deltaY))return;
      const values=valuesFor();if(values.length<2)return;
      e.preventDefault();
      const now=performance.now(),delta=e.deltaY*(e.deltaMode===1?16:e.deltaMode===2?control.clientHeight:1);
      if(now-last>180 || Math.sign(sum)!==Math.sign(delta))sum=0;
      last=now;sum+=delta;if(Math.abs(sum)<32 || now-switched<140)return;
      const current=selectedIndex(values),next=Math.max(0,Math.min(values.length-1,current+Math.sign(sum)));
      sum=0;switched=now;if(next!==current)choose(values[next]);
    },{passive:false});
    control.addEventListener('mouseleave',()=>{sum=0;});
  }

  Object.entries(metricNames).forEach(([key,name]) => {
    const button = node('button'); button.type = 'button'; button.dataset.metric = key;
    button.append(node('span', name, 'metric-label'), node('span', '—', 'metric-value'));
    button.addEventListener('click', () => { metric = key; clearDrill(); render(); }); $('metrics').append(button);
  });

  function buildFilters() {
    $('filter-controls').replaceChildren();
    for (const [key,name] of Object.entries(filterNames)) {
      const details = node('details', undefined, 'filter'); details.dataset.filter = key;
      const summary = node('summary', name + (filters[key].size ? ` · ${filters[key].size}` : ' · 全部'));
      const popup = node('div', undefined, 'filter-popup');
      const search = node('input'); search.type = 'search'; search.placeholder = `搜索${name}`; search.setAttribute('aria-label', `搜索${name}`);
      const options = node('div', undefined, 'filter-options');
      const candidates = (data?.rows || []).filter(r => key !== 'accountKey' || !filters.provider.size || filters.provider.has(r.provider));
      const values = [...new Set([...candidates.map(r => r[key]), ...filters[key]])].sort((a,b) => rowLabel(key,a).localeCompare(rowLabel(key,b),'zh-CN'));
      for (const value of values) {
        const label = node('label'); const box = node('input'); box.type = 'checkbox'; box.checked = filters[key].has(value); box.value = value;
        label.append(box, node('span',rowLabel(key,value)));
        box.addEventListener('change', () => {
          if (box.checked) filters[key].add(value); else filters[key].delete(value);
          if (key === 'provider') filters.accountKey.clear();
          cycle = null; clearDrill();
          summary.textContent = name + (filters[key].size ? ` · ${filters[key].size}` : ' · 全部');
          render();
          if (key === 'provider') { details.open = false; buildFilters(); }
        }); options.append(label);
      }
      if (!values.length) options.append(node('p','此范围暂无选项'));
      search.addEventListener('input', () => { for (const label of options.querySelectorAll('label')) label.hidden = !label.textContent.toLowerCase().includes(search.value.toLowerCase()); });
      details.addEventListener('toggle', () => { if (details.open) document.querySelectorAll('.filter[open]').forEach(other => { if(other !== details) other.open = false; }); });
      summary.title='悬停滚轮切换单项；点击展开可多选';
      bindWheel(summary,()=>[null,...values],options=>filters[key].size===1?Math.max(0,options.indexOf([...filters[key]][0])):0,value=>{
        filters[key].clear();if(value!==null)filters[key].add(value);
        if(key==='provider')filters.accountKey.clear();
        cycle=null;clearDrill();summary.textContent=name+(filters[key].size?` · ${filters[key].size}`:' · 全部');
        options.querySelectorAll('input[type=checkbox]').forEach(box=>{box.checked=filters[key].has(box.value);});
        if(key==='provider')buildFilters();render();
      },()=>!details.open);
      popup.append(search,options); details.append(summary,popup); $('filter-controls').append(details);
    }
  }
  async function windowAction(action) {
    try {
      if (window.__TAURI__) await window.__TAURI__.window.getCurrentWindow()[action]();
      else if (action === 'close') window.close();
    } catch (error) {
      $('load-status').hidden=false; $('load-status').dataset.error='true';
      $('load-status').textContent=`窗口操作失败：${String(error)}`;
    }
  }
  $('detail-close').addEventListener('click',()=>windowAction('close'));
  $('detail-minimize').addEventListener('click',()=>windowAction('minimize'));
  $('detail-maximize').addEventListener('click',()=>windowAction('toggleMaximize'));
  document.querySelector('.page-head').addEventListener('dblclick',e=>{if(!e.target.closest('button'))windowAction('toggleMaximize');});
  for (const direction of ['North','South','East','West','NorthEast','NorthWest','SouthEast','SouthWest']) {
    const edge=node('div',undefined,`window-resize ${direction}`);edge.setAttribute('aria-hidden','true');
    edge.addEventListener('pointerdown',e=>{if(e.button===0&&window.__TAURI__){e.preventDefault();window.__TAURI__.window.getCurrentWindow().startResizeDragging(direction).catch(()=>{});}});
    document.body.append(edge);
  }
  document.addEventListener('pointerdown', e => { document.querySelectorAll('.filter[open]').forEach(d => { if (!d.contains(e.target)) d.open = false; }); });
  document.addEventListener('keydown', e => {
    if(e.key === 'Escape' && !e.isComposing) { e.preventDefault(); windowAction('close'); }
  });

  function renderChips() {
    $('chips').replaceChildren();
    const chip = (label, remove) => { const b = node('button',`${label} ×`); b.type='button'; b.title=`移除 ${label}`; b.addEventListener('click',remove); $('chips').append(b); };
    for (const [key,values] of Object.entries(filters)) for(const value of values) chip(rowLabel(key,value), () => { values.delete(value); cycle=null; clearDrill(); buildFilters(); render(); });
    if(session) chip(`会话 ${session.slice(0,12)}`, () => { session=''; clearDrill(); render(); });
    if(!$('chips').childElementCount) $('chips').append(node('span','全部来源', 'note'));
  }

  function makeBins(rows) {
    if (view !== 'trend') return group(rows, r => r[$('group').value], viewCosts)
      .map(g => ({...g, label:rowLabel($('group').value,g.key)})).sort((a,b) => (metricValue(b.stats) ?? -1) - (metricValue(a.stats) ?? -1));
    const hourly = $('grain').value === 'hour';
    const key = r => hourly ? `${r.date} ${String(r.hour).padStart(2,'0')}:00` : r.date;
    const bins = new Map(group(rows,key,viewCosts).map(g => [g.key,g]));
    const result = [];
    for(let date=data.start; date<=data.end; date=offset(date,1)) {
      for(let h=0; h<(hourly?24:1); h++) {
        const k = hourly ? `${date} ${String(h).padStart(2,'0')}:00` : date;
        const b = bins.get(k) || {key:k,rows:[],stats:aggregate([],viewCosts)};
        result.push({...b,label:hourly && data.start===data.end ? `${String(h).padStart(2,'0')}:00` : k.slice(5)});
      }
    }
    return result;
  }

  function cycleKey(c) { return c ? JSON.stringify([c.provider,c.account,c.window,c.start,c.end]) : ''; }

  /** 扩展查询未就绪时，统计单元格显示加载或失败状态。 */
  function loadingCell(kind, title) {
    const td=node('td',kind==='error'?'读取失败':'加载中','num cycle-usage-loading');
    if(kind!=='error'){td.setAttribute('aria-busy','true');const spinner=node('span',undefined,'loading-spinner');spinner.setAttribute('aria-hidden','true');td.prepend(spinner);}
    if(title)td.title=title;
    return td;
  }

  // 实际使用=周期开始至今的累计金额，只认周期归属，不受日期范围与筛选影响；范围不足时扩展查询一次。
  function cycleUsageStats(c) {
    if (cycleUsage?.status!=='ready') return null;
    return aggregate(filter(cycleUsage.data.rows,{},'',c), cycleUsage.data.costs);
  }
  function ensureCycleUsage() {
    if(!cycleRows.length)return;
    const start=localDate(new Date(Math.min(...cycleRows.map(c=>c.start)))), end=localDate(), key=`${start}|${end}`;
    if(cycleUsage?.key===key)return;
    if(start>=data.start && end<=data.end) { cycleUsage={key,status:'ready',data}; queueMicrotask(render); return; }
    cycleUsage={key,status:'loading'};
    const sourceData=data;
    const query=window.__TAURI__
      ? window.__TAURI__.core.invoke('query_usage_details',{start,end})
      : (window.hudDetailsPreviewQuery?window.hudDetailsPreviewQuery(start,end):Promise.reject(new Error('无数据源')));
    query.then(result=>{ if(cycleUsage?.key!==key || data!==sourceData)return; cycleUsage={key,status:'ready',data:result}; render(); })
      .catch(error=>{ if(cycleUsage?.key!==key || data!==sourceData)return; cycleUsage={key,status:'error',error:String(error)}; render(); });
  }

  function renderCycles(baseRows) {
    const control = $('cycle-window'), selectedWindow = control.value;
    const keys = [...new Set(data.cycles.filter(c =>
      (!filters.provider.size || filters.provider.has(c.provider)) &&
      (!filters.accountKey.size || filters.accountKey.has(JSON.stringify([c.provider,c.account]))))
      .map(c=>JSON.stringify([c.provider,c.account,c.window])))];
    // 不在每次指标/周期切换时替换原生下拉的选项，保留弹层和焦点状态。
    if(JSON.stringify([...control.options].map(o=>o.value))!==JSON.stringify(keys)) {
      control.replaceChildren();
      keys.forEach(k => { const o=node('option',JSON.parse(k).map(x=>x || '未记录').join(' · ')); o.value=k; control.append(o); });
      if(keys.includes(selectedWindow)) control.value=selectedWindow;
    }
    const selected=control.value, previousCycle=cycleKey(cycle);
    // 周期列表不受所选日期范围限制，直接回溯该账号与窗口的全部已记录周期。
    cycleRows=data.cycles.filter(c => JSON.stringify([c.provider,c.account,c.window])===selected);
    cycle=cycleRows.find(c=>cycleKey(c)===previousCycle)||cycleRows[0]||null;
    const monetary=metric==='cost'||metric==='average';
    const extended=cycleUsage?.status==='ready'?cycleUsage.data:null;
    const rangeCoversNow=Date.now()<dayStart(offset(data.end,1));
    const cols=['周期（本地时间）',`${metric==='cost'?'100%用量预估':metricNames[metric]}（${units[metric]}）`,'实际使用估值（$）','调用',...(monetary?['费用覆盖']:[]),'最近额度读数','状态'];
    const table=node('table'), head=node('thead'), hr=node('tr'), body=node('tbody'); cols.forEach(c=>hr.append(node('th',c))); head.append(hr); table.append(head,body);
    cycleRows.forEach(c => {
      // 扩展查询就绪即用完整周期数据；未就绪时仅当所选范围已覆盖该周期至今的请求才用当前数据。
      const source=extended||((c.start>=dayStart(data.start)&&rangeCoversNow)?data:null);
      const s=source?aggregate(filter(source.rows,filters,session,c),source.costs):null;
      const tr=node('tr');
      tr.classList.toggle('selected',c===cycle);
      const cell=node('td'), button=node('button',stamp(c.start)); button.type='button';
      button.setAttribute('aria-pressed',String(c===cycle));
      const select=()=>{cycle=c;clearDrill();render();};
      button.addEventListener('click',select);
      tr.addEventListener('click',e=>{if(!e.target.closest('button'))select();});
      cell.append(button); tr.append(cell);
      const used=c.remaining==null?null:Math.min(100,Math.max(0,100-c.remaining));
      const projected=metric==='cost'&&s&&s.cost!=null&&used!==null&&used>=1?s.cost*100/used:null;
      const value=node('td',s?`${valueText(metric==='cost'?projected:s[metric],metric)} ${units[metric]}`:'—','num');
      if(s)value.title=metric==='cost'
        ?(projected!=null
          ?`100%用量费用预估 ${valueText(projected,'cost')} $ = 周期累计 ${valueText(s.cost,'cost')} $ ÷ 已用 ${used.toFixed(1)}%；未定价请求不计入累计；实际累计见「实际使用估值」列`
          :`100%用量费用预估暂无法折算（${used==null?'无额度读数':'已用不足 1% 或暂无已定价费用'}）；周期累计 ${valueText(s.cost,'cost')} $ 见「实际使用估值」列`)
        :`${metricNames[metric]} ${valueText(s[metric],metric)} ${units[metric]}${monetary?`；费用覆盖 ${s.priced}/${s.calls} 次；未覆盖部分不计入已知金额`:''}`;
      else value.title=cycleUsage?.status==='error'?`周期统计读取失败：${cycleUsage.error}`:'周期统计加载中';
      tr.append(value);
      const usage=cycleUsageStats(c), actual=node('td',usage?`${valueText(usage.cost,'cost')} $`:(cycleUsage?.status==='error'?'读取失败':'加载中'),'num');
      if(!usage && cycleUsage?.status!=='error') {
        actual.classList.add('cycle-usage-loading');
        actual.setAttribute('aria-busy','true');
        const spinner=node('span',undefined,'loading-spinner');
        spinner.setAttribute('aria-hidden','true');
        actual.prepend(spinner);
      }
      actual.title=usage?`周期开始至今累计 ${valueText(usage.cost,'cost')} $ · ${number.format(usage.tokens)} Token · ${usage.calls} 次 · 费用覆盖 ${usage.priced}/${usage.calls}；按模型定价折算，非实际扣款；不受日期范围与筛选影响${cycleUsage.data.warnings?.length?`；${cycleUsage.data.warnings.join('；')}`:''}`
        :(cycleUsage?.status==='error'?`实际使用读取失败：${cycleUsage.error}`:'实际使用加载中');
      tr.append(actual,s?node('td',String(s.calls),'num'):loadingCell(cycleUsage?.status==='error'?'error':'loading'));
      if(monetary)tr.append(s?node('td',`${s.priced}/${s.calls} 次`,'num'):loadingCell(cycleUsage?.status==='error'?'error':'loading'));
      const read=node('td',c.remaining===null?'—':`已用 ${(100-c.remaining).toFixed(1)}%`,'num'); read.title=`观测于 ${stamp(c.observedAt)}；不是期末最终读数`; tr.append(read);
      tr.append(node('td',`${c.end>Date.now()?'进行中':'已结束'}`)); body.append(tr);
    });
    if(!cycleRows.length) { const td=node('td',keys.length?'所选账号与窗口没有明确边界的周期':'没有可用的明确周期边界', 'empty'); td.colSpan=cols.length; const tr=node('tr');tr.append(td);body.append(tr); }
    $('cycle-list').replaceChildren(table);
    $('cycle-message').textContent='周期统计覆盖完整周期并遵循当前筛选；实际使用估值为周期开始至今的累计金额，只覆盖本地日志，不受日期范围与筛选影响';
    $('cycle-actions').hidden=!cycle;
    const i=cycleRows.indexOf(cycle); $('cycle-prev').disabled=i<0||i>=cycleRows.length-1; $('cycle-next').disabled=i<=0;
    ensureCycleUsage();
  }

  function svgNode(tag, attrs={}, text) { const el=document.createElementNS(ns,tag);for(const [k,v] of Object.entries(attrs)) el.setAttribute(k,String(v));if(text!==undefined)el.textContent=text;return el; }
  function chart(bins) {
    $('chart').replaceChildren(); $('chart-tooltip').hidden=true;
    const width=Math.max(350,$('chart').clientWidth), height=$('chart').clientHeight, left=58, right=16, top=12, bottom=32;
    const w=width-left-right,h=height-top-bottom;
    const svg=svgNode('svg',{viewBox:`0 0 ${width} ${height}`,role:'img','aria-label':`${metricNames[metric]}，${units[metric]}；明细表提供精确值`});
    let plotted=bins;
    // 保持完整时间轴。密集小时序列按连续区间合并，重新按累计量算比率。
    const capacity=Math.max(8,Math.floor(w/12));
    if(view==='trend' && bins.length>capacity) {
      const span=Math.ceil(bins.length/capacity); plotted=[];
      for(let i=0;i<bins.length;i+=span) {const part=bins.slice(i,i+span), rows=part.flatMap(b=>b.rows);plotted.push({key:part[0].key,label:`${part[0].label} — ${part.at(-1).label}`,rows,stats:aggregate(rows,viewCosts)});}
    } else if(view!=='trend') plotted=bins.slice(0,16);
    const values=plotted.map(b=>metricValue(b.stats));
    const max=Math.max(0,...values.filter(v=>Number.isFinite(v)));
    const power=10**Math.floor(Math.log10(max||1));
    let step=Math.max(power/2,Math.ceil(max/power/4)*power);
    if(metric==='calls'||metric==='tokens')step=Math.max(1,Math.ceil(step));
    if(metric==='cache')step=25;
    const ceiling=metric==='cache'?100:step*4;
    for(let i=0;i<=4;i++){const y=top+h-h*i/4;svg.append(svgNode('line',{x1:left,y1:y,x2:width-right,y2:y,class:'grid'}),svgNode('text',{x:left-8,y:y+4,'text-anchor':'end'},valueText(ceiling*i/4,metric,true)));}
    const stride=w/Math.max(1,plotted.length), points=[];
    const line=['speed','cache','average'].includes(metric) && view==='trend';
    plotted.forEach((b,i)=>{
      const value=metricValue(b.stats),x=left+stride*(i+.5),y=top+h-h*(value??0)/ceiling;
      if(value!==null&&Number.isFinite(value)) {
        if(line) points.push([x,y]);
        else svg.append(svgNode('rect',{x:x-Math.min(26,stride*.65)/2,y,width:Math.min(26,stride*.65),height:Math.max(0,top+h-y),rx:2,class:'data-mark'}));
      } else if(line) points.push(null);
      const labelStep=Math.max(1,Math.ceil(plotted.length/Math.max(3,Math.floor(w/105))));
      if(i%labelStep===0) svg.append(svgNode('text',{x,y:height-9,'text-anchor':i===0?'start':'middle'},b.label.length>17?`${b.label.slice(0,15)}…`:b.label));
      const hit=svgNode('rect',{x:left+stride*i,y:top,width:Math.max(1,stride),height:h,class:'hit',tabindex:0,role:'button','aria-label':`${b.label} ${valueText(value)} ${units[metric]}，查看明细`});
      const show=e=>{const t=$('chart-tooltip');t.textContent=`${b.label} · ${valueText(value)} ${units[metric]} · ${b.stats.calls} 次调用`;t.hidden=false;const rect=hit.getBoundingClientRect();t.style.left=`${Math.max(8,Math.min(e.clientX||rect.x,innerWidth-330))}px`;t.style.top=`${Math.max(8,(e.clientY||rect.y)-45)}px`;};
      hit.addEventListener('pointermove',show);hit.addEventListener('focus',show);hit.addEventListener('pointerleave',()=>$('chart-tooltip').hidden=true);hit.addEventListener('blur',()=>$('chart-tooltip').hidden=true);
      const drillDown=()=>{drill={label:b.label,ids:new Set(b.rows.map(r=>r.id))};mode='requests';page=0;renderTable();};
      hit.addEventListener('click',drillDown);hit.addEventListener('keydown',e=>{if(e.key==='Enter'||e.key===' '){e.preventDefault();drillDown();}});svg.append(hit);
    });
    if(line){let path='';let connected=false;points.forEach(p=>{if(!p){connected=false;return;}path+=`${connected?'L':'M'}${p[0]} ${p[1]} `;connected=true;}); const p=svgNode('path',{d:path,class:'data-line','pointer-events':'none'});svg.append(p);points.filter(Boolean).forEach(p=>svg.append(svgNode('circle',{cx:p[0],cy:p[1],r:3,class:'data-mark','pointer-events':'none'})));}
    if(!bins.length || !data.rows.length)svg.append(svgNode('text',{x:left+w/2,y:top+h/2,'text-anchor':'middle'},'所选范围暂无请求'));
    $('chart').append(svg);
    $('chart-caption').textContent=view==='trend'?`${data.start} — ${data.end}${plotted.length<bins.length?' · 相邻时段合并显示':''}`:`按${$('group').selectedOptions[0].textContent}${bins.length>16?' · 图示前16组，表格含全部':''}`;
  }

  function renderTable() {
    if (!data) return;
    const rows=drill?displayRows.filter(r=>drill.ids.has(r.id)):displayRows;
    const grouping=$('group').value;
    const grouped=group(rows,r=>r[grouping],viewCosts).map(g=>({...g,label:rowLabel(grouping,g.key)}));
    const requestMode=mode==='requests';
    const entries=requestMode?rows.map(r=>({label:stamp(r.timestamp),row:r,stats:aggregate([r],viewCosts)})):mode==='chart'?makeBins(rows):grouped;
    entries.sort((a,b)=>{
      const av=sort==='label'?a.label:sort==='time'?a.row?.timestamp:sort==='value'?a.stats[metric]:a.stats[sort];
      const bv=sort==='label'?b.label:sort==='time'?b.row?.timestamp:sort==='value'?b.stats[metric]:b.stats[sort];
      if(av==null)return bv==null?0:1;if(bv==null)return -1;
      const cmp=typeof av==='string'?av.localeCompare(bv,'zh-CN'):av-bv;return descending?-cmp:cmp;
    });
    page=Math.max(0,Math.min(page,Math.ceil(entries.length/pageSize)-1));
    const table=node('table'),thead=node('thead'),head=node('tr'),tbody=node('tbody');
    const columns=requestMode?[['时间','time'],['模型 / 强度',''],['供应商 / 账号',''],['来源',''],['状态',''],[metricNames[metric],'value'],['输入','input'],['输出','output'],['缓存读取','cacheRead']]
      :[[mode==='chart'&&view==='trend'?'时段':$('group').selectedOptions[0].textContent,'label'],[metricNames[metric],'value'],['调用','calls'],['输入','input'],['输出','output'],['缓存读取','cacheRead'],['费用覆盖','']];
    columns.forEach(([name,key],i)=>{const th=node('th');if(i>=(requestMode?5:1))th.className='num';if(key){const b=node('button',name+(sort===key?(descending?' ↓':' ↑'):''));b.type='button';b.addEventListener('click',()=>{descending=sort===key?!descending:true;sort=key;renderTable();});th.append(b);}else th.textContent=name;head.append(th);});
    thead.append(head);table.append(thead,tbody);
    entries.slice(page*pageSize,(page+1)*pageSize).forEach(entry=>{
      const tr=node('tr'); const cell=(text,numeric=false)=>{const td=node('td',text,numeric?'num':'');td.title=text;tr.append(td);};
      if(requestMode){const r=entry.row;cell(stamp(r.timestamp));cell(`${r.model} · ${r.effort}`);cell(`${r.provider||'未记录'} · ${r.account}`);cell(sourceLabel(r));cell(({success:'成功',failed:'失败',cancelled:'取消',unknown:'未知'})[r.outcome]||'未知');}
      else{const td=node('td'),b=node('button',entry.label);b.type='button';b.title=entry.label;b.addEventListener('click',()=>{drill={label:entry.label,ids:new Set(entry.rows.map(r=>r.id))};mode='requests';page=0;renderTable();});td.append(b);tr.append(td);}
      cell(`${valueText(entry.stats[metric])} ${units[metric]}`,true);
      if(!requestMode)cell(String(entry.stats.calls),true);
      cell(number.format(entry.stats.input),true);cell(number.format(entry.stats.output),true);cell(number.format(entry.stats.cacheRead),true);
      if(!requestMode)cell(`${entry.stats.priced}/${entry.stats.calls} 次`,true);
      tbody.append(tr);
    });
    if(!entries.length){const tr=node('tr'),td=node('td','当前条件没有请求记录','empty');td.colSpan=columns.length;tr.append(td);tbody.append(tr);}
    $('table').replaceChildren(table);$('row-count').textContent=`${rows.length.toLocaleString()} 次请求${drill?` · ${drill.label}`:''}`;
    $('page-label').textContent=`${entries.length?page*pageSize+1:0}–${Math.min((page+1)*pageSize,entries.length)} / ${entries.length} ${requestMode?'条':'组'}`;
    $('page-prev').disabled=page===0;$('page-next').disabled=(page+1)*pageSize>=entries.length;$('drill-clear').hidden=!drill;
    document.querySelectorAll('[data-mode]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.mode===mode)));
  }

  function render() {
    renderChips();
    document.querySelectorAll('[data-metric]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.metric===metric)));
    document.querySelectorAll('[data-view]').forEach(b=>b.setAttribute('aria-selected',String(b.dataset.view===view)));
    $('cycle-controls').hidden=view!=='cycles';$('cycle-list').hidden=view!=='cycles';$('cycle-actions').hidden=view!=='cycles'||!cycle;
    $('grain-label').hidden=view!=='trend';$('group-label').hidden=false;
    if(!data)return;
    const base=filter(data.rows,filters,session);
    if(view==='cycles')renderCycles(base);
    // 周期视图使用扩展查询的完整周期数据；未就绪时先用所选范围内的部分数据，就绪后自动补全。
    const cycleSource=view==='cycles'&&cycle?(cycleUsage?.status==='ready'?cycleUsage.data:data):null;
    displayRows=view==='cycles'?(cycle?filter(cycleSource.rows,filters,session,cycle):[]):filter(base,{},{});
    viewCosts=(cycleSource||data).costs;
    const stats=aggregate(displayRows,viewCosts);
    document.querySelectorAll('[data-metric]').forEach(b=>{const k=b.dataset.metric;const value=b.querySelector('.metric-value');const unavailable=view==='cycles'&&!cycle;value.textContent=unavailable?'—':valueText(stats[k],k,true);value.append(node('span',units[k],'metric-unit'));b.title=`${metricNames[k]} ${unavailable?'—':valueText(stats[k],k)} ${units[k]}`;if(k==='cost'||k==='average'){b.querySelector('.metric-label').textContent=metricNames[k];b.title+=`；费用覆盖 ${stats.priced}/${stats.calls} 次`;}});
    groupRows=makeBins(displayRows);
    $('chart-title').textContent=`${metricNames[metric]} · ${view==='trend'?'时间趋势':view==='cycles'?'周期内分组':'分组对比'}`;
    chart(groupRows);
    const notes={tokens:`输入 ${number.format(stats.input)} + 输出 ${number.format(stats.output)} + 缓存读取 ${number.format(stats.cacheRead)} Token`,speed:`${stats.samples} 个有效成功样本 · 有效生成时间 ${(stats.generation/1000).toFixed(2)} 秒；不平均各请求速度`,cache:`缓存读取 /（输入 + 缓存读取），按 Token 加权`,calls:`成功 ${stats.success} · 失败 ${stats.failed} · 取消 ${stats.cancelled} · 未知 ${stats.unknown}`,cost:`费用覆盖 ${stats.priced}/${stats.calls} 次；按 Magpie 逐请求金额汇总，未覆盖部分不计入已知金额`,average:`已知费用 / ${stats.priced} 次已定价调用；未定价调用不参与均值`};
    $('metric-note').textContent=notes[metric]+(cycle&&view==='cycles'?' · 统计所选完整周期':'');
    renderTable();
  }

  async function load(fullCycle = null) {
    const start=$('start').value,end=$('end').value;
    if(!start||!end||start>end||end>localDate()||(dayStart(end)-dayStart(start))/86400000>365){$('load-status').hidden=false;$('load-status').dataset.error='true';$('load-status').textContent='请选择有效日期范围，结束日期不晚于今天，一次最多 366 天';return;}
    const keepView=data?.start===start && data?.end===end;
    const id=++request;loading=true;if(!keepView){data=null;clearDrill();}$('load-status').hidden=false;$('load-status').dataset.error='false';$('load-status').textContent='正在读取用量…';
    document.querySelector('main').setAttribute('aria-busy','true');$('refresh').disabled=true;
    if(!keepView){
      $('chart').replaceChildren();$('table').replaceChildren();$('cycle-list').replaceChildren();
      document.querySelectorAll('.metric-value').forEach(v=>v.textContent='—');
    }
    try {
      const result=window.__TAURI__?await window.__TAURI__.core.invoke('query_usage_details',{start,end})
        :await (window.hudDetailsPreviewQuery?window.hudDetailsPreviewQuery(start,end):Promise.reject(new Error('独立预览未注入数据，请从 HUD 打开详情')));
      if(id!==request)return;
      if(result.start!==start||result.end!==end)throw new Error('返回的统计日期与所选范围不一致');
      data=result;labels=new Map();data.rows.forEach(r=>{r.accountKey=JSON.stringify([r.provider,r.account]);labels.set(r.sourceKey,sourceLabel(r));});cycleUsage=null;
      if (Array.isArray(fullCycle)) cycle=data.cycles.find(c=>JSON.stringify([c.provider,c.account,c.window,c.start,c.end])===JSON.stringify(fullCycle))||null;
      $('load-status').hidden=true;$('warnings').textContent=data.warnings.join('；');
      $('updated').textContent=`更新于 ${new Date().toLocaleTimeString('zh-CN',{hour12:false})} · ${data.start} — ${data.end}`;
      buildFilters();render();
    } catch(error){if(id!==request)return;$('load-status').dataset.error='true';$('load-status').textContent=`读取失败：${String(error)}。可点击刷新重试。`;}
    finally{if(id===request){loading=false;$('refresh').disabled=false;document.querySelector('main').setAttribute('aria-busy','false');}}
  }
  function applyContext(context) {
    metric=metricNames[context.metric]?context.metric:'tokens';
    resetFilters();if(context.source&&context.source!=='*')filters.sourceKey.add(context.source);session=context.session||'';
    $('start').value=context.date||localDate();$('end').value=context.date||localDate();$('grain').value='hour';view='trend';mode='groups';render();load();
  }
  $('range-form').addEventListener('submit',e=>{e.preventDefault();cycle=null;load();});$('refresh').addEventListener('click',load);
  document.querySelectorAll('[data-days]').forEach(b=>b.addEventListener('click',()=>{const days=Number(b.dataset.days);const end=days===-1?offset(localDate(),-1):localDate();$('end').value=end;$('start').value=days>0?offset(end,1-days):end;$('grain').value=days>1?'day':'hour';cycle=null;load();}));
  $('clear').addEventListener('click',()=>{resetFilters();buildFilters();render();});
  document.querySelectorAll('[data-view]').forEach(b=>b.addEventListener('click',()=>{view=b.dataset.view;clearDrill();render();}));
  document.querySelectorAll('[data-mode]').forEach(b=>b.addEventListener('click',()=>{mode=b.dataset.mode;page=0;if(data)renderTable();}));
  $('group').addEventListener('change',()=>{clearDrill();render();});$('grain').addEventListener('change',()=>{clearDrill();render();});
  $('cycle-window').addEventListener('change',()=>{cycle=null;clearDrill();render();});
  $('cycle-prev').addEventListener('click',()=>{cycle=cycleRows[cycleRows.indexOf(cycle)+1]||cycle;clearDrill();render();});
  $('cycle-next').addEventListener('click',()=>{cycle=cycleRows[cycleRows.indexOf(cycle)-1]||cycle;clearDrill();render();});
  $('cycle-clear').addEventListener('click',()=>{cycle=null;view='trend';clearDrill();render();});
  $('cycle-full').addEventListener('click',()=>{if(!cycle)return;$('start').value=localDate(new Date(cycle.start));$('end').value=localDate(new Date(Math.min(Date.now(),cycle.end-1)));const identity=[cycle.provider,cycle.account,cycle.window,cycle.start,cycle.end];load(identity);});
  $('drill-clear').addEventListener('click',()=>{clearDrill();renderTable();});
  $('page-prev').addEventListener('click',()=>{page--;renderTable();});$('page-next').addEventListener('click',()=>{page++;renderTable();});
  let resizeFrame;new ResizeObserver(()=>{cancelAnimationFrame(resizeFrame);resizeFrame=requestAnimationFrame(()=>{if(data)chart(groupRows);});}).observe($('chart'));
  ['group','grain','cycle-window'].forEach(id=>{
    const control=$(id);control.title='悬停滚轮切换';
    bindWheel(control,()=>[...control.options].filter(o=>!o.disabled).map(o=>o.value),values=>values.indexOf(control.value),value=>{
      control.value=value;control.dispatchEvent(new Event('change',{bubbles:true}));
    },()=>!control.disabled);
  });
  $('start').max=$('end').max=localDate();
  if(innerWidth<640)document.querySelector('.filter-disclosure').open=false;
  async function start() {
    if(window.__TAURI__){await window.__TAURI__.event.listen('usage-details-context',e=>applyContext(e.payload));applyContext(await window.__TAURI__.core.invoke('usage_details_context'));}
    else {window.addEventListener('hud-details-preview-context',e=>applyContext(e.detail));const ctx=new URLSearchParams(location.search);applyContext({metric:ctx.get('metric'),date:ctx.get('date'),source:ctx.get('source'),session:ctx.get('session')});}
  }
  start().catch(error=>{$('load-status').hidden=false;$('load-status').textContent=String(error);});
})();
