import struct,sys,os,re
from capstone import *
SPEC=os.path.join(os.path.dirname(__file__),'spec')
specmap={'KERNEL':'krnl386.exe16','GDI':'gdi.exe16','USER':'user.exe16','COMMDLG':'commdlg.dll16','WIN87EM':'win87em.dll16'}
def loadspec(m):
    r={}
    f=os.path.join(SPEC,specmap.get(m,'x')+'.spec')
    if os.path.exists(f):
        for l in open(f):
            mm=re.match(r'^(\d+)\s+\S+\s+(?:-\S+\s+)*(\w+)',l)
            if mm: r[int(mm.group(1))]=mm.group(2)
    return r
class NE:
    def __init__(s,path):
        d=s.d=open(path,'rb').read()
        ne=s.ne=struct.unpack_from('<I',d,0x3c)[0]
        s.segcnt,s.modcnt=struct.unpack_from('<HH',d,ne+0x1c)
        segtab,rsrctab,resntab,modtab,imptab=struct.unpack_from('<HHHHH',d,ne+0x22)
        entab=struct.unpack_from('<H',d,ne+4)[0]
        nrestab=struct.unpack_from('<I',d,ne+0x2c)[0]
        s.align=struct.unpack_from('<H',d,ne+0x32)[0]
        s.autodata=struct.unpack_from('<H',d,ne+0x0e)[0]
        s.mods=[]
        for i in range(s.modcnt):
            o=struct.unpack_from('<H',d,ne+modtab+i*2)[0]; p=ne+imptab+o
            s.mods.append(d[p+1:p+1+d[p]].decode())
        s.imptab=ne+imptab
        s.specs={m:loadspec(m) for m in s.mods}
        for m in s.mods:
            dll=os.path.join(os.path.dirname(os.path.abspath(path)),m+'.DLL')
            if not s.specs[m] and os.path.exists(dll) and os.path.abspath(dll)!=os.path.abspath(path):
                sub=NE(dll); s.specs[m]={o:sub.names.get(e,f'#{o}') for o,e in sub.entries.items()}
        s.segs=[]
        for i in range(s.segcnt):
            off,ln,fl,mn=struct.unpack_from('<HHHH',d,ne+segtab+i*8)
            s.segs.append((off<<s.align,ln or (0x10000 if off else 0),fl,mn or 0x10000))
        # entry table -> (seg,off) by ordinal
        s.entries={}; p=ne+entab; ordn=1
        while True:
            cnt,ind=d[p],d[p+1]; p+=2
            if cnt==0: break
            for k in range(cnt):
                if ind==0: pass
                elif ind==0xff: fl,_,sg,of=struct.unpack_from('<BHBH',d,p); p+=6; s.entries[ordn]=(sg,of)
                else: fl,of=struct.unpack_from('<BH',d,p); p+=3; s.entries[ordn]=(ind,of)
                ordn+=1
        s.names={}
        for tab in (ne+resntab,nrestab):
            p=tab
            while d[p]:
                n=d[p];nm=d[p+1:p+1+n].decode('latin1');o=struct.unpack_from('<H',d,p+1+n)[0];p+=n+3
                if o in s.entries: s.names[s.entries[o]]=nm
    def segdata(s,i):
        off,ln,fl,mn=s.segs[i-1]
        return bytearray(s.d[off:off+ln]) if off else bytearray(mn)
    def relocs(s,i):
        off,ln,fl,mn=s.segs[i-1]; out={}
        if not off or not fl&0x100: return out
        p=off+ln; n=struct.unpack_from('<H',s.d,p)[0]; p+=2
        data=s.segdata(i)
        for k in range(n):
            st,flg,so,t1,t2=struct.unpack_from('<BBHHH',s.d,p); p+=8
            kind=flg&3
            if kind==0:
                if (t1&0xff)==0xff: tgt=('int',)+s.entries.get(t2,(0,0)); 
                else: tgt=('int',t1&0xff,t2)
            elif kind==1:
                m=s.mods[t1-1]; tgt=('imp',m,s.specs.get(m,{}).get(t2,f'#{t2}'))
            elif kind==2:
                q=s.imptab+t2; tgt=('imp',s.mods[t1-1],s.d[q+1:q+1+s.d[q]].decode())
            else: tgt=('os',t1)
            locs=[so]
            if not flg&4 and kind!=3:
                while True:
                    nx=struct.unpack_from('<H',data,locs[-1])[0]
                    if nx==0xffff or nx in locs or nx>=len(data): break
                    locs.append(nx)
            for l in locs:
                t=tgt
                if kind==0 and st==2 and tgt[0]=='int' and (t1&0xff)!=0xff:
                    if l>=3 and data[l-3]==0x9a: t=('int',tgt[1],struct.unpack_from('<H',data,l-2)[0])
                    else: t=('seg',tgt[1])
                out[l]=(st,t)
        return out
