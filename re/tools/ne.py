import struct,sys
d=open(sys.argv[1],'rb').read()
ne=struct.unpack_from('<I',d,0x3c)[0]
h=d[ne:ne+64]
(segcnt,modcnt,nrnsz)=struct.unpack_from('<HHH',h,0x1c)
segtab,rsrctab,resntab,modtab,imptab=struct.unpack_from('<HHHHH',h,0x22)
nrestab=struct.unpack_from('<I',h,0x2c)[0]
align=struct.unpack_from('<H',h,0x32)[0]
print("segments",segcnt,"align",align)
code=data=0
for i in range(segcnt):
    off,ln,fl,mn=struct.unpack_from('<HHHH',d,ne+segtab+i*8)
    ln=ln or 0x10000
    if fl&1: data+=ln
    else: code+=ln
    if i<200 and len(sys.argv)>2: print(i+1,hex(off<<align),ln,"DATA" if fl&1 else "CODE",hex(fl))
print("code bytes",code,"data bytes",data)
mods=[]
for i in range(modcnt):
    o=struct.unpack_from('<H',d,ne+modtab+i*2)[0]
    p=ne+imptab+o; mods.append(d[p+1:p+1+d[p]].decode())
print("imports:",mods)
# names
def names(p):
    out=[]
    while d[p]:
        n=d[p];out.append((d[p+1:p+1+n].decode('latin1'),struct.unpack_from('<H',d,p+1+n)[0]));p+=n+3
    return out
print("resident:",names(ne+resntab)[:40])
print("nonres:",names(nrestab)[:80])
# resources
p=ne+rsrctab; sh=struct.unpack_from('<H',d,p)[0]; p+=2
cnt={}
while True:
    t,n=struct.unpack_from('<HH',d,p); p+=8
    if t==0: break
    cnt[t]=n; p+=12*n
print("resource types:",{ {0x8001:'CURSOR',0x8002:'BITMAP',0x8003:'ICON',0x8004:'MENU',0x8005:'DIALOG',0x8006:'STRING',0x8009:'ACCEL',0x800c:'GRPCURSOR',0x800e:'GRPICON'}.get(k,hex(k)):v for k,v in cnt.items()})
