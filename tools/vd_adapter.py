#!/usr/bin/env python3
"""Run real pinned VisiData command-log replay; state inspection only."""
import json,sys
import visidata as v

def run(spec):
    v.vd.options.batch=True
    v.vd.options.undo=True
    sheets={}
    identities={}
    for path in spec['paths']:
        row=v.CommandLogRow(sheet='',col='',row='',longname='open-file',input=path,keystrokes='')
        if v.vd.replayOne(row):raise RuntimeError('reference open failed')
        v.vd.sync();sheet=v.vd.sheet;sheet.ensureLoaded();v.vd.sync()
        sheets[sheet.name]=sheet
        identities[sheet.name]={id(row):str(i+1) for i,row in enumerate(sheet.rows)}
    sheet=next(iter(sheets.values()));v.vd.push(sheet)
    pending=''
    def snapshot():
        out=[]
        for name,s in sheets.items():
            out.append({'alias':name,'columns':[c.name for c in s.visibleCols],
                'rows':[{'ref':identities[name][id(r)],'cells':[typed(c.getTypedValue(r)) for c in s.visibleCols],
                         'selected':s.isSelected(r)} for r in s.rows]})
        return {'sheets':out}
    checkpoints=[snapshot()]
    for step in spec['steps']:
        if step['kind']=='context':
            sheet=sheets[step['sheet']];v.vd.push(sheet)
            sheet.cursorRowIndex=step['row'];sheet.cursorVisibleColIndex=step['column']
        else:
            key=step.get('key','')
            if key=='esc': pending='';checkpoints.append(snapshot());continue
            longname=step.get('longname','')
            if key:
                pending+=key
                if not sheet.getCommand(pending):checkpoints.append(snapshot());continue
                key=pending;pending=''
            before=len(v.vd.lastErrors)
            row=v.CommandLogRow(sheet=sheet.name,col='',row='',longname=longname,input=step.get('input',''),keystrokes=key)
            if v.vd.replayOne(row):raise RuntimeError(f'reference replay aborted: {step}')
            v.vd.sync()
            if len(v.vd.lastErrors)>before:raise RuntimeError(str(v.vd.lastErrors[before:]))
        checkpoints.append(snapshot())
    return checkpoints

def typed(value):
    if value is None:return 'Null'
    if isinstance(value,str):return {'Text':value}
    if isinstance(value,bool):return {'Integer':int(value)}
    if isinstance(value,int):return {'Integer':value}
    if isinstance(value,float):return {'Float':value}
    if isinstance(value,bytes):return {'Blob':list(value)}
    raise RuntimeError(f'unsupported reference value: {type(value)}')
if __name__=='__main__':
    print(json.dumps(run(json.load(sys.stdin)),ensure_ascii=False))
