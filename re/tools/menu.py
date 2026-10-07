import struct,sys
d=open(sys.argv[1],'rb').read()
ne=struct.unpack_from('<I',d,0x3c)[0]
rsrctab=struct.unpack_from('<H',d,ne+0x24)[0]
p=ne+rsrctab; sh=struct.unpack_from('<H',d,p)[0]; p+=2
while True:
    t,n=struct.unpack_from('<HH',d,p); p+=8
    if t==0: break
    for i in range(n):
        off,ln=struct.unpack_from('<HH',d,p+i*12)
        if t==0x8004:
            q=(off<<sh)+4; depth=0
            stack=[]
            def item(q,depth):
                while True:
                    fl=struct.unpack_from('<H',d,q)[0];q+=2
                    mid=None
                    if not fl&0x10: mid=struct.unpack_from("<H",d,q)[0]; q+=2
                    e=d.index(b'\0',q); s=d[q:e].decode('latin1');q=e+1
                    print("  "*depth+(s or "----")+(f"  [{mid}]" if mid else ""))
                    if fl&0x10: q=item(q,depth+1)
                    if fl&0x80: return q
            item(q,0)
    p+=12*n
