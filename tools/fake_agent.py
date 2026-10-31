#!/usr/bin/env python3
"""Offline Codex subprocess double. Never selected implicitly for real runs."""
import json,os,pathlib,sys,time
if '--help' in sys.argv:
    print('--json --output-schema --output-last-message --sandbox');sys.exit()
args=sys.argv;prompt=sys.stdin.read();review='--sandbox' in args and args[args.index('--sandbox')+1]=='read-only'
cwd=pathlib.Path(args[args.index('-C')+1]);mode=os.environ.get('PARITY_FAKE_MODE','complete')
if mode=='timeout':time.sleep(30)
if mode=='auth':print('authentication failed');sys.exit(1)
status='approved' if review else 'complete'
if review and mode=='reject':status='rejected'
if not review:
    path=cwd/'src/controller/controller_impl.rs'
    if path.exists():
        old='Command::InvertSelection => Err(AppError::invalid_input(\n                    "invert selection is not implemented",\n                )), '
        text=path.read_text()
        import re
        pattern=r'Command::InvertSelection => Err\(AppError::invalid_input\(\s*"invert selection is not implemented",?\s*\)\),'
        implementation='''Command::InvertSelection => {
                    if self.view.table_name == "table_of_tables" {
                        return Err(AppError::invalid_input("invert selection on sheet list is unsupported"));
                    }
                    let (_,rows) = self.database.get(u32::MAX,0,&self.view.table_name,"")?;
                    let selected = rows.into_iter().filter_map(|r| match r.first() {
                        Some(DataItem::Integer(id)) if !self.selected_rows.contains(id) => Some(*id),
                        _ => None,
                    }).collect::<HashSet<_>>();
                    self.selected_rows.clear();self.selected_rows.extend(selected);
                    self.sync_frequency_source_selection()?;
                    Ok(ExecutionOutcome::default())
                },'''
        path.write_text(re.sub(pattern,implementation,text))
        import subprocess
        subprocess.run(['cargo','fmt'],cwd=cwd,check=True)
    else:
        import re
        found=re.search(r'"id":\s*"([^"]+)"',prompt);ident=found.group(1) if found else 'test-task'
        filename='implementation.txt' if ident=='test-task' else f'implementation-{ident}.txt'
        (cwd/filename).write_text('implemented\n')
    if mode=='protected':(cwd/'parity/backlog.json').write_text('{}')
result=dict(status=status,summary='Offline fake agent result',findings=[])
pathlib.Path(args[args.index('--output-last-message')+1]).write_text(json.dumps(result))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':0,'output_tokens':0}}))
