import re,glob,os,collections,sys
d=sys.argv[1]
callers=collections.defaultdict(set); imps=collections.defaultdict(set); funcs=collections.defaultdict(set)
for f in sorted(glob.glob(d+'/seg*.asm')):
    seg=int(os.path.basename(f)[3:6]); cur=f'sub_{seg}_0000'
    lines=open(f).read().split('\n')
    for i,l in enumerate(lines):
        m=re.match(r'([0-9a-f]{4}): (push bp|inc bp)$',l)
        if m and (l.endswith('inc bp') or (i>0 and not lines[i-1].endswith('inc bp'))):
            # MS C prologue: [mov ax,ds; nop;] inc bp; push bp  OR push bp; mov bp,sp
            nxt=lines[i+1] if i+1<len(lines) else ''
            if 'push bp' in nxt or 'mov bp, sp' in nxt:
                a=m.group(1)
                if l.endswith('inc bp') and i>=2 and lines[i-2].endswith('mov ax, ds'): a=lines[i-2][:4]
                cur=f'sub_{seg}_{a}'; funcs[seg].add(a)
        m=re.search(r'lcall (\S+)',l)
        if m:
            t=m.group(1)
            if t.startswith('sub_') or t.isupper(): callers[t].add(cur)
            elif '.' in t: imps[t].add(cur)
        m=re.match(r'([0-9a-f]{4}): call (0x[0-9a-f]+)$',l)
        if m: callers[f'sub_{seg}_{int(m.group(2),16):04x}'].add(cur)
with open(d+'/XREF.txt','w') as o:
    o.write('# callee <- callers (cur = nearest preceding prologue; approximate)\n')
    for k in sorted(callers): o.write(f'{k} <- {" ".join(sorted(callers[k]))}\n')
    o.write('\n# imported API <- callers\n')
    for k in sorted(imps): o.write(f'{k} <- {" ".join(sorted(imps[k]))}\n')
print(len(callers),len(imps))
