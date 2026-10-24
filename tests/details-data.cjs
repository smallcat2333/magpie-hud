'use strict';
const assert = require('node:assert/strict');
const { aggregate, group, filter } = require('../ui/details-data.js');
const rows = [
  { id:'a', session:'s', sourceKey:'one', model:'A', provider:'p', account:'u', timestamp:1000, input:100, output:100, cacheRead:900, tokens:1100, generationMs:1000, outcome:'success' },
  { id:'b', session:'s', sourceKey:'two', model:'B', provider:'p', account:'u', timestamp:2000, input:900, output:900, cacheRead:100, tokens:1900, generationMs:9000, outcome:'success' },
  { id:'c', session:'t', sourceKey:'one', model:'A', provider:'p', account:'u', timestamp:3000, input:0, output:99, cacheRead:0, tokens:99, generationMs:null, outcome:'cancelled' },
];
const costs = [{ rowIds:['a'], cost:1, priced:1, unpriced:0 },{ rowIds:['b'], cost:2, priced:1, unpriced:0 }];
const all = aggregate(rows,costs);
assert.equal(all.tokens,3099);assert.equal(all.speed,100);assert.equal(all.cache,50);
assert.equal(all.cost,3);assert.equal(all.average,1.5);assert.equal(all.priced,2);assert.equal(all.cancelled,1);
const onlyA = filter(rows,{model:new Set(['A'])});
assert.equal(aggregate(onlyA,costs).cost,1,'model subset uses its own request price');
assert.equal(aggregate(filter(rows,{model:new Set(['B']),sourceKey:new Set(['two'])},'s'),costs).cost,2);
assert.equal(aggregate(filter(rows,{},'',{provider:'p',account:'u',start:1000,end:2000}),costs).cost,1);
assert.equal(group(rows,r=>r.model,costs).reduce((n,g)=>n+(g.stats.cost??0),0),all.cost);
assert.equal(aggregate([rows[2]],costs).cost,null,'unmatched remains unknown');
assert.equal(aggregate([rows[2]],[{rowIds:['c'],cost:0,priced:1}]).cost,0,'real zero cost is preserved');
assert.equal(group(rows,r=>r.model,costs).reduce((n,g)=>n+g.stats.tokens,0),all.tokens);
assert.deepEqual(filter(rows,{},'',{provider:'p',account:'u',start:1000,end:2000}).map(r=>r.id),['a'],'cycle uses half-open boundaries');
assert.equal(filter(rows,{sourceKey:new Set(['one'])},'s').length,1);
assert.equal(aggregate([],costs).cost,0);assert.equal(aggregate([],costs).speed,null);assert.equal(aggregate([],costs).cache,null);
assert.equal(aggregate([rows[2]],costs).speed,null,'cancelled request excluded from TPS');
console.log('Detail aggregation: weighted metrics, filters, cost coverage and cycle boundaries passed.');
