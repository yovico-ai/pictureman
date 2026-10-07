import struct,sys,os,io
from PIL import Image
src,out=sys.argv[1],sys.argv[2]
d=open(src,'rb').read()
ne=struct.unpack_from('<I',d,0x3c)[0]
rsrctab,resntab=struct.unpack_from('<HH',d,ne+0x24)
base=ne+rsrctab
p=base; sh=struct.unpack_from('<H',d,p)[0]; p+=2
T={0x8001:'cursor',0x8002:'bitmap',0x8003:'icon',0x8004:'menu',0x8005:'dialog',0x8006:'string',0x800a:'rcdata',0x800c:'grpcursor',0x800e:'grpicon'}
def rname(i):
    if i&0x8000: return str(i&0x7fff)
    q=base+i; return d[q+1:q+1+d[q]].decode('latin1')
def cstr(q):
    e=d.index(b'\0',q); return d[q:e].decode('latin1'),e+1
dlgs=open(os.path.join(out,'dialogs.txt'),'w'); strs=open(os.path.join(out,'strings.txt'),'w')
while True:
    t,n=struct.unpack_from('<HH',d,p); p+=8
    if t==0: break
    tn=T.get(t,hex(t))
    for i in range(n):
        off,ln,fl,rid=struct.unpack_from('<HHHH',d,p+i*12)
        blob=d[off<<sh:(off<<sh)+(ln<<sh)]; nm=rname(rid)
        if tn=='bitmap':
            bfh=b'BM'+struct.pack('<IHHI',14+len(blob),0,0,0)
            hs=struct.unpack_from('<I',blob,0)[0]; bc=struct.unpack_from('<H',blob,14)[0]
            nc=struct.unpack_from('<I',blob,32)[0] if hs>=40 else 0
            if bc<=8 and nc==0: nc=1<<bc
            bfh=bfh[:10]+struct.pack('<I',14+hs+nc*(4 if hs>=40 else 3))
            try: Image.open(io.BytesIO(bfh+blob)).save(f'{out}/bmp_{nm}.png')
            except Exception as e: print('bmp',nm,e)
        elif tn in('icon','cursor'):
            q=4 if tn=='cursor' else 0
            b=blob[q:]; hs,w,h=struct.unpack_from('<Iii',b,0); bc=struct.unpack_from('<H',b,14)[0]
            nc=(1<<bc) if bc<=8 else 0
            h//=2; stride=((w*bc+31)//32)*4; mstride=((w+31)//32)*4
            pal=[b[hs+4*k:hs+4*k+3][::-1] for k in range(nc)]
            xo=hs+4*nc; ao=xo+stride*h
            im=Image.new('RGBA',(w,h))
            for y in range(h):
                for x in range(w):
                    row=xo+(h-1-y)*stride
                    if bc==1: idx=(b[row+x//8]>>(7-x%8))&1
                    elif bc==4: idx=(b[row+x//2]>>(4*(1-x%2)))&15
                    else: idx=b[row+x]
                    a=(b[ao+(h-1-y)*mstride+x//8]>>(7-x%8))&1
                    c=pal[idx]
                    if a and tn=='cursor' and idx: c=b'\x80\x80\x80'  # invert pixel
                    im.putpixel((x,y),(*c,0 if a and not(tn=='cursor' and idx) else 255))
            im.save(f'{out}/{tn}_{nm}.png')
        elif tn=='string':
            q=0;bid=int(nm)
            for k in range(16):
                L=blob[q];s=blob[q+1:q+1+L].decode('latin1');q+=1+L
                if s: strs.write(f'{(bid-1)*16+k}\t{s}\n')
        elif tn=='dialog':
            q=0; st=struct.unpack_from('<I',blob,0)[0]; cnt=blob[4]; x,y,w,h=struct.unpack_from('<hhhh',blob,5); q=13
            def bs(q):
                if blob[q]==0xff: return '#%d'%struct.unpack_from('<H',blob,q+1)[0],q+3
                e=blob.index(b'\0',q); return blob[q:e].decode('latin1'),e+1
            menu,q=bs(q); cls,q=bs(q); cap,q=bs(q)
            font=''
            if st&0x40: pt=struct.unpack_from('<H',blob,q)[0]; f,q=bs(q+2); font=f'{f} {pt}'
            dlgs.write(f'\nDIALOG {nm} "{cap}" {w}x{h} {font}\n')
            for c in range(cnt):
                cx,cy,cw,ch,cid=struct.unpack_from('<hhhhH',blob,q); cs=struct.unpack_from('<I',blob,q+10)[0]; q+=14
                if blob[q]&0x80: kl={0x80:'BUTTON',0x81:'EDIT',0x82:'STATIC',0x83:'LISTBOX',0x84:'SCROLLBAR',0x85:'COMBOBOX'}.get(blob[q],hex(blob[q]));q+=1
                else: kl,q=bs(q)
                tx,q=bs(q); q+=1+blob[q]
                dlgs.write(f'  {kl:10} id={cid:<5} ({cx},{cy},{cw},{ch}) style={cs:#x} "{tx}"\n')
        elif tn=='rcdata':
            open(f'{out}/rcdata_{nm}.bin','wb').write(blob)
    p+=12*n