def tname(ne,tgt):
    if tgt[0]=='imp': return f'{tgt[1]}.{tgt[2]}'
    if tgt[0]=='int':
        nm=ne.names.get((tgt[1],tgt[2]))
        return nm or f'sub_{tgt[1]}_{tgt[2]:04x}'
    if tgt[0]=='seg': return f'SEG{tgt[1]} (far ptr segment; offset pushed next)'
    return f'OS{tgt[1]}'
def fpu_unemulate(data):
    i=0
    while i<len(data)-2:
        if data[i]==0xCD and 0x34<=data[i+1]<=0x3D:
            b=data[i+1]
            if b<=0x3B: data[i]=0x9B; data[i+1]=0xD8+(b-0x34)
            elif b==0x3C:
                x=data[i+2]; seg={0x18:0x26,0x98:0x2E,0x58:0x36,0xD8:0x3E}.get(x&0xF8)
                if seg: data[i]=0x9B; data[i+1]=seg; data[i+2]=0xD8|(x&7)
            else: data[i]=0x90; data[i+1]=0x9B
            i+=2
        else: i+=1
def strat(dseg,o):
    if o>=len(dseg): return None
    e=dseg.find(b'\0',o)
    if e-o<3 or e-o>120: return None
    s=dseg[o:e]
    if all(32<=c<127 or c in(9,10,13) for c in s) and (o==0 or dseg[o-1]==0): return s.decode()
def disasm(ne,i,dseg=None):
    data=ne.segdata(i); rel=ne.relocs(i)
    fpu_unemulate(data)
    md=Cs(CS_ARCH_X86,CS_MODE_16); md.skipdata=True
    out=[]; buf=bytes(data); pos=0; recent=[]
    names={o:n for (sg,o),n in ne.names.items() if sg==i}
    while pos<len(buf):
        skipto=None
        for ins in md.disasm(buf[pos:],pos):
            a=ins.address
            if a in names: out.append(f'\n;==== {names[a]} ====')
            recent=(recent+[ins])[-6:]
            mm=re.match(r'word ptr cs:\[bx \+ (0x[0-9a-f]+)\]',ins.op_str)
            if ins.mnemonic=='jmp' and mm:
                T=int(mm.group(1),16); n=None
                for r in reversed(recent):
                    c=re.match(r'(?:ax|bx), (0x[0-9a-f]+|\d+)$',r.op_str)
                    if r.mnemonic=='cmp' and c: n=int(c.group(1),0)+1; break
                if n and T==a+ins.size and T+2*n<=len(buf):
                    out.append(f'{a:04x}: jmp word ptr cs:[bx + {T:#x}]   ; switch table, {n} entries')
                    for k in range(n):
                        out.append(f'{T+2*k:04x}:   dw {struct.unpack_from("<H",buf,T+2*k)[0]:#06x}   ; case {k}')
                    skipto=T+2*n; break
            txt=f'{a:04x}: {ins.mnemonic} {ins.op_str}'
            notes=[tname(ne,rel[k][1]) for k in range(a,a+ins.size) if k in rel]
            if ins.mnemonic=='lcall' and notes:
                txt=f'{a:04x}: lcall {notes[0]}'; notes=notes[1:]
            if dseg is not None and not ins.mnemonic.startswith(('j','call','lcall','loop')):
                for m in re.findall(r'0x([0-9a-f]{3,4})\b',ins.op_str):
                    st=strat(dseg,int(m,16))
                    if st: notes.append(repr(st))
            if notes: txt+='   ; '+' '.join(notes)
            out.append(txt)
        pos=skipto if skipto else len(buf)
    return out
if __name__=='__main__':
    ne=NE(sys.argv[1]); outdir=sys.argv[2]; os.makedirs(outdir,exist_ok=True)
    dseg=bytes(ne.segdata(ne.autodata)) if ne.autodata else None
    for i in range(1,ne.segcnt+1):
        if ne.segs[i-1][2]&1: 
            open(f'{outdir}/seg{i:03d}_data.bin','wb').write(ne.segdata(i)); continue
        open(f'{outdir}/seg{i:03d}.asm','w').write('\n'.join(disasm(ne,i,dseg))+'\n')
    print('autodata',ne.autodata,'mods',ne.mods)
