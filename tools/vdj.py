#!/usr/bin/env python3
"""Strict supported subset of VisiData JSONL command logs. Full VDJ is deferred."""
import argparse,json,sys
# Verified stateless commands; context carries explicit zero-based indices.
LONGNAMES={'stoggle-rows':'invert_selection','unselect-rows':'clear_selection'}
def decode(lines):
    out=[{'kind':'header','schema_version':1}]
    for number,line in enumerate(lines,1):
        if not line.strip() or line.startswith("#"):continue
        r=json.loads(line)
        if set(r)-{'sheet','col','row','longname','input','keystrokes','comment'}:raise ValueError(f'VDJ line {number}: unsupported fields')
        if r.get('longname') not in LONGNAMES:raise ValueError(f'VDJ line {number}: unsupported longname')
        if r.get('input') or r.get('col') or r.get('row'):raise ValueError(f'VDJ line {number}: parameters/context semantics unsupported')
        if r.get('sheet'):out.append({'kind':'context','sheet':r['sheet'],'row':0,'column':0})
        out.append({'kind':'command','name':LONGNAMES[r['longname']],'inputs':[]})
    return out
def encode(records):
    out=[];sheet=''
    if not records or records[0]!={'kind':'header','schema_version':1}:raise ValueError('expected native schema version 1')
    for r in records[1:]:
        if r['kind']=='context':
            if r['row'] or r['column']:raise ValueError('VDJ indexed context semantics unsupported')
            sheet=r['sheet'];continue
        if r['kind']!='command' or r.get('inputs'):raise ValueError('VDJ supports only parameterless registry commands')
        reverse={v:k for k,v in LONGNAMES.items()}
        if r['name'] not in reverse:raise ValueError('unsupported VDJ command')
        out.append(dict(sheet=sheet,col='',row='',longname=reverse[r['name']],input='',keystrokes='',comment=''))
    return out
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('command',choices=['import','export']);p.add_argument('input');p.add_argument('output');a=p.parse_args()
    with open(a.input) as f:records=decode(f) if a.command=='import' else encode([json.loads(l) for l in f])
    with open(a.output,'w') as f:
        for r in records:f.write(json.dumps(r)+'\n')
