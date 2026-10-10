import sqlite3,time
from pathlib import Path
root=Path('/data/dev/stationd')
con=sqlite3.connect('file:'+str(root/'data/plugins/listener-stats.db')+'?mode=ro',uri=True,timeout=0.2)
params=dict(period='24 h',**{'from':'','to':'','mount':'','grouping':'Total'})
for name in ['audience','hourly','daily','dashboard']:
    sql=(root/'plugins/listener-stats-wasm/src/ui/period.sql').read_text()+(root/f'plugins/listener-stats-wasm/src/ui/{name}.sql').read_text()
    if name=='dashboard': sql=sql.replace("'Pays'", "'Pays'")
    started=time.monotonic();con.set_progress_handler(lambda: int(time.monotonic()-started>5),1000)
    try:
        rows=con.execute(sql,params).fetchall();print(name,len(rows),'rows',round((time.monotonic()-started)*1000),'ms',flush=True)
    except Exception as e:print(name,type(e).__name__,str(e),round((time.monotonic()-started)*1000),'ms',flush=True)
    if name=='audience':
        con.set_progress_handler(None,0)
        for r in con.execute('EXPLAIN QUERY PLAN '+sql,params):print(r[3],flush=True)
